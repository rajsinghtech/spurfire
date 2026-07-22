# Peer gameplay networking

Spurfire's native gameplay data plane uses application UDP through embedded RustScale. The Alpha
validation branch pins both direct dependencies to RustScale master revision
`4d12d5f3f576577025044f460545f4e816ec32c2`, merged through
[#105](https://github.com/rajsinghtech/rustscale/pull/105). Earlier v0.1.5 revision
`7139bf384045a7e398320ae853e751c61c8218b9` reproduced the refresh-time stall tracked in
[#104](https://github.com/rajsinghtech/rustscale/issues/104). The first one-region PR #105 candidate
reduced a six-minute exact consumer run to 103–208 ms but still failed one follower. An isolated
revision passed a short run at 145 ms, then failed its full second cycle at 206–325 ms. RustScale
[#106](https://github.com/rajsinghtech/rustscale/issues/106) showed that diagnostic STUN addresses
belong to temporary sockets; the merged revision publishes only changed Magicsock-owned endpoints.
The tree-identical PR revision passed both live gates at 110 ms and 139 ms peaks. The exact rebased
master revision then passed the full 900,001 ms run at a 97 ms peak with 54,000 minimum inputs per
sender, 39,999 mm minimum motion, and 0 ms presentation desync. The temporary
v0.1.4-compatible backport [#103](https://github.com/rajsinghtech/rustscale/pull/103) was useful for
isolating and live-qualifying that fix, but is no longer the consumer pin. A Windows exit-139 failure
initially attributed to post-v0.1.4 RustScale later reproduced on the exact backport, moved between the
integrated course and process teardown, and completed all standalone `PeerSession` work without
starting a RustScale connection. It is tracked as a Spurfire/Godot lifecycle defect in
[Spurfire issue #14](https://github.com/rajsinghtech/spurfire/issues/14), not as a reason to remain
on the backport.

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
5. Mints 27 non-reusable, ephemeral, preauthorized 15-minute keys for the full suite.
6. Enrolls two independent embedded RustScale servers with normal path selection.
7. Exchanges bounded signed Spurfire traffic in both directions, reports the route class, and measures the median of nine application-path RTT probes; direct median RTT at or above 80 ms fails.
8. Enrolls two additional peers with RustScale's test-only direct-path disable switch, repeats the signed RTT exchange, and requires both directions to report `Derp`.
9. Enrolls 16 peers, waits for all 240 directed netmap relationships, and proves a pairwise signed wire-2 full mesh.
10. Accepts four signed leaves, closes those nodes, re-enrolls them with fresh keys and endpoints under signed roster revision 2, and proves a second 240-direction mesh plus replacement gameplay input.
11. Starts three additional peers as separate OS processes with a signed exact-endpoint wire-2 roster, establishes the mesh, and forcibly kills authority process A without a Leave packet.
12. Verifies surviving processes B and C elect B after the two-second silence boundary, installs B's fragmented complete M3–M5 checkpoint on C, proves exact score/clock/objective continuity plus continued rider input, and requires kill-to-continuation below three seconds.
13. Closes survivors and exactly deletes the child tailnet under an exit trap.

`just scale-live` runs steps 1–5, 9–10, and 13 with only the 20 scale/churn keys.

On 2026-07-21, one complete Linux ARM64 run used direct paths with a 3 ms application median,
forced DERP in both directions with a 23 ms median, proved 16 nodes and 240 initial signed
directions, accepted 48 signed leaves, admitted four fresh endpoint replacements, proved 240
revised signed directions plus replacement gameplay input, printed complete-state continuity with
`failover_ms=2044`, and ended with `SPURFIRE_P2P_LIFECYCLE_OK`. All 240 scale routes were Direct.
A separate organization-tailnet listing confirmed exact absence of that child and every disposable
child created during the correction runs. This is live protocol/lifecycle development evidence,
not the private packaged-client artifact required by the terminal release gate. Earlier runs exposed
RustScale's retryable macOS port-mapper shutdown uncertainty, which the smoke retries and treats as
a local teardown warning only after traffic succeeds.

`just p2p-game-live` separately launches eight real headless Godot processes on a disposable child
tailnet. It fails unless all 56 directed paths report telemetry, all seven follower-input paths and
seven authority-snapshot paths deliver, and each gameplay-HUD route/RTT row exactly matches the
independent route and rolling nine-sample RTT median emitted by that client. This is an insecure practice-wire integration
qualification and must not be represented as the secure game-client lobby lifecycle acceptance test.
On 2026-07-21, the eight-process Linux ARM64 qualification passed with all 56 routes Direct, a 20 ms
median, 56 exact HUD matches, all seven authority input senders, and all seven snapshot receivers.
An independent organization listing then confirmed exact absence of the disposable child tailnet.
`just p2p-game-soak-live` holds the same eight-process matrix for 15 minutes while sending a
changing authoritative qualification transform. It fails if any follower sees a snapshot gap over
200 ms, receives fewer than 16 snapshots per second, or observes less than 30 m of motion, or if
the authority does not continuously receive all seven input streams. Followers also compare the
interpolated rider with the known qualification trajectory at least 30 times per second and fail
above 200 ms of equivalent planar presentation error. Shortened runs are development
checks only; this remains practice-wire replication evidence rather than secure lifecycle proof.

The 2026-07-21 15-minute qualification did **not** pass the 200 ms gate. All seven followers
received the exact expected 18,000 snapshots and stayed within 11 ms of the interpolated
qualification trajectory, but isolated snapshot gaps reached 225–334 ms. Long-gap telemetry then
correlated the spikes with RustScale's fixed-phase five-minute endpoint/netcheck refresh. Bounding
each embedded Tokio runtime to two workers reduced a six-minute refresh-boundary run to 131–202 ms,
but one follower still failed closed at 202 ms. Each attempt's cleanup trap reported deletion of its
disposable child. A later organization listing still contained the older record
`TrsgR9zy7s11CNTRL` (`spurfire-godot-1784661024`), while the operational tailnet delete endpoint
returned `404 tailnet not found` for that exact ID. Treat that as an inert provider record requiring
separate remediation, not as the live tailnet from the final run. The refresh defect is tracked in
[RustScale issue #100](https://github.com/rajsinghtech/rustscale/issues/100); this was an open M6
blocker until the reviewed fix passed the default run below.

Merged RustScale PR #101 randomizes the periodic refresh per peer and limits that maintenance pass
to the STUN endpoint work needed by magicsock. Its original exact candidate revision
`eea0e4cd40d60a7c143ad7671439d66d2912df08` passed shortened and default eight-Godot soaks. The
isolated v0.1.4 backport revision `ad92ab56474ac37adff5c48da1ae8eaaa50efb43` then passed
RustScale's `rustscale-netcheck` and `rustscale-tsnet` gates, Spurfire's complete hosted CI, and one
complete Client Preflight matrix including Windows startup/export/packaged launch, Linux
x64/ARM64, macOS universal, and candidate assembly. A later exact-backport Windows run failed once
with exit 139 and passed on retry; the same intermittent failure appeared at more than one
RustScale revision and is tracked in Spurfire issue #14. The backport's shortened 360,000 ms live
run peaked at 131 ms snapshot gap and 1 ms presentation desync.

The subsequent default 900,000 ms run on that exact backport also passed. All eight Godot clients
and 56 directed Direct routes remained live through three independently randomized maintenance
cycles; the authority received at least 53,999 inputs from each follower, peak snapshot gap was
131 ms, maximum last-snapshot age was 28 ms, minimum motion span was 39,999 mm, and peak
presentation desync was 1 ms across at least 130,435 presentation samples per follower. The cleanup
trap deleted the exact final child (`tailce2727.ts.net`); builder credentials and run files were
absent afterward, and that child was absent from the organization listing. The unrelated older inert
record described above remains, so the broad leaked-state gate stays open. This closes the M6
practice-wire transport/presentation soak, not secure packaged-client lifecycle, horse physics, or
human-play qualification.

## Security boundaries

Canonical formats, key custody, validation ordering, and rotation rules for signed sessions
are specified in `docs/session-identity-architecture.md` (decision D12).

- Never pass organization or child OAuth credentials to Godot.
- Never print auth keys. The live script writes them only to its private temporary directory. It deletes that directory after proven provider cleanup; if cleanup cannot be proven, it fails and retains the permission-protected recovery directory so the child credential is not destroyed before remediation.
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
  Credentialed qualification still must exercise forced-DERP gameplay under packet loss, packaged
  8–16-player sessions, route transitions, and roaming. The two-peer probe proves explicit DERP
  selection, while the live 16-peer transport probe proves signed leave/re-enroll churn and revised
  endpoint admission without claiming packaged-client play.
- Authority rider inputs drive distinct actor and horse presentation state, and followers replay
  their bounded unacknowledged input history after each reconciliation snapshot. The credential-free
  scale gate covers the deterministic proxy at 6/8/12/16 peers; packaged-client handling remains a
  live qualification item.
- RustScale currently may report `portmapper cleanup remains uncertain` repeatedly on macOS close even though process exit releases local resources. Track this upstream.
- RustScale's fixed-phase five-minute endpoint refresh in v0.1.4 can synchronize embedded peers and
  breach the 200 ms gameplay gap gate. The fix in PR
  [#101](https://github.com/rajsinghtech/rustscale/pull/101), first isolated as v0.1.4 backport PR
  [#103](https://github.com/rajsinghtech/rustscale/pull/103) and now consumed from exact merged main,
  passed both the shortened six-minute refresh-boundary regression and the full 15-minute gate at a
  131 ms peak. The independent Windows lifecycle flake remains tracked in Spurfire issue #14;
  secure packaged-client and human gates also remain.
