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

This checks formatting, runs Clippy with warnings denied, and executes the complete Rust test suite. Replication tests cover jittered snapshots, shortest-arc yaw, bounded extrapolation, replay rejection, stale authority epochs, and deterministic migration.

## 3. Godot integration gate

```bash
just game-test
```

This builds and signs the Rust GDExtension, imports assets, then runs:

- Horse/controller and native networking smoke.
- `PeerSession` envelope/snapshot/replay checks.
- `NetworkRider` interpolation and reconciliation checks.
- Polish UI smoke.
- Mounted combat smoke.
- Main-scene startup.

Expected markers include:

```text
SPURFIRE_GODOT_SMOKE_OK
SPURFIRE_POLISH_SMOKE_OK
SPURFIRE_COMBAT_UI_SMOKE_OK
```

## 4. Manual gameplay check

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
7. Escape releases the mouse; press Escape again to quit.

## 5. Visible three-client P2P demo

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

## 6. Automated UDP and authority-loss test

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

The cleanup trap deletes the tailnet on success, failure, interruption, or timeout. A macOS warning containing `portmapper cleanup remains uncertain` is a known RustScale local-shutdown issue; it does not invalidate the test if the success and lifecycle markers appear.

## 7. Final secret and repository checks

```bash
git diff --check
git status --short
git diff -- . ':!.env' | rg 'tskey-|Bearer |clientSecret' || true
```

No auth key, bearer token, OAuth secret, capability plaintext, or generated child credential should appear.

## 8. Control-plane network-view and activation plan

The normative matrix is [control-plane-network-view.md#required-test-plan](control-plane-network-view.md#required-test-plan). It covers exact-lobby capabilities/audience projection, FQDN validation, directional report aggregation, stale/unknown facts, cache-only inspection, one-real-lobby leasing, startup reconciliation, exact-ID cleanup proof, secret canaries, and the never-join dependency gate.

For this workstream, do **not** build on the development Mac and do not run a live provider probe. Push the branch, then validate from a clean credential-free checkout on `ssh ubuntu@raj-builder`; never copy `.env`, OAuth material, auth keys, or capabilities to the builder. Documentation/Helm checks may use:

```bash
git diff --check origin/main...HEAD
scripts/check-packaging.sh
```

Implementation branches additionally run their scoped Rust checks on the Linux builder. Cross-platform compilation, tests, and artifacts run only in GitHub Actions. No test in this slice may create/delete a live tailnet, alter Ottawa, disturb a managed peer session, tag a release, or publish a package.

Before a separate public-real activation review, the evidence bundle must include:

1. passing unit/fault tests for all capability, identity, freshness, lease, reconciliation, and cleanup cases in the normative matrix;
2. rendered-chart policy evidence that Ottawa remains `dryRun=true`, `provisioningMode=dry_run`, `existingSecret=""`, and `persistence.enabled=false`;
3. a clean Linux run with no credentials and passing cross-platform GitHub Actions;
4. separately approved, credentialed live evidence for restrictive child policy, device-list semantics, Direct/DERP/Peer Relay gameplay paths, and exact-ID cleanup;
5. exercised operator alerts/runbook for create ambiguity, missing vault material, orphan quarantine, cleanup polling failure, and vault deletion failure.

A live probe passing does not waive an activation gate, and a lobby `DESTROYED` result is not cleanup proof.
