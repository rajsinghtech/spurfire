# Spurfire

*High noon. Low ping.*

**Spurfire** is a third-person, peer-hosted open-range movement shooter where every player fights from horseback, performs dangerous flying dismounts, builds Spur with stylish riding, and battles across terrain that scales with the size of the lobby. Today the prototype ships mounted locomotion, the SF-series rifle range, and the networking spine; the Saddle Dive, on-foot kit, Spur meter, and Bounty Run are designed (see `docs/prototype-plan.md`) but not yet built.

This repository contains the Rust **control plane** and the Godot game prototype. The control plane provisions Tailscale-backed lobbies, mints one-use join credentials, exposes the `spurfire-server` HTTP service, and provides the `spurfire-ctl` operations CLI. The game uses Godot 4.7.1 with Rust GDExtension gameplay classes. Game clients embed pinned [RustScale](https://github.com/rajsinghtech/rustscale) native Rust UDP and play peer-to-peer; `spurfire-server` is never a permanent gameplay server.

## Repository layout

```text
crates/spurfire-control/   Tailscale API client and provisioning primitives
crates/spurfire-protocol/  Wire DTOs, lobby state machine, deterministic election
crates/spurfire-server/    spurfire-server Axum lobby control service
crates/spurfire-cli/       spurfire-ctl development and operations CLI
crates/spurfire-gdext/     Rust gameplay classes for Godot
game/                      Godot 4.7.1 project and graybox tests
charts/spurfire-control/   Hardened OCI-published Helm deployment chart
scripts/                   API probes and game build/test helpers
Dockerfile                 Multi-stage, non-root spurfire-server image
docs/design.md             Product and game design source of truth
docs/architecture.md       Control/data planes and trust boundaries
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
.github/workflows/         Locked CI gates and manual credentialed e2e
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
- `just game-test` — headless import and M0 smoke tests with a bounded timeout.
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

The chart defaults to one credential-free dry-run replica with no public route. It includes opt-in Gateway API values for `spurfire.rajsingh.info`; the prototype API must be protected by external authentication before public use. See [docs/deployment.md](docs/deployment.md) for tags, digest/signature verification, Helm installation, existing-Secret real mode, persistence, and restart caveats.

## Documentation

- [docs/design.md](docs/design.md) — game design and product source of truth.
- [docs/architecture.md](docs/architecture.md) — system architecture and boundaries.
- [docs/lobby-service.md](docs/lobby-service.md) — server routes, state machine, examples, and security limits.
- [docs/deployment.md](docs/deployment.md) — OCI artifacts, Docker, Helm, Gateway API, and operations.
- [docs/decisions.md](docs/decisions.md) — decisions and open questions.
- [docs/tailscale-api.md](docs/tailscale-api.md) — current API permission evidence.
- [docs/rustscale-integration.md](docs/rustscale-integration.md) — sibling integration readiness.
- [docs/rustscale-tailnet-tooling.md](docs/rustscale-tailnet-tooling.md) — reference script comparison and safe wrapper policy.
- [docs/godot-m0.md](docs/godot-m0.md) — local setup, automation, M0 checks, and platform caveats.

## Status

The control plane, protocol, CLI, and HTTP lobby prototype implement organization tailnet-per-lobby provisioning with an in-memory, redacted child-secret vault. Godot 4.7.1 plus Rust GDExtension provides mounted movement/combat (milestones M0–M1) and a native `PeerSession`. Disposable live probes have verified three Godot peers exchanging sustained gameplay UDP with per-peer direct/DERP/peer-relay classification and application RTT, then deleting the child tailnet. Gameplay milestones M2–M5 (Saddle Dive, on-foot kit, Spur meter, Bounty Run) are designed but unbuilt. M6 completion work (one unified migration rule, real match-state handoff, capped-rewind lag compensation, a client-driven lobby join flow, landing-page live stats) plus restart recovery, cross-platform packaging, and RustScale's platform/telemetry gaps remain before an alpha; see `docs/prototype-plan.md` and `docs/decisions.md`.
