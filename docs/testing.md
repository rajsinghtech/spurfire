# Testing Spurfire

## 1. Prerequisites

- Rust toolchain compatible with the workspace and GDExtension (`rustup update`).
- Godot 4.7.1.
- `just`, `curl`, and `jq`.

On macOS with Homebrew:

```bash
brew install just jq
brew install --cask godot
rustup update
```

From the repository root:

```bash
just setup
```

## 2. Rust correctness gate

```bash
just check
```

This checks formatting, runs Clippy with warnings denied, and executes the complete Rust test suite. CI runs the same recipe on Ubuntu, macOS, and Windows after a locked all-targets `cargo check`. Replication tests cover jittered snapshots, shortest-arc yaw, bounded extrapolation, replay rejection, stale authority epochs, and deterministic migration.

## 3. Godot integration gate

```bash
just game-test
```

This builds and signs the Rust GDExtension, imports assets, then runs:

- Horse/controller, Saddle Dive, and native networking smoke.
- `PeerSession` envelope/snapshot/replay/stance checks.
- `NetworkRider` interpolation and reconciliation checks.
- Polish UI smoke.
- Mounted and dive-combat smoke.
- Main-scene startup.

Expected markers include:

```text
SPURFIRE_GODOT_SMOKE_OK
SPURFIRE_POLISH_SMOKE_OK
SPURFIRE_COMBAT_UI_SMOKE_OK
SPURFIRE_ALPHA_LOBBY_SMOKE_OK
```

The Alpha lobby marker is a credential-free contract smoke for strict origin/join-code handling,
capability route glue, exact-roster binding, endpoint/report calls, and truthful leave/cleanup UI.
It is not a live provider lifecycle, two-download, coherent remote-authority gameplay, or human-feel
qualification.

### Godot 4.7 UID sidecars

Godot 4.7.1 creates a neighboring `.uid` sidecar when a recognized resource such as a GDScript does not store its own UID. The editor reuses that value and moves the sidecar with the resource; `.godot/uid_cache.bin` is machine-generated cache state and remains ignored.

Repository policy:

- Run the pinned Godot 4.7.1 import from the isolated implementation worktree, then commit each genuinely new `file.gd.uid` with `file.gd`.
- Never hand-author a UID, copy one from another script/worktree, or regenerate an already tracked sidecar. Duplicate or changed IDs can break `uid://` references.
- Move, rename, or delete the source and sidecar together. Never commit `game/.godot/`.
- `game/scripts/multiplayer_replication.gd.uid` and `game/scripts/network_status.gd.uid` predate M2 as untracked files in the primary worktree. M2 integration leaves them untouched; handle either only in a separate, intentional review rather than copying or regenerating them in an isolated M2 worktree.

## 4. Release qualification

Run the metadata-only gate without building:

```bash
scripts/check-release-metadata.sh 0.2.0
```

GitHub Actions then provides these credential-free gates:

1. **CI:** `cargo check --locked --all-targets` and `just check` on Ubuntu, macOS, and Windows.
2. **Linux Godot:** a checksum-verified Godot 4.7.1 editor runs the bounded real scene/smoke suite. `scripts/check-alpha-smoke-log.sh` additionally requires the integrated one-lobby contract marker. That fixture/source marker does not prove HTTP, provider, coherent multiplayer, cleanup, two-download, or human qualification.
3. **Release QA tooling:** the deterministic M2–M5 playtest aggregator, secret-canary, lifecycle-evidence, trust-blocker, and no-overwrite tests run on Linux.
4. **Client Preflight:** pull requests, main pushes, manual dispatches, and later release tags export Linux x86_64, Windows x86_64, and macOS universal archives. The combined short-lived workflow artifact includes checksums, SPDX metadata, platform trust records, and verified GitHub provenance on non-PR runs. Preflight never creates a release, tag, package, or deployment.

The current macOS candidate is only ad-hoc signed and is not notarized. The current Windows candidate has no Authenticode signature. Both are explicit release blockers; checksums and provenance do not waive them. Tag-triggered package jobs validate but do not publish stable OCI aliases. Do not create `v0.2.0` until the exact-SHA release evidence manifest and every implementation, safety, lifecycle, artifact, and human gate are green. Publishing remains a separate protected-environment dispatch that refuses to overwrite any draft or published release.

See [alpha-release-qualification.md](alpha-release-qualification.md) for candidate artifacts, telemetry aggregation, two-client entry points, and the terminal evidence contract.

## 5. Manual gameplay check

```bash
just game-run
```

Verify:

1. W progresses Walk → Trot → Gallop.
2. S brakes before reversing.
3. A/D sidestep at rest and steer while moving.
4. Mouse 1 fires; Mouse 2 aims; R reloads.
5. 1/2/3 switch horse archetypes; 4/5/6 switch rifles.
6. The lower-left network panel says `NET OFFLINE` when no lobby join credential has been supplied. This is expected for local play.
7. At grounded speed ≥8 m/s, press E to Saddle Dive; below 8 m/s, E performs an ordinary dismount. Verify the horse runs out and remains retrievable, airborne reload is blocked, recovery gates movement/fire, and remount does not teleport the horse.
8. Escape releases the mouse and opens the capture gate; click once to recapture. Escape never quits. Use the gate's `QUIT` button so network matches follow normal leave/cleanup.
9. Build Spur through riding/combat, verify the 50/80/100 readiness tones, and press Q at full meter. Mounted Q must show a six-second Majestic Charge countdown; horseless Q must begin Majestic Return immediately and consume the meter without starting Charge.

M2's forced smoke scenarios verify deterministic mechanics, not natural-play success. Record real 15-minute sessions until the observational gates can be evaluated: 2–4 dives per player, airborne hit rate +25–40% relative to gallop, 25–40% deaths within three seconds after landing, all four notifications naturally within three matches, and reversal presentation without animation popping. Until evidence meets those bands, status stays **implementation complete / playtest pending**.

M4 follows the same evidence rule. The aggregator reports points by each locked source, movement-style share, median and p75 Charges per observed actor, median time to first Charge, Charges per 15 player-minutes, Charge/full-meter time shares, contextual-spend split, and charged versus uncharged duel win rates. Source completion does not check off M4 until invited-player sessions show median >=1 and p75 <=3 Charges per match, a +15–25% Charge duel delta, useful instant Return, and readiness players can identify without looking.

## 6. Credential-free signed process proofs

```bash
just p2p-proof
```

This bounded Linux gate starts two separate loopback UDP peer processes, installs one
server-signed exact-endpoint roster, rejects a tampered Ed25519 envelope before replay state can
move, and accepts signed traffic in both directions. It then starts three fresh peer processes,
waits for B and C to verify signed traffic from the initial authority A, kills A without a Leave
packet, and requires both survivors to agree on B at epoch 2. The migration proof rejects a
tampered signed migration checkpoint, accepts the intact hash-checked checkpoint containing
distinct bounded rider/combat state, rejects C's signed non-authority snapshot, accepts B's
subject-tagged authority snapshot, and accepts signed rider input after migration. A third fresh
three-process scenario uses live wire 2.0: it kills A, elects B, installs B's fragmented complete
M3–M5 checkpoint on C, waits for C's signed installation acknowledgment, and then verifies exact
score, clock, and active-objective continuity in B's next MatchState. Rust tests also cover forged
payload subjects, result deduplication across different transport sequences, invalid checkpoint
atomicity, no-op migration polling during fragment reassembly, and exactly-one epoch advancement.

Expected exact markers are:

```text
SPURFIRE_SIGNED_TWO_PROCESS_OK peer_processes=2 signatures=strict accepted_bidirectional=true combat=authority_once result_dedup=true authority=a epoch=1
SPURFIRE_SIGNED_THREE_PROCESS_MIGRATION_OK peer_processes=3 signatures=strict authority_roles=strict authority=a successor=b epoch=2 agreement=b,c checkpoint=hash_checked riders=2 combat_receipts=retained continued_play=true
SPURFIRE_SIGNED_WIRE2_M5_MIGRATION_OK peer_processes=3 signatures=strict authority=a successor=b epoch=2 checkpoint=complete_m3_m5 score_continuity=true clock_continuity=true objective_continuity=true
```

The wrapper refuses to run when a recognized Tailscale, capability, or GitHub credential variable
is present. After a locked build, it runs the proof under `env -i` with an isolated temporary home
and temp directory; peer children also clear their environments. The binary enforces bounded
control/scenario deadlines and reaps every child on both success and failure. It performs no
provider calls and is the deterministic signed-session process gate. Together with the Godot
source/contract smoke this is **implemented source proof**, not credentialed or human evidence. It
does not replace separately packaged invited-client play, real RustScale route coverage,
production key custody, natural M2 tuning, or provider lifecycle/cleanup evidence.

## 7. Visible three-client P2P demo

Create the gitignored `.env` if needed:

```bash
cp .env.example .env
```

Set `TS_CLIENT_ID` and `TS_CLIENT_SECRET` to an organization OAuth client that can create API-only tailnets. Never commit this file.

Run:

```bash
just p2p-demo
```

This builds once, creates a disposable child tailnet, and opens three Godot windows named **Rider A**, **Rider B**, and **Rider C**. Wait until every HUD says `NET CONNECTED`. Focus any window and ride with W/A/S/D; its horse is replicated into both other windows through real RustScale application UDP. Hold **Tab** to show every rider's `DIRECT`, `DERP`, or `PEER RELAY` path, application RTT, live/stale state, and authority badge. Close all three windows—or press Ctrl-C in the launching terminal—to delete the child tailnet.

Expected terminal markers:

```text
SPURFIRE_GODOT_P2P_READY local=a peers=2
SPURFIRE_GODOT_P2P_READY local=b peers=2
SPURFIRE_GODOT_P2P_READY local=c peers=2
SPURFIRE_GODOT_P2P_ROUTE local=a peer=b route=DIRECT
SPURFIRE_GODOT_P2P_RTT local=a peer=b rtt_ms=<measured>
SPURFIRE_GODOT_P2P_SNAPSHOT local=a ...
cleanup: deleted P2P demo tailnet ...
```

`just game-run` is intentionally a single local client and therefore displays `NET OFFLINE`; it is not the three-player launcher.

## 8. Automated UDP and authority-loss test

Run:

```bash
just p2p-live
```

The headless probe creates a disposable child tailnet and should print:

```text
SPURFIRE_P2P_UDP_OK ... route_a_to_b=Direct|Derp|PeerRelay ...
SPURFIRE_MIGRATION_OK authority=a successor=b epoch=2 continued_play=true
SPURFIRE_P2P_LIFECYCLE_OK tailnet=<deleted-tailnet>
```

`Direct` is preferred but relay paths are valid when NAT or local policy prevents direct UDP. The migration marker proves three separate OS peer processes formed a mesh, authority A was forcibly terminated, B and C agreed on B at epoch 2, and a rider-input packet was accepted after migration.

The cleanup trap attempts to delete the tailnet on success, failure, interruption, or timeout. This development probe is **not Alpha lifecycle evidence**: its delete acknowledgement is not two exact stable-ID absence observations plus verified vault erasure, and it uses a permissive temporary policy. A macOS warning containing `portmapper cleanup remains uncertain` is a known RustScale local-shutdown issue, but any cleanup uncertainty still blocks Alpha lifecycle qualification even when UDP/migration markers appear.

The RustScale probe proves real transport and process-loss behavior, while `just p2p-proof` is the
credential-free strict-signature gate. The current RustScale migration executable still uses the
legacy insecure envelope path; do not treat its lifecycle marker alone as signed-session
qualification.

## 9. Final secret and repository checks

```bash
git diff --check
git status --short
git diff -- . ':!.env' | rg 'tskey-|Bearer |clientSecret' || true
```

No auth key, bearer token, OAuth secret, capability plaintext, or generated child credential should appear.

## 10. Control-plane network-view and activation plan

The normative matrix is [control-plane-network-view.md#required-test-plan](control-plane-network-view.md#required-test-plan). It covers exact-lobby capabilities/audience projection, FQDN validation, directional report aggregation, stale/unknown facts, cache-only inspection, one-real-lobby leasing, startup reconciliation, exact-ID cleanup proof, secret canaries, and the never-join dependency gate.

For this workstream, do **not** build on the development Mac and do not run a live provider probe. Push the branch, then validate from a clean credential-free Linux checkout; never copy `.env`, OAuth material, auth keys, or capabilities to any build host. Documentation/Helm checks may use:

```bash
git diff --check origin/main...HEAD
scripts/check-packaging.sh
```

Implementation branches additionally run their scoped Rust checks on the Linux builder. Cross-platform compilation, tests, and artifacts run only in GitHub Actions. No test in this slice may create/delete a live tailnet, alter a hosted deployment, disturb a managed peer session, tag a release, or publish a package.

Before a separate public-real activation review, the evidence bundle must include:

1. passing unit/fault tests for all capability, identity, freshness, lease, reconciliation, and cleanup cases in the normative matrix;
2. rendered-chart policy evidence that the hosted public deployment remains `dryRun=true`, `provisioningMode=dry_run`, `existingSecret=""`, and `persistence.enabled=false`;
3. a clean Linux run with no credentials and passing cross-platform GitHub Actions;
4. separately approved, credentialed live evidence for restrictive child policy, device-list semantics, Direct/DERP/Peer Relay gameplay paths, and exact-ID cleanup;
5. exercised operator alerts/runbook for create ambiguity, missing vault material, orphan quarantine, cleanup polling failure, and vault deletion failure.

A live probe passing does not waive an activation gate, and a lobby `DESTROYED` result is not cleanup proof.
