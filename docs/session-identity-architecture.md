# Spurfire P2P session-identity architecture

Branch `agent/alpha-completion`. The session-identity implementation was validated against released
RustScale v0.1.4 revision `272ee212c7c339c3d028ea474554154bc28ae381`; the current Alpha
validation branch temporarily pins PR #101 candidate revision
`eea0e4cd40d60a7c143ad7671439d66d2912df08` for refresh-soak qualification. Status:
**implemented** (wire-1.2 signed-session
foundation, extended by the live M3 wire-2.0 payload/session path) and validated in a secret-free isolated checkout on the alpha builder
(formatting, warnings-denied Clippy, tests, dependency/secret guards). Real admission
remains force-closed; the control service stays out of gameplay.

---

## 1. Decision (verdict up front)

**Both halves are required, and they do different jobs:**

1. **Tailscale WireGuard node identity = channel binding only.** A Tailscale node key is a
   Curve25519/NaCl-box key (`rustscale/crates/key/src/node.rs`, `NodePrivate::seal_to/open_from`)
   and **cannot produce signatures**. What it *does* give is enforced inside pinned RustScale:
   every decrypted datagram is attributed to the peer `NodePublic` whose tunnel opened it, and
   `peer_map.packet_source_matches` drops any packet whose source IP is not owned by that node
   key (cryptokey routing), then the ACL `filter.check_in` runs. **So a datagram's source
   tailnet IP is WireGuard-authenticated.** That is the channel binding: session key ↔ player ↔
   tailnet IP ↔ node key ↔ WireGuard session.
2. **Ephemeral per-session Ed25519 key = application identity.** Because node keys cannot sign,
   each client generates a fresh Ed25519 keypair per lobby session generation, registers the
   public key through the capability-bound endpoint-registration route with a
   proof-of-possession, and signs every gameplay envelope. An admitted peer cannot forge
   another roster member: forgery requires the victim's session private key, not just tailnet
   membership.

No interactive challenge-response is needed: WireGuard's handshake already authenticates the
channel and liveness, and the registration proof-of-possession prevents key-claim theft. The
roster manifest is server-signed so peers verify it post-start without the control plane
reachable and detect asymmetric views by comparing `roster_hash`.

## 2. Gap this change closed

- `SessionState::accept` checked lobby/epoch/sequence but **auto-inserted unknown senders**;
  only the GDExt layer filtered by roster, and the only endpoint check lived in **GDScript**.
- `session_generation` existed in the server store but was **not exposed in any API DTO and not
  bound into envelopes**.
- `register_session_endpoint` carried no key material and no one-player-per-node check, so an
  admitted/compromised member could claim another rider's endpoint.
- Wire version 1.1 with major-only compatibility admitted mixed rosters into signed lobbies.

## 3. Architecture

### 3.1 Trust chain

```
server manifest signature  vouches  roster: player_id ↔ session_pubkey ↔ tailnet IP:port (+advisory node_key)
WireGuard + RustScale      proves   datagram src IP belongs to that netmap node (cryptokey routing + ACL)
peer's own netmap          proves   src IP ↔ current node key (status()/ipn_status())
Ed25519 signature          proves   envelope content came from holder of session_pubkey
```

The control plane never joins a lobby tailnet (D9), so it cannot observe node keys; node-key
claims are client-asserted and peer-enforced. The server owns metadata and signatures only —
it never touches gameplay UDP.

### 3.2 Keys

- **Lobby manifest key (server):** per-lobby Ed25519 keypair generated on demand; private key
  held in server memory only (never the JSON store, never the vault, redacted `Debug`,
  destroyed with the lobby, regenerated on restart — clients re-pin via the poll that already
  exists). Public key is delivered in `JoinLobbyResponse` and `LobbyResponse`.
- **Session key (client):** per (lobby, session_generation) Ed25519 keypair, generated
  natively in GDExt (`PeerSession.generate_session_key`); the private half lives in
  `Zeroizing`, never leaves native code, and is never logged or persisted. A new keypair is
  generated whenever `session_generation` changes.
- `ed25519-dalek = "2"` is a direct dependency of `spurfire-protocol`, `spurfire-net`,
  `spurfire-gdext`, and `spurfire-server`. Every transitive crate was already pinned in
  `Cargo.lock` via rustscale — **zero new crates**; `check-control-plane-deps.sh` stays green
  (no server → rustscale/spurfire-net/godot edge, no UDP socket in the server).

### 3.3 Canonical bytes (all SHA-256 / Ed25519 inputs; big-endian; length-prefixed strings; player-sorted rows — election.rs pattern)

`roster_hash` (32 B), `SPURFIRE-ROSTER\0v1\0` domain:
```
network_generation u64 || session_generation u64 || roster_revision u64 || count u32
for each row sorted by player_id:
  player_id 16B || session_pubkey 32B || ip 4B(v4)/16B(v6) || port u16 || node_key 32B (zero if unclaimed)
```

Signed-envelope digest (the Ed25519 message; sign the 32-byte SHA-256 digest,
`verify_strict`-style, never the raw blob), `SPURFIRE-ENV\0v1\0` domain:
```
wire_major u16 || wire_minor u16 || lobby_id 16B
network_generation u64 || session_generation u64 || roster_hash 32B
sender 16B || authority_epoch u64 || sequence u64 || simulation_tick u64
payload_len u32 || canonical_payload_bytes
```
`canonical_payload_bytes` is an explicit fixed-layout encoding per `PeerPayload` variant
(NEVER serde_json — key order is not canonical). The signature commits to the exact lobby,
network generation, session generation, complete signed roster hash, sender, authority epoch,
sequence, tick, and payload — the replay domain is (lobby, network generation, session
generation, roster hash, sender key).

Registration proof-of-possession (`key_proof`, self-challenge), `SPURFIRE-KEYREG\0v1\0` domain:
```
lobby_id 16B || player_id 16B || network_generation u64 || roster_revision u64
tailnet_ip bytes || port u16 || session_pubkey 32B
```

Manifest signature input, `SPURFIRE-MANIFEST\0v1\0` domain:
```
manifest_public_key 32B || canonical_roster_bytes
```

### 3.4 Wire/API changes (additive)

- `Envelope` gains optional `session: Option<SessionBinding>` = `{ network_generation,
  session_generation, roster_hash, signature }`. serde JSON ignores unknown fields, so 1.1
  readers decode 1.2 envelopes (they just cannot verify) — mixed rosters are refused at start
  (below). Overhead stays inside the 1200-byte datagram bound (largest ShotCommand/ShotResult
  vectors re-tested with signatures attached).
- `RegisterSessionEndpointRequest` += `session_public_key`, `key_proof`, optional `node_key`.
- `LobbySessionPeer` += `session_public_key`, `node_key`; `LobbySessionProjection` +=
  `session_generation`, `secure`, `roster_hash`, `manifest_signature`, `manifest_public_key`.
- `JoinLobbyResponse`/`LobbyResponse` += `session_generation`, `manifest_public_key`.
- This foundation introduced wire **1.2**. The live M3 lobby now requires compatible wire 2.0
  members and a complete secure projection; unsigned envelopes remain available only after
  explicit local demo/test opt-in on the legacy proof codec.

### 3.5 Receiver validation ordering (single native gate)

`SecureSession::accept_with_source` (GDExt `accept_packet_with_source`) enforces, in order:

1. Size ≤ 1200 B (pre-parse). 2. JSON decode + wire-major compatibility. 3. Payload shape
   validation. 4. Binding present → else `UnsignedInSecureMode`. 5. `sender` ∈ manifest →
   else `UnknownSender` (`SessionState::accept` no longer auto-inserts unknown senders).
6. `src_ip:port` == manifest endpoint for sender → else `EndpointMismatch` (the WireGuard
   channel binding; the GDScript comparison is deleted). 7. Advisory: src IP → node key via
   the peer's own netmap == manifest `node_key` if both claim → else `NodeKeyMismatch`.
8. `lobby_id` match. 9. `network_generation`/`session_generation` == manifest → else
   `WrongGeneration`. 10. `roster_hash` == local manifest hash → else `RosterMismatch`
   (asymmetric-view detection). 11. `verify_strict(signature, manifest session_pubkey[sender],
   envelope digest)` → else `BadSignature`. 12. `authority_epoch` ≥ current. 13. `sequence`
   strictly increasing per sender. 14. Authority claims (`Authority`/`MigrationSnapshot`)
   are coherent → else `InvalidAuthorityClaim`: the claim must be a self-nomination
   (`authority` == `sender`), and either an exact one-epoch advance accepted only while the
   current authority is silent in the receiver's own view (the local player always counts
   as fresh, so a live authority fails closed against remote usurpation), or a same-epoch
   claim from the installed authority or, after that same silence timeout, one converging
   split elections toward the lowest `PlayerId`. Epoch jumps such as
   `u64::MAX` and third-party installs are inert and mutate nothing.

**Every identity, source, and authority-claim check runs before replay, epoch, or authority
state can mutate.** Counters reset only when session generation/roster hash changes, never
on epoch change.

### 3.6 Rotation, one-player-per-node, fail-closed restart

- **Session-key rotation:** new keypair per `session_generation`; re-registration uses the
  increasing wall-clock-millisecond `sequence` (client restarts/re-keys stay ahead of the
  server's cached value, and the server evicts registrations past the 60 s projection
  retention so a stale cache can never permanently fence out a restarted client) plus the
  30 s renew/re-register paths. Old-generation signatures fail step 9.
- **Node-key rotation:** the primary binding (session key ↔ IP) is unaffected; the WireGuard
  layer drops stale-key ciphertext (authorization generations); a changed node key must be
  re-registered with an increasing sequence — peers never accept a silently different claim.
- **Tailnet IP change:** endpoint re-registration (existing mechanism) + a new key proof.
- **One-player-per-node (alpha policy):** the server rejects registration when another roster
  player already claims the same tailnet address (or the same node key when claimed);
  peers independently reject manifests containing duplicate IPs or node keys (defense in
  depth). This limits Sybil per node, not per human.
- **Fail-closed restart/rekey:** manifest keys are memory-only, so a server restart cannot
  silently reuse or replace a key inside an old replay domain — startup reconciliation bumps
  `session_generation` on every active signed session, the projection empties, and clients
  must re-key/re-register against the freshly generated key they observe via poll. Clients pin
  the manifest key and refuse a changed key unless it arrives with a strictly newer session
  generation. Starting a lobby clears pre-start endpoint registrations so they can never be
  projected into the new signed replay domain.

## 4. Landed files / APIs / tests

- **`crates/spurfire-protocol`** — new `src/session.rs`: `SessionPublicKey`/
  `SessionSignature`/`NodeKey`/`RosterHash` newtypes (redacted `Debug`, b64url/hex serde),
  `SessionBinding`, `RosterManifest(Entry)` with duplicate-identity validation,
  `canonical_roster_bytes`, `roster_hash`, `canonical_envelope_digest`,
  `canonical_keyreg_digest`, `canonical_manifest_digest`, domain prefixes,
  `SessionIdentityError`; `api.rs` DTO additions plus `validate_secure_start_roster`;
  `version.rs` → 1.2. The crate stays pure-Rust/no-network.
- **`crates/spurfire-server`** — `register_session_endpoint` verifies `key_proof`, stores
  key/node claims, rejects duplicate IP/node claims (`session_identity_required`,
  `session_key_proof_invalid`, `node_already_claimed`); `session_projection` emits the signed
  complete roster + `roster_hash` + manifest signature and a `secure` flag that requires every
  roster member to hold a fresh key registration; per-lobby memory-only manifest key in
  `AppState` (redacted, removed on destroy); restart reconciliation bumps active session
  generations; real (non-dry-run) start requires the active compatible signed wire from every member plus a secure
  projection; start clears the endpoint cache. **Real admission stays disabled**: the
  readiness gate in `docs/p2p-networking.md` remains force-closed on the native secret-handoff
  and coherent-authority items, and this change opens no real path.
- **`crates/spurfire-net`** — `Envelope.session`; `SecureSession` (verified manifest +
  `SessionState`) with `accept_with_source` and the extended `AcceptOutcome`; unknown-sender
  auto-insert removed; `canonical_payload_bytes`; `rustscale.node_key_for(ip)` mapping the
  WireGuard-authenticated source IP to the current netmap node key.
- **`crates/spurfire-gdext/peer_session.rs`** — `Zeroizing` session keypair,
  `generate_session_key`, `session_public_key`, `key_proof`, `bind_manifest_key` (refuses
  silent key replacement), `configure_secure_session` (verifies the server signature, pins the
  manifest, checks the local key is in the roster), `accept_packet_with_source`; `make_packet`
  signs in secure mode; legacy `accept_packet` requires explicit `set_insecure_demo_mode`.
- **`game/`** — the shell generates/re-binds keys on join and on every observed
  session-generation or manifest-key change and registers with proof; the bridge routes every
  packet through the native source-checked gate before touching routing state; demo scenes and
  contract tests opt into insecure mode explicitly.
- **Tests** — protocol: canonical-roster sorting/golden vectors, manifest-signature strict
  verify and tamper failure, bounded/redacted encodings. net: secure-gate rejection ordering
  across unsigned/endpoint/generation/roster/tamper/forgery/node-key classes, unknown-sender
  regression, signed 1200-byte datagram bound over every payload vector. server router:
  invalid proof, duplicate claims, replayed sequence, manifest signature verifies against the
  projected roster, restart bumps generation and re-keys with an empty projection. process:
  `just p2p-proof` runs separate loopback peers, rejects tampered signed traffic before state
  mutation, kills authority A, proves B/C agreement on B at epoch 2, rejects a signed
  non-authority snapshot, and accepts signed authority and gameplay traffic after migration.
  live: `p2p_smoke.rs` runs signed traffic inside `just p2p-live`; the RustScale
  `migration_smoke.rs` path uses signed exact-endpoint wire 2, forcibly kills A, installs B's
  fragmented complete M3–M5 checkpoint on C, and requires exact score/clock/objective continuity
  plus continued input in under three seconds.
- **Docs** — D12 (`docs/decisions.md`), security-boundaries rewrite in
  `docs/p2p-networking.md`, `docs/prototype-plan.md` join-flow wording.

## 5. Migration compatibility

HTTP is additive/optional (old clients get `session_identity_required` on real lobbies;
dry-run/dev tolerate keyless registration). Wire 1.2 remains internally major-compatible for
the legacy proof path, while live M3 admission requires major 2. Security comes from the server
start guard and from secure-mode receivers rejecting unsigned envelopes. The wire
version is hashed into the canonical election input, so the election golden hash moved with
the bump (expected). `check-control-plane-deps.sh` is unchanged.

## 6. Threat-model limits

Defeats: admitted/compromised peer forging another roster member; replay across
lobbies/generations/sessions; asymmetric roster views (hash in every signature); endpoint or
key-claim theft (capability-bound player + key proof); post-start manifest tampering by peers
(server signature). Does NOT defeat: a peer lying about its *own* gameplay truth (Byzantine
inputs/snapshots/authority claims — ranked trust stays open under D5); control-plane
compromise (mints tailnets, keys, manifest keys — total by design); Tailscale control
compromise (netmap lies); DoS/dropping; traffic analysis; multiple players operated by one
human on distinct nodes. Always `verify_strict`. Private keys are zeroized, native-only, and
never logged or persisted. Signature cost is negligible (≤16 peers × 60 Hz; ~70 µs/verify).

---

## Implementation and validation status

- Landed on `agent/alpha-completion` in two commits: the recovered signed-session
  implementation, then this document plus decision-record ordering.
- Validation gate (secret-free isolated checkout on `ubuntu@raj-builder`; no Mac
  compile/export, no credentials, no provider/deployment/release mutations, no
  primary-worktree changes): `cargo fmt --check`, `cargo clippy --locked --all-targets --
  -D warnings` (including the `rustscale`-feature smoke bins), `cargo test --locked`,
  `bash scripts/check-control-plane-deps.sh`, and a tree-wide secret scan. Results are
  recorded with the landing commits.
- Real admission remains force-closed. `just p2p-proof` runs the credential-free signed
  two-process exchange, the retained wire-1 migration scenario, and a wire-2 three-process
  authority kill with exact M5 score/clock/objective continuity. Real RustScale route evidence and
  the `migration_smoke.rs` signed-traffic extension require credentialed tailnets and remain
  separate follow-ups.
