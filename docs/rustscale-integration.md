# RustScale integration survey

**Survey snapshot:** sibling `rustscale` commit `81cfea8c9babd867c0f0273b905c885706f4365c`
(crate version `0.1.3`, 87 workspace packages), inspected without building on 2026-07-16.
The verdict applies to that commit: RustScale is moving quickly, so every cited API and line should
be rechecked when the dependency revision changes.

**Current consumer validation (2026-07-21):** The Alpha validation branch pins RustScale master
revision `4d12d5f3f576577025044f460545f4e816ec32c2`, merged through PR #105. Earlier v0.1.5 revision
`7139bf384045a7e398320ae853e751c61c8218b9` retained PR #101's randomized,
STUN-only refresh schedule but still launched a whole-DERP-map probe burst; two exact 15-minute
consumer runs reproduced 239–462 ms gameplay gaps and are tracked in RustScale #104. PR #105 first
limited periodic publication to one home-region probe; a six-minute exact consumer run improved the
cluster to 103–208 ms but still failed one follower. An isolated-runtime revision passed its short
run at 145 ms, then failed the full run's second cycle at 206–325 ms. RustScale #106 showed that the
generic netcheck address belongs to a temporary socket, so the current candidate publishes only
changed Magicsock-owned endpoints. The tree-identical PR revision passed both live gates at 110 ms
and 139 ms peaks; the exact rebased master revision still requires final consumer requalification.
The temporary v0.1.4-compatible PR #103 backport passed RustScale's
netcheck/tsnet gates, Spurfire's hosted all-platform consumer matrix, and the full 15-minute live
eight-Godot soak before the consumer returned to main. A Windows exit-139 failure originally filed
against RustScale later reproduced at the exact backport and completed every standalone
`PeerSession` operation without starting a RustScale connection. That intermittent course/teardown
failure is tracked in Spurfire issue #14 and no longer blocks main consumption on a false dependency
boundary. This does not supersede the older API survey below.

## Executive verdict

Rust callers can join a tailnet with a short-lived auth key **today**, use an in-process userspace
TCP/UDP stack, and obtain an accurate live route class (`Direct`, `Relay`, or `Derp`). That is enough
for a Rust-only prototype. It is **not yet a production-ready, all-platform game integration**:
the C ABI has no UDP datagram API, per-peer RTT is an on-demand ping rather than part of status (and
is not exported by the C ABI), peer-relay status is incompletely projected, and a periodic Hostinfo
refresh appears able to de-advertise peer-relay capability. Treat lobby-tailnet enrollment as ready
for an integration spike, not as a locked shipping subsystem.

## Survey method and marker counts

The table is a lexical `rg`-equivalent count over every `*.rs` file recursively under each crate
(including unit/integration tests and examples); `bindings` is the one Python file. Thus most
`panic!` occurrences are test assertions, while `TODO` includes comments. Counts are in the order
**`todo!()` / `unimplemented!()` / `FIXME` / `TODO` / `panic!()`**. There are **zero** `todo!()`,
`unimplemented!()`, and `FIXME` hits in every surveyed component. Test counts below count
`#[test]` plus `#[tokio::test]`, again lexically.

| Component | Rust files | Markers (`todo / unimpl / FIXME / TODO / panic`) | Tests |
|---|---:|---:|---:|
| `tsnet` | 49 | `0 / 0 / 0 / 9 / 39` | 620 |
| `magicsock` | 15 | `0 / 0 / 0 / 0 / 8` | 177 |
| `derp` | 5 | `0 / 0 / 0 / 0 / 12` | 25 |
| `controlclient` | 10 | `0 / 0 / 0 / 0 / 2` | 56 |
| `ipn` | 10 | `0 / 0 / 0 / 0 / 0` | 98 |
| `ipnstate` | 1 | `0 / 0 / 0 / 0 / 0` | 12 |
| `tailcfg` | 16 | `0 / 0 / 0 / 0 / 0` | 98 |
| `key` | 6 | `0 / 0 / 0 / 0 / 0` | 30 |
| `disco` | 3 | `0 / 0 / 0 / 0 / 0` | 25 |
| `netcheck` | 9 | `0 / 0 / 0 / 0 / 2` | 56 |
| `ffi` | 3 | `0 / 0 / 0 / 0 / 3` | 42 |
| `bindings` | 0 (1 Python) | `0 / 0 / 0 / 0 / 0` | 0 |
| `appc` | 6 | `0 / 0 / 0 / 0 / 0` | 53 |

Marker absence is not completion: several limitations are expressed as prose (`not yet`,
`simplified`, `stub`) rather than macros. The large `tsnet` count is evidence of active coverage,
not proof of production maturity; live control-plane tests are ignored unless credentials are
provided (`crates/tsnet/src/tests.rs:2732-2756`). No secret-bearing test or build was run for this
survey.

## Crate map for an embedded client

**`rustscale-tsnet` — integration facade; alpha/broad.** This is the crate Spurfire should call,
not a hand-assembled lower layer. `ServerBuilder` configures hostname, auth key, control URL,
ephemerality and state (`crates/tsnet/src/lib.rs:252-267`, `439-495`); `Server::up`, `dial`,
`listen`, and `listen_packet` expose lifecycle and userspace TCP/UDP
(`crates/tsnet/src/api.rs:198`, `313`, `329`). It also exposes status, LocalAPI, WhoIs and a
`magicsock()` escape hatch. Its API is extensive and heavily tested, but still explicitly carries
nine TODO comments and compatibility/stub surfaces, so regard it as alpha.

**`rustscale-magicsock` — path selection/data transport; alpha but substantive.** It owns direct
UDP discovery, DERP fallback and peer-relay selection. Public `Magicsock`, `MagicsockConfig`,
`Endpoint`, `BestPath`, `PathClass`, relay-manager types and `cli_ping` are exported
(`crates/magicsock/src/lib.rs:36-50`, `604-668`, `973`, `2597`, `2811`). The state machine ranks
`Direct > Relay > Derp` (`crates/magicsock/src/endpoint.rs:225-247`, `487-513`). Coverage is broad,
but the crate describes itself as a simplified port (`crates/magicsock/src/lib.rs:3`) and is a
high-churn, correctness-critical dependency rather than a stable game-facing API.

**`rustscale-derp` — DERP protocol/client/server; usable internal layer.** Public API includes
`DerpClient::{connect, recv, send_packet, send_ping}`, protocol `Received`, frame codecs, and a
`DerpServer` (`crates/derp/src/lib.rs:16-28`; `crates/derp/src/client.rs:135-238`, `438-519`).
Spurfire should consume it transitively through tsnet/magicsock. Its 12 panics are test assertions
in the surveyed files, and no incomplete markers were found, but only 25 tests make it less
battle-proven than the Go implementation it mirrors.

**`rustscale-controlclient` — control-plane enrollment/map client; usable internal layer.** It
implements ts2021 Noise plus HTTP/2 register and streaming map operations; public exports include
`ControlClient`, `RegisterError`, map-session types, `fetch_server_pub_key`, and login flags
(`crates/controlclient/src/lib.rs:21-35`). `ControlClient::register` sends
`/machine/register` (`crates/controlclient/src/client.rs:638-687`). Auth-key login is wired through
this crate, but Spurfire should not call it directly because tsnet owns key persistence, map updates,
DERP setup, and cleanup.

**`rustscale-ipn` — client state machine/preferences; fairly mature data/control layer.** Public
`IpnBackend`, `NotifyBus`, `Prefs`, `StartOptions`, `State`, profiles and notification masks model
the daemon state (`crates/ipn/src/lib.rs:28-50`, `168-195`, `219`). It has 98 tests and no surveyed
markers. It matters for lifecycle/status notifications but is normally reached through tsnet or
LocalAPI, not linked directly by game code.

**`rustscale-ipnstate` — serialized status models; small and usable.** Public `Status`,
`PeerStatus`, `PingResult` and `StatusBuilder` mirror Tailscale status JSON
(`crates/ipnstate/src/lib.rs:42`, `124`, `279`, `302`). `PeerStatus` already has `CurAddr`, `Relay`
and `PeerRelay` fields (`crates/ipnstate/src/lib.rs:154-160`), and `PingResult` has latency and relay
metadata (`279-299`). The types are mature enough to consume, but tsnet does not populate all of
them correctly/fully today.

**`rustscale-tailcfg` — control-plane wire schema; broad but intentionally a subset.** It publicly
re-exports DERP maps, nodes, map/register requests, DNS, filters, services and capabilities
(`crates/tailcfg/src/lib.rs:26-63`). Registration structs are explicitly subsets of upstream
(`crates/tailcfg/src/register.rs:11-15`, `69-72`). With 98 tests and no markers it is solid as an
internal wire model, but its PascalCase fields and upstream-coupled schema are not a game API.

**`rustscale-key` — cryptographic key types; focused and usable.** It exports machine, node,
disco, and network-lock key pairs plus parsing/serialization and NaCl-box helpers
(`crates/key/src/lib.rs:20-29`, `48-51`). No incomplete markers were found across six files and 30
tests. Keep it transitive except where `NodePublic` is needed to query magicsock; never persist or
log its private values in game telemetry.

**`rustscale-disco` — NAT-traversal message codec; focused and usable.** It exports disco message
types, address encoding, wrapper detection, authenticated sealing and opening
(`crates/disco/src/lib.rs:25-36`, `41-91`). It has 25 tests and no surveyed markers. It should stay
behind magicsock unless Spurfire is implementing diagnostics.

**`rustscale-netcheck` — STUN/ICMP network characterization; partial but useful.** Public `Prober`
and `Report` measure UDP reachability, NAT mapping and per-DERP-region latency
(`crates/netcheck/src/lib.rs:21-27`; `crates/netcheck/src/report.rs:16-62`). The prober is explicitly
simplified (`crates/netcheck/src/prober.rs:1-14`), and `Report` still says ICMPv4 is “not implemented
yet; always false” (`crates/netcheck/src/report.rs:31-33`) even though a fallback is present in the
prober, an internal consistency warning. Use it for host/DERP selection, not peer RTT.

**`rustscale-ffi` — C ABI over tsnet; prototype-quality for game embedding.** It builds `cdylib`,
`staticlib`, and `rlib` (`crates/ffi/Cargo.toml:8-12`) and exposes opaque handles for configuration,
`ts_up`, TCP listen/dial, status JSON, WhoIs, and cleanup (`crates/ffi/src/lib.rs:203-1040`). Panics
are caught at the ABI boundary (`crates/ffi/src/lib.rs:132-143`). The committed generated header is
`include/rustscale.h`. It is useful for engine integration, but it omits UDP and ping APIs, and its
blocking global-runtime design needs game-thread isolation.

**`bindings` — Python ctypes wrapper; demo maturity, not a platform strategy.** The sole binding
wraps the C ABI as `Server`, `Listener`, and `Connection` (`bindings/python/rustscale.py:1-20`,
`151-287`). There is no UniFFI, JNI, Swift, Kotlin, or UDL source in the tree. Python library lookup
only distinguishes macOS (`.dylib`) from everything else (`.so`) at
`bindings/python/rustscale.py:42-55`, so it is not even a Windows packaging implementation.

**`rustscale-appc` — app-connector routing; not needed by Spurfire's core path.** It dynamically
advertises routes learned from DNS and exports `AppConnector`, `RouteAdvertiser`, connector
selection, and prefix utilities (`crates/appc/src/lib.rs:18-27`). It has 53 tests and no markers,
but lobby clients neither advertise SaaS routes nor need app-connector behavior. Leave it
transitive if tsnet requires it; do not build game logic on it.

## Embedding path: auth key to online node today

### Native Rust (recommended first spike)

The minimal implemented flow is demonstrated by `crates/tsnet/examples/rustscale-serve.rs:90-106`
and the simpler `hello.rs:19-39`:

```rust
let mut node = rustscale_tsnet::Server::builder()
    .hostname(lobby_device_name)
    .auth_key(short_lived_one_use_key)
    .control_url(control_url)
    .ephemeral(true)
    .state_dir(per_lobby_private_state_dir)
    .build()?;
Box::pin(node.up()).await?;
let game_udp = node.listen_packet(":GAME_PORT").await?;
```

`up()` performs control registration, receives the network map, initializes WireGuard/magicsock,
connects DERP and starts netstack. The production daemon uses the same builder path at
`crates/rustscaled/src/daemon.rs:250-301`; CLI `up --auth-key` feeds auth through LocalAPI
(`crates/cli/src/commands/up.rs:17`, `97`). For lobby isolation, use a fresh ephemeral node and
private state directory per lobby, call `close()` on leave, and let the backend mint the one-use
credential. Do not put an admin/OAuth credential in the client.

Auth-key flow is not merely sketched: `ServerBuilder::auth_key` is public, the registration request
contains `RegisterResponseAuth`, and tests cover wrapped one-use keys and credential redaction
(`crates/tsnet/src/tests.rs:1297-1431`; `crates/tailcfg/src/register.rs:110-122`). Nevertheless, the
real control-plane enrollment tests require external credentials and are ignored by default, so CI
must add a secret-safe two-node canary before shipping.

### Non-Rust engines/languages

The C sequence is `ts_new` -> `ts_set_hostname` -> `ts_set_authkey` ->
`ts_set_control_url` -> `ts_set_ephemeral` -> `ts_up`; then inspect `ts_status_json` and finally
`ts_close` (`crates/ffi/src/lib.rs:203-398`, `447-500`). This is a real C ABI with static/shared
library outputs and a generated C header, not UniFFI. It currently exposes TCP streams only:
`ts_listen` and `ts_dial` reject non-TCP protocols (`crates/ffi/src/lib.rs:763-835`, `897-956`),
while the required Rust UDP API exists at `crates/tsnet/src/api.rs:323-356`. Therefore C/C++ game
clients cannot yet send gameplay datagrams through embedded netstack without adding ABI surface or
using TUN mode and OS sockets. No first-party mobile or console language/package bindings were
found.

## Connection introspection for the lobby UI

### Route label: yes in Rust; yes through status JSON

`rustscale_magicsock::PathClass` is exactly the required enum: `None`, `Derp`, `Relay`, `Direct`
(`crates/magicsock/src/endpoint.rs:74-83`). `Magicsock::peer_path_class` evaluates each peer's
current `BestPath` (`crates/magicsock/src/lib.rs:2597-2607`), and `Server::status()` copies it into
`PeerInfo.path_class` (`crates/tsnet/src/api.rs:31-47`; `crates/tsnet/src/status.rs:32-43`). The C ABI
serializes that field as `"path_class"` (`crates/ffi/src/lib.rs:471-482`). Mapping is therefore:
`Direct` -> **Direct**, `Relay` -> **Peer Relay**, `Derp` -> **DERP Relay**, and `None` ->
**Connecting/Unavailable**. Poll status when the lobby panel is visible; routes are explicitly
mutable.

Do not derive the label only from `ipnstate::PeerStatus`: although that model has `Relay` and
`PeerRelay`, `Server::ipn_status()` sets `Relay` only for DERP, leaves `PeerRelay` at default, and
uses the local home DERP region rather than the peer path's selected region
(`crates/tsnet/src/api.rs:150-169`). `ServerStatus.path_class` is the reliable source at this
snapshot.

### RTT: available on demand, not as continuous status

`Magicsock::cli_ping(peer_key, peer_name, peer_ip, size)` concurrently probes direct candidates and
DERP and returns the first pong in `ipnstate::PingResult` with `LatencySeconds`, `Endpoint`,
`DERPRegionID/Code`, and `PeerRelay` (`crates/magicsock/src/lib.rs:2803-2817`, `2940-2953`;
`crates/ipnstate/src/lib.rs:279-299`). A Rust client can obtain `Server::magicsock()` and peer keys
from `Server::status()`, then sample RTT. `netcheck::Report::region_latency` is only client-to-DERP
STUN RTT (`crates/netcheck/src/report.rs:43-48`), not peer RTT; DERP's ping frames are keepalive
primitives, not a per-peer measurement API.

There is no RTT field in `PeerInfo`, no continuous RTT cache, and no C ABI export for `cli_ping`.
For the lobby's median/worst RTT and authority election, add a bounded sampling service (for
example, one ping per peer every 2-5 seconds with jitter, EWMA and timeout state) plus an async C ABI
or engine bridge. Avoid calling the blocking FFI from the render/game loop.

## Ranked gaps and bug-hunt findings

The literal macro hunt found no `todo!()` or `unimplemented!()` in the surveyed crates. The
highest-impact half-wired behavior is instead:

1. **P0 — gameplay UDP is absent from the C ABI.** Rust tsnet has `listen_packet` and
   `UdpListener::{recv_from, send_to}` (`crates/tsnet/src/api.rs:323-356`;
   `crates/netstack/src/lib.rs:166-206`), but FFI exports only TCP listen/dial/read/write
   (`crates/ffi/src/lib.rs:763-1040`). This blocks a typical C/C++ engine from using the low-latency
   userspace path and is the largest all-platform embedding gap.
2. **P0 — lobby telemetry is only half wired.** Route class is available, but peer RTT requires the
   internal-ish `Server::magicsock()` escape hatch and is not in status/FFI
   (`crates/tsnet/src/lib.rs:1441-1460`; `crates/magicsock/src/lib.rs:2811`). `ipn_status()` leaves
   `PeerRelay` empty and reports DERP with the node's local home region
   (`crates/tsnet/src/api.rs:150-169`). The UI can label paths through `ServerStatus`, but cannot yet
   implement the specified latency matrix portably.
3. **P1 — periodic Hostinfo can de-advertise an enabled peer relay.** Periodic endpoint updates
   correctly accept and publish `peer_relay_server` (`crates/tsnet/src/link_monitor.rs:398-458`),
   but the separate Hostinfo refresh hard-codes `peer_relay: false`
   (`crates/tsnet/src/link_monitor.rs:531-604`). If control treats the latest Hostinfo as
   authoritative, a relay-enabled node can disappear as a candidate after refresh.
4. **P1 — roaming republishes stale/optimistic network information.** On a major link change the
   code runs netcheck and uses only `global_v4`; its MapRequest still sends the startup
   `home_derp` and unconditional `WorkingUDP: True` (`crates/tsnet/src/link_monitor.rs:330-385`).
   The periodic path does the same (`398-470`). This can preserve a poor DERP choice or falsely
   claim UDP after switching networks, directly affecting fallback quality.
5. **P1 — no first-party all-platform binding/packaging layer.** The repository has one generated C
   header and one Python ctypes wrapper; no UniFFI/JNI/Swift/Kotlin layer was found. The FFI crate
   produces native static/shared libraries (`crates/ffi/Cargo.toml:8-12`), but mobile lifecycle,
   backgrounding, network-change hooks, Windows DLL packaging, consoles, and ABI compatibility are
   not productized.
6. **P2 — DERP's first packet can be dropped on connection failure.** Lazy DERP send returns false
   and drops the packet when `get_or_connect` fails (`crates/magicsock/src/lib.rs:1495-1517`). Later
   game traffic retries naturally, but handshake/control traffic needs explicit retry and health
   handling; test this under blocked UDP, DERP disconnect, and region migration.
7. **P2 — explicit stubs remain outside the core lobby path.** TSMP and PeerAPI pings return “not yet
   implemented” (`crates/tsnet/src/localapi.rs:2654`, `2793-2819`), netcheck describes itself as
   simplified (`crates/netcheck/src/prober.rs:1-14`), and PMTUD is stubbed on unsupported platforms
   (`crates/magicsock/src/pmtud/stubs.rs:1-20`). These are not blockers for the first Rust desktop
   spike, but they matter for diagnostics and heterogeneous-platform reliability.

### Required integration tests before calling it beta

Run two real ephemeral nodes through the same backend-minted lobby credential and assert: auth-key
consumption/reuse rejection; UDP echo both directions; forced DERP fallback; direct-to-DERP and
DERP-to-direct transitions; peer-relay allocation and route reporting; Wi-Fi-to-cellular roaming;
status and sampled RTT agreement; cleanup/removal after `close`; and 16-peer churn. Quantify packet
loss, route convergence time, median/p95 RTT, and auth-to-online time. Ensure logs and CI artifacts
redact credentials.

Spurfire's 2026-07-21 live development probe now covers explicit forced DERP and signed 16-peer
leave/re-enroll churn with a complete 240-direction remesh. Direct/DERP transitions, peer relay,
roaming, packet-loss qualification, close-time device disappearance, and packaged-client scale
remain required before beta.

## Dependency recommendation

**Use a git dependency pinned to an exact RustScale commit for CI/releases, with an optional local
path override for developers.** A bare path dependency is convenient during the spike but makes a
Spurfire revision non-reproducible and can silently pick up sibling breakage. A floating git branch
has the same problem remotely. Pin the full commit in Cargo/source-lock configuration and update it
through reviewed integration PRs; locally, use Cargo `[patch]` (kept out of committed release
configuration) to point that same package set at `../rustscale` while co-developing.

Do **not** vendor a subset now. `rustscale-tsnet` directly spans a large, tightly coupled set of
workspace crates, and maintaining a hand-selected fork would hide upstream fixes and create a
security-update burden. Vendor a whole audited pinned snapshot only if a target platform/build
system cannot consume Cargo git sources or release provenance requires source escrow. For non-Rust
engines, publish native artifacts and `include/rustscale.h` from the same pinned commit, recording
that commit in the artifact version and lobby compatibility handshake.
