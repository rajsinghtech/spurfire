# Peer gameplay networking

Spurfire's native gameplay data plane uses application UDP through embedded RustScale. Both direct RustScale dependencies are pinned to released v0.1.4 revision `272ee212c7c339c3d028ea474554154bc28ae381`; do not update the pin without rerunning the dependency gates and live lifecycle probe.

## Components

- `crates/spurfire-net` defines transport-independent, 1,200-byte-bounded envelopes.
- Envelopes carry heartbeats, rider input/snapshots, shot commands/results, authority announcements, and migration snapshots.
- `SessionState` rejects replayed/out-of-order sequences, wrong-lobby traffic, and stale authority epochs. Live wire 2.0 installs the exact match-start `election_v1` order after independently recomputing the server projection. After two seconds of authority silence it selects the best surviving candidate from that order, using the scored degraded order and only then lowest-ID fallback. Remote claims remain self-nominations for exactly the next epoch; third-party installs and epoch jumps are rejected before state mutation.
- `spurfire_net::rustscale::RustScalePeer` enrolls an ephemeral node with a one-use auth key, clears the key after `up()`, binds `Server::listen_packet`, and sends/receives through `UdpListener`.
- `SnapshotBuffer` interpolates authoritative states two ticks behind, follows the shortest yaw arc, caps velocity extrapolation at fifteen ticks (250 ms at 60 Hz), and classifies prediction corrections as smoothable or snap-sized.
- Godot's native `PeerSession` node owns RustScale on a background Tokio runtime and moves packets to the main thread through signals. OAuth credentials remain control-plane-only; this class accepts only a narrow join auth key.
- Godot's `NetworkRider` presents up to 600 buffered remote snapshots (at least ten seconds at 60 Hz) and exposes reconciliation results. The lobby bridge sends wire-2 actor state at 20 Hz, sends stance changes immediately, and feeds accepted authority state into remote riders. Full actor bases arrive at 2 Hz; intermediate packets contain signed field-level deltas. A missing or mismatched base fails closed until the next full packet. Followers retain a bounded one-second input window, replay unacknowledged ticks from accepted authority poses, and then apply smooth-or-snap position/yaw correction.

The C ABI still has no gameplay UDP API. Spurfire does not use it: the Rust GDExtension links the native Rust `tsnet` API directly.

RustScale v0.1.4 owns the netstack poll-loop notification in `UdpListener`, wakes it after a successful application-UDP enqueue, and includes idle-delivery and 20 Hz anti-batching regression tests. Spurfire therefore carries no local netstack override. A managed 1,600-snapshot-per-peer soak measured steady maximum packet gaps around 41–54 ms; control-map reconnects caused brief 125–287 ms gaps but no rejection, disconnect, accumulated delay, or traffic loss.

## Live proof

Run:

```bash
just p2p-live
```

The script requires the gitignored `.env` OAuth settings. It:

1. Mints an organization token.
2. Creates an API-only child tailnet.
3. Immediately stores its one-time child credentials in a mode-0600 temporary file.
4. Installs a disposable `tag:spurfire` allow policy.
5. Mints five non-reusable, ephemeral, preauthorized 15-minute keys.
6. Enrolls two independent embedded RustScale servers.
7. Exchanges bounded signed Spurfire traffic in both directions, reports the route class, and measures the median of nine application-path RTT probes; direct median RTT at or above 80 ms fails.
8. Starts three additional peers as separate OS processes with a signed exact-endpoint wire-2 roster, establishes the mesh, and forcibly kills authority process A without a Leave packet.
9. Verifies surviving processes B and C elect B after the two-second silence boundary, installs B's fragmented complete M3–M5 checkpoint on C, proves exact score/clock/objective continuity plus continued rider input, and requires kill-to-continuation below three seconds.
10. Closes survivors and exactly deletes the child tailnet under an exit trap.

On 2026-07-21, the complete Linux ARM64 probe used direct paths with a 3 ms application median,
printed signed complete-state continuity with `failover_ms=2044`, and ended with
`SPURFIRE_P2P_LIFECYCLE_OK`. A separate
organization-tailnet listing confirmed exact absence of that child and every disposable child
created during the correction runs. This is live protocol/lifecycle development evidence, not the
private packaged-client artifact required by the terminal release gate. Earlier runs exposed
RustScale's retryable macOS port-mapper shutdown uncertainty, which the smoke retries and treats as
a local teardown warning only after traffic succeeds.

## Security boundaries

Canonical formats, key custody, validation ordering, and rotation rules for signed sessions
are specified in `docs/session-identity-architecture.md` (decision D12).

- Never pass organization or child OAuth credentials to Godot.
- Never print auth keys. The live script writes them only to its private temporary directory and deletes that directory on every exit path.
- Datagram size is checked before JSON parsing and before send.
- Live wire 2.0 binds each M3 gameplay datagram to the exact lobby, network generation, session generation, signed complete-roster hash, sender, authority epoch, sequence, tick, and canonical fixed-layout payload with a native-only ephemeral Ed25519 key. RustScale's cryptokey routing authenticates the tailnet source IP; the native receive gate checks that IP/port against the signed manifest before signature, replay, epoch, or authority state can mutate. A current netmap node-key claim is an advisory WireGuard cross-check, never an application signing key. The wire-1.2 codec remains for bounded M2 proof/demo coverage, not live lobby admission.
- The capability-bound endpoint route verifies key possession, rejects duplicate IP/node claims, and returns a server-signed complete projection. Server restart cannot reuse its memory-only manifest key silently: active sessions bump generation and must re-key/re-register. Unknown senders never auto-insert. Unsigned packets remain available only after explicit local demo/test opt-in.
- This defeats cross-player forgery and cross-lobby/generation/session replay. It does not make a peer's own gameplay claims truthful, defeat control-plane/Tailscale compromise, prevent dropping/DoS, or solve ranked verification. Real product readiness remains forced closed on the coherent authority and native secret-handoff gates.

## Remaining M6 and production work

- D6 migration, the real M3–M5 checkpoint, D7's 150 ms admission cap, the ten-second
  presentation ring, and native zeroizing HTTPS create/join handoff are implemented and covered by
  credential-free tests. The successor restores the complete latest signed checkpoint and
  announces its hash.
- The 20 Hz actor stream now uses temporal field-level compression against 2 Hz full bases;
  credential-free tests prove smaller delta packets, missing-base rejection, reconstruction, and
  recovery on the next base.
- The credential-free three-process proof now kills the original authority, installs the fragmented
  complete wire-2 M3–M5 checkpoint, and verifies exact score, clock, and objective continuity.
  Credentialed qualification must still exercise 8–16 peers, forced DERP, route transitions,
  roaming, packet loss, and churn.
- Authority rider inputs drive distinct actor and horse presentation state, and followers replay
  their bounded unacknowledged input history after each reconciliation snapshot. The credential-free
  scale gate covers the deterministic proxy at 6/8/12/16 peers; packaged-client handling remains a
  live qualification item.
- RustScale currently may report `portmapper cleanup remains uncertain` repeatedly on macOS close even though process exit releases local resources. Track this upstream.
