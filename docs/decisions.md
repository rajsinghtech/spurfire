# Spurfire Decision Log (ADR-lite)

Numbered decisions with status: **accepted** | **provisional** | **open**.

## D1 — Peer-hosted authority

**Status:** accepted

Gameplay uses peer-hosted authoritative networking, not distributed simulation. All gameplay
machines are peers on a lobby tailnet; one player is elected match authority and validates
movement, shots, damage, score, and events. No permanent dedicated gameplay server. Peers
keep recent state snapshots for authority migration.

## D2 — Tailnet-per-lobby with shared-tailnet fallback

**Status:** provisional — organization lifecycle verified; production secret custody unresolved

Tailnet-per-lobby remains the preferred isolation model. The organization-scoped
`GET/POST /organizations/-/tailnets` route is now verified, as are child OAuth token exchange and
child-scoped `DELETE /tailnet/{dnsName}`. Earlier 404s came from guessed top-level collection
routes and are retained only as negative route evidence. `TailnetPerLobby` is implemented in the
server with a provider-owned in-memory child-secret vault and fail-closed restart behavior.

**Fallback:** one managed game tailnet with lobby-specific tags + ACLs. `SharedTailnet` remains
implemented, but historical probes received 403 for auth-key, device-list, and ACL operations.
Capability reporting keeps organization-tailnet access independent from these shared scopes.

Child-scoped one-use key issuance is implemented and mock-tested but still requires live
end-to-end verification. This decision remains provisional because that verification is incomplete
and the process-local vault must still be replaced by a dynamic encrypted secret manager with
reconciliation, rotation, audit, and cleanup recovery; see `docs/control-plane-network-view.md`. Production custody intent: **setec** (secrets
service) backing the child-OAuth vault, with mutation-closed startup reconciliation against exact
upstream stable IDs.

## D3 — Rust monorepo for the control plane

**Status:** accepted

The control plane is a single Rust workspace: `spurfire-control` (library: Tailscale
tailnet/key lifecycle) and `spurfire-cli` (`spurfire-ctl` binary). A future backend service
builds on the same library.

## D4 — Godot 4 with Rust GDExtension

**Status:** accepted

The game client uses **Godot 4.7.1** for scenes, input, camera, UI, content iteration, and platform packaging, with gameplay systems implemented in Rust through `gdext` GDExtension classes. The first proof point is the M0 graybox horse-movement slice; RustScale integration begins only after its movement acceptance checks pass.

This closes the former blocking engine question. Historically, Bevy was attractive for native Rust and Unity/Unreal offered more mature content tooling, while Godot had the best expected iteration speed and an idiomatic in-process Rust boundary. Godot was selected to optimize the feel-tuning loop without giving up Rust gameplay code.

Acceptance does not erase platform risk: native libraries must be built and packaged per OS/architecture; mobile and console GDExtension support is not validated; large-world and 16-rider performance remain unproven; and RustScale still has gameplay-UDP, telemetry, relay, and all-platform caveats. See `docs/godot-m0.md` and `docs/rustscale-integration.md`.

## D5 — Ranked result verification

**Status:** open (deferred past alpha; intended mechanism recorded)

Peer-hosted matches need trustable results. Intended mechanism: peer co-sign quorum — the
results DTO already records co-signers as untrusted inputs, so adopting it later is not a
wire break. No ranked play ships before this is designed.

## D6 — Mid-match authority is peer-owned; one migration rule

**Status:** accepted (design; implementation in M6-complete)

The match must survive without the control plane. On authority silence, every surviving
peer recomputes `election_v1` over the match-start measurement matrix restricted to the
survivor set — deterministic and coordination-free, so all peers and the service reach the
same successor independently. `SessionState`'s lowest-connected-ID rule becomes the
degraded fallback inside the same protocol scoring function, replacing today's three
divergent rules (server scored re-election / peer lowest-ID / election_v1). The server's
scored re-election applies only in `READY`; during `IN_MATCH` the service validates
successor heartbeats by recomputing the shared function. Epoch checks remain the guard
against stale authorities.

## D7 — Lag compensation: authority-side rewind, hard-capped at 150ms

**Status:** accepted (design; implementation in M6-complete)

`CombatAuthority` keeps ~250ms of position **and stance** history per target (crouch/roll
hitboxes must rewind too). `ShotCommand` carries the shooter's view tick; the authority
rewinds targets to it, capped at 150ms — beyond the cap, shooters lead. The authority
player's own shots run the same path with ~0 rewind; fairness gate: authority-vs-peer hit%
gap < 5%. Consequence for gameplay milestones: snapshot DTOs carry a stance field from M2
onward, and the Tactical Roll gets no invulnerability frames (displacement + smaller
hitbox only) because i-frames are unadjudicable under peer authority at real RTTs.

## D8 — No accounts or persistent user state through alpha

**Status:** accepted

Identity is ephemeral per lobby (client UUID + display name). Lobby access control is a
creator-shared join code on top of service rate limits. No persistent leaderboards, no
user database; the single-process JSON store remains the only durable state. The public
landing page shows only secret-free aggregates (riders online, lobbies by state,
direct-connection rate, median RTT).

Opaque, expiring, lobby-scoped creator/invitation/participant capabilities are compatible with
this decision: they authorize one ephemeral lobby and do not create an account, profile, or
recovery identity. The current join code and client-asserted player headers are migration-only;
capability authorization is mandatory before any public real lobby.

## D9 — The control plane owns but never joins lobby tailnets

**Status:** accepted

For dedicated lobbies, ownership means create, bind the provider stable ID and tailnet DNS
name/FQDN, issue narrow enrollment credentials, inspect cached provider metadata and participant
reports, reconcile, and delete. `spurfire-server`, `spurfire-control`, and the normal operator CLI
must never run a Tailscale/RustScale node in a lobby tailnet. Joining would add private node state,
peer-facing attack surface, cross-tailnet compromise blast radius, biased observer measurements,
and a false gameplay-participant/witness role without producing player-to-player truth.

The server must not depend on RustScale, `spurfire-net`, a gameplay listener, relay, or observer
runtime. A future short-lived diagnostic observer requires a separate security ADR and may not be
linked into or deployed with the main control plane. See
[control-plane-network-view.md](control-plane-network-view.md).

## D10 — Capability-protected exact-lobby network inspection

**Status:** accepted (safe groundwork; real activation closed)

There is no public lobby directory or anonymous real-lobby lookup. A participant or creator uses
an opaque capability bound to one exact lobby, player where applicable, network generation,
scope, and expiry. An operator authenticates on a private listener, may choose from minimal
summaries, and then inspects exactly one lobby. Unauthorized and absent requests have one 404
shape. Inspection GETs are cache-only and mutation-free.

The selected view may show the complete `tailnet_dns_name`/FQDN to authorized members while the
network is active or cleanup is pending. In the illustrative `example-tailnet.ts.net`, the TLD is `.net`; the useful
value is the complete FQDN, not a supposed `.ts.net` TLD. FQDNs and private addresses are
capability-gated topology metadata. Every lifecycle, enrollment, route, application RTT/loss,
authority, freshness, and cleanup fact carries source and assurance; participant reports remain
untrusted reports and never affect gameplay or cleanup truth.

## D11 — One real lobby and fail-closed activation

**Status:** accepted for alpha safety; hosted real activation not approved

One singleton lease covers every real lobby in either dedicated or shared compatibility mode.
Public real mode, when separately approved, accepts only server-selected `tailnet_per_lobby`.
Ambiguous create, restart, polling, cleanup, identity, or vault state holds the lease and closes
new real mutations. Release requires definitive pre-resource create rejection, exact dedicated
absence plus encrypted-secret erasure, or complete shared-resource cleanup.

Public real activation additionally requires capability migration on every lobby route, an
independent default-off real-mutation kill switch, a dynamic encrypted child-OAuth vault,
mutation-closed startup reconciliation, exact-ID orphan/cleanup operations, application and
gateway abuse controls, private operator identity, live restrictive child-policy qualification,
privacy approval, alerts/runbook exercises, persistent non-secret state, clean Linux/cross-platform checks, and a
separate GitOps review. The hosted public deployment remains `dryRun=true`, `provisioningMode=dry_run`,
`existingSecret=""`, and `persistence.enabled=false` until that review.

## D12 — Session identity is WireGuard channel binding plus ephemeral Ed25519

**Status:** accepted for Alpha session identity

A RustScale-delivered UDP source address is authenticated by WireGuard cryptokey routing and is
used only as channel binding. Tailscale node keys are Curve25519 transport keys and never sign
Spurfire messages. Every client instead generates a fresh native-only Ed25519 key for each lobby
session generation, proves possession through its participant-capability-bound endpoint
registration, and signs canonical domain-separated envelopes (wire 2.0 in the live M3 lobby;
wire 1.2 is retained for the M2 proof/demo path). The signature binds the
lobby, network and session generations, complete signed roster hash, sender, authority epoch,
sequence, simulation tick, and fixed-layout payload.

The server signs the complete endpoint/key projection with a per-lobby memory-only key. Peers
reject duplicate IP or claimed node keys, unknown senders, source endpoint mismatches, generation
or roster mismatches, non-strict signatures, and replay before state mutation. Node-key rotation
requires increasing-sequence re-registration; the application identity remains the session key
plus tailnet IP. A server restart cannot silently replace its manifest key inside an old replay
domain: active sessions bump generation and re-key/re-register. Unsigned compatibility is limited
to explicit dry-run/demo/test mode. This does not verify a peer's own gameplay truth and does not
resolve D5 ranked verification. Canonical formats, validation ordering, and rotation rules are
specified in `docs/session-identity-architecture.md`.

## Settled design questions (formerly open, 2026-07-17)

1. **Visual setting** — stylized arcade West: Kenney low-poly CC0 pipeline, saturated
   desert palette, fictional SF-series weapons (no real-world brands).
2. **Win condition** — Bounty Run score race: 15 min, highest bounty wins, respawns on.
3. **Lobby size** — 6–16, default 8, validated at 6/12/16.
4. **Loadout** — starting rifle pick at spawn/respawn; no ground scavenging; pickups only
   as dynamic-objective rewards.
5. **Horse persistence** — per-match archetype pick; cosmetic-only persistence later;
   stats never persist.
6. **Audience** — friends-first party game through alpha; public/ranked deferred (D5).
7. **Production secret custody** — intent: setec-backed vault with startup reconciliation
   replacing the in-memory prototype vault (see D2). Implementation details remain open.
