<p align="center"><img src="assets/spurfire-logo.svg" alt="Spurfire logo" width="120"></p>

# Spurfire

*High noon. Low ping.*

**Spurfire** is a third-person, peer-hosted open-range movement shooter where every player fights from horseback, performs dangerous flying dismounts, builds Spur with stylish riding, and battles across terrain that scales with the size of the lobby. Today the prototype includes mounted locomotion, the SF-series rifle range, the live-verified networking spine, and the M2 Saddle Dive implementation prepared for 0.2.0. Saddle Dive is **playtest pending, not done**. M3 now has a deterministic native authority foundation for horse loss, on-foot stances, recall, return, and remount, but its wire-v2/Godot/combat integration is not complete; the Spur meter and Bounty Run remain unbuilt (see `docs/prototype-plan.md`).

This repository contains the Rust **control plane** and the Godot game prototype. The control plane provisions Tailscale-backed lobbies, mints one-use join credentials, exposes the `spurfire-server` HTTP service, and provides the `spurfire-ctl` operations CLI. The game uses Godot 4.7.1 with Rust GDExtension gameplay classes. Game clients embed pinned [RustScale](https://github.com/rajsinghtech/rustscale) native Rust UDP and play peer-to-peer; `spurfire-server` is never a permanent gameplay server.

## Repository layout

```text
crates/spurfire-control/   Tailscale API client and provisioning primitives
crates/spurfire-protocol/  Wire DTOs, lobby state machine, deterministic election
crates/spurfire-server/    spurfire-server Axum lobby control service
crates/spurfire-cli/       spurfire-ctl development and operations CLI
crates/spurfire-gdext/     Rust gameplay classes for Godot
game/                      Godot 4.7.1 project and graybox tests
assets/                    Brand assets (canonical Spurfire rowel mark)
charts/spurfire-control/   Hardened OCI-published Helm deployment chart
scripts/                   API probes and game build/test helpers
Dockerfile                 Multi-stage, non-root spurfire-server image
docs/design.md             Product and game design source of truth
docs/architecture.md       Control/data planes and trust boundaries
docs/control-plane-network-view.md  Network ownership, inspector, gates, runbook
docs/lobby-service.md      HTTP routes, lifecycle, dry-run, and operations
docs/decisions.md          ADR-lite decisions and blocking questions
docs/tailscale-api.md      Redacted Tailscale probe evidence and verdict
docs/rustscale-integration.md  RustScale readiness survey
docs/p2p-networking.md       Peer protocol, replication, migration, and live UDP proof
docs/testing.md              Local, Godot, manual, and live test steps
docs/deployment.md           Container, Helm, GHCR, and deployment operations
docs/rustscale-tailnet-tooling.md  Organization-tailnet script comparison
docs/godot-m0.md           Godot setup, M0/M0.5 handling contract, and platform notes
docs/asset-licenses.md     Verified provenance and licenses for imported game assets
docs/combat-m1.md          Mounted assault-rifle controls, authority boundary, and tests
docs/release-notes-0.2.0.md  M2 release scope, verification, and pending playtest gates
.github/workflows/         Locked CI, client preflight, and manual credentialed e2e
justfile                   Task runner recipes
```

## Quickstart

Control-plane prerequisites: Rust 1.91 or newer and [`just`](https://github.com/casey/just). Game development also requires Godot 4.7.1 and Bash; see [docs/godot-m0.md](docs/godot-m0.md) for macOS, Linux, and Windows setup.

```sh
just setup
just check

# Start a zero-mutation lobby service; no credentials required.
just serve-dry
# In another terminal:
curl -sS http://127.0.0.1:8080/healthz
curl -sS http://127.0.0.1:8080/v1/capabilities

# Build the Rust GDExtension and run bounded headless Godot smoke tests.
just game-test
# Or iterate locally.
just game-editor
```

PowerShell:

```powershell
cargo run -p spurfire-server -- --dry-run --bind 127.0.0.1:8080
Invoke-RestMethod http://127.0.0.1:8080/healthz
```

For live API probing only, copy `.env.example` to the gitignored `.env` and fill in `TS_CLIENT_ID` and `TS_CLIENT_SECRET`. Organization-tailnet list/create, child token exchange, and child deletion are verified; shared-tailnet key/device/ACL scopes remain blocked in the historical probe. Dry-run remains the safe default. Never place organization or child OAuth credentials in a game client.

Useful commands:

- `just check` — format check, Clippy with warnings denied, and all tests.
- `just serve-dry` — loopback dry-run server with zero Tailscale mutations.
- `cargo run -p spurfire-server -- --help` — server options.
- `just e2e` — manual live token probe; requires `.env`.
- `just game-build [debug|release]` — build and install the platform-native GDExtension.
- `just game-test` — headless import and M0–M2 smoke tests with a bounded timeout.
- `just game-editor` / `just game-run` — edit or run one local Godot client.
- `just p2p-demo` — provision a disposable tailnet and open three replicated Godot clients with a Tab route/RTT roster.
- `just p2p-live` — headless real-UDP and forced-authority-migration probe.
- `just --list` — all recipes.

## Packaged control service

GitHub Actions publishes Linux amd64/arm64 images and the OCI chart; local workflows only validate them:

```text
ghcr.io/rajsinghtech/spurfire-server
oci://ghcr.io/rajsinghtech/charts/spurfire-control
```

The chart defaults to one credential-free dry-run replica with no public route. The hosted preview deliberately exposes only the forced-dry-run service: public real provisioning remains blocked by capability/abuse controls, encrypted dynamic child-credential recovery, startup reconciliation, and the other activation gates. See [docs/deployment.md](docs/deployment.md) for artifacts and [docs/control-plane-network-view.md](docs/control-plane-network-view.md) for the authoritative activation/runbook contract.

## Release qualification

Release metadata is checked by `scripts/check-release-metadata.sh`: the server manifest and lockfile row, Helm chart and app versions, Godot `config/version`, landing-page label, and release-note filename/heading must agree. Internal crates remain at 0.1.0.

Every pull request runs `just check` on Ubuntu, macOS, and Windows plus the bounded Godot 4.7.1 smoke suite on Linux. The nonpublishing **Client Preflight** workflow runs on pull requests and manual dispatches and builds Linux x86_64, Windows x86_64, and macOS universal archives. It uploads only short-lived workflow artifacts; it does not create a tag or GitHub release, publish a package, advance an alias, or require Apple notarization credentials. See [docs/testing.md](docs/testing.md) and [docs/deployment.md](docs/deployment.md).

## Documentation

- [docs/design.md](docs/design.md) — game design and product source of truth.
- [docs/architecture.md](docs/architecture.md) — system architecture and boundaries.
- [docs/control-plane-network-view.md](docs/control-plane-network-view.md) — dedicated ownership, never-join decision, exact-lobby inspector, telemetry provenance, activation gates, and operator runbook.
- [docs/lobby-service.md](docs/lobby-service.md) — current/target routes, state machines, examples, and security limits.
- [docs/deployment.md](docs/deployment.md) — OCI artifacts, Docker, Helm, Gateway API, and operations.
- [docs/decisions.md](docs/decisions.md) — decisions and open questions.
- [docs/tailscale-api.md](docs/tailscale-api.md) — current API permission evidence.
- [docs/rustscale-integration.md](docs/rustscale-integration.md) — sibling integration readiness.
- [docs/rustscale-tailnet-tooling.md](docs/rustscale-tailnet-tooling.md) — reference script comparison and safe wrapper policy.
- [docs/godot-m0.md](docs/godot-m0.md) — local setup, automation, smoke checks, and Godot UID policy.
- [docs/release-notes-0.2.0.md](docs/release-notes-0.2.0.md) — M2 scope, release qualification, and observational gates.

## Status

The control plane, protocol, CLI, and HTTP lobby prototype implement organization tailnet-per-lobby provisioning with an encrypted exact-tuple file vault, writer fencing, and mutation-closed startup reconciliation. Organization child create/token/delete is live-proven; child-scoped one-use enrollment is implemented and mock-tested but still needs live end-to-end verification. The main control plane never joins a lobby tailnet. Public real activation remains closed until workload identity/external custody, persistent abuse controls, restrictive child-policy proof, orphan-remediation exercises, and the remaining operations gates are complete; the hosted preview remains dry-run. Godot 4.7.1 plus Rust GDExtension provides mounted movement/combat (M0–M1), the M2 Saddle Dive implementation, and a native `PeerSession`. M2 is **implementation complete / playtest pending**: deterministic and headless gates do not replace the natural frequency, hit-rate, post-landing-death, notification, and animation checks in `docs/prototype-plan.md`. M3 native authority groundwork is in progress; wire-v2 actor replication, target geometry, combat/Godot presentation, and acceptance instrumentation remain before M3 is playable. M4–M5 remain unbuilt. M6 completion work and RustScale's remaining platform/telemetry gaps still block alpha; see `docs/prototype-plan.md`, `docs/decisions.md`, and `docs/control-plane-network-view.md`.
