# Spurfire

*High noon. Low ping.*

**Spurfire** is a third-person, peer-hosted open-range shooter where every player fights from horseback, performs dangerous flying dismounts, develops a bond with their mount, and battles across terrain that scales with the size of the lobby.

This repository contains the Rust **control plane** and the Godot game prototype. The control plane provisions Tailscale-backed lobbies, mints one-use join credentials, exposes the `spurfire-server` HTTP service, and provides the `spurfire-ctl` operations CLI. The game uses Godot 4.7.1 with Rust GDExtension gameplay classes. Game clients embed pinned [RustScale](https://github.com/rajsinghtech/rustscale) native Rust UDP and play peer-to-peer; `spurfire-server` is never a permanent gameplay server.

## Repository layout

```text
crates/spurfire-control/   Tailscale API client and provisioning primitives
crates/spurfire-protocol/  Wire DTOs, lobby state machine, deterministic election
crates/spurfire-server/    spurfire-server Axum lobby control service
crates/spurfire-cli/       spurfire-ctl development and operations CLI
crates/spurfire-gdext/     Rust gameplay classes for Godot
game/                      Godot 4.7.1 project and graybox tests
scripts/                   API probes and game build/test helpers
docs/design.md             Product and game design source of truth
docs/architecture.md       Control/data planes and trust boundaries
docs/lobby-service.md      HTTP routes, lifecycle, dry-run, and operations
docs/decisions.md          ADR-lite decisions and blocking questions
docs/tailscale-api.md      Redacted Tailscale probe evidence and verdict
docs/rustscale-integration.md  RustScale readiness survey
docs/p2p-networking.md       Peer protocol, replication, migration, and live UDP proof
docs/testing.md              Local, Godot, manual, and live test steps
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
- `just game-editor` / `just game-run` — edit or run the Godot project.
- `just --list` — all recipes.

## Documentation

- [docs/design.md](docs/design.md) — game design and product source of truth.
- [docs/architecture.md](docs/architecture.md) — system architecture and boundaries.
- [docs/lobby-service.md](docs/lobby-service.md) — server routes, state machine, examples, and security limits.
- [docs/decisions.md](docs/decisions.md) — decisions and open questions.
- [docs/tailscale-api.md](docs/tailscale-api.md) — current API permission evidence.
- [docs/rustscale-integration.md](docs/rustscale-integration.md) — sibling integration readiness.
- [docs/rustscale-tailnet-tooling.md](docs/rustscale-tailnet-tooling.md) — reference script comparison and safe wrapper policy.
- [docs/godot-m0.md](docs/godot-m0.md) — local setup, automation, M0 checks, and platform caveats.

## Status

The control plane, protocol, CLI, and HTTP lobby prototype implement organization tailnet-per-lobby provisioning with an in-memory, redacted child-secret vault. Godot 4.7.1 plus Rust GDExtension provides mounted movement/combat and a native `PeerSession`. A disposable live probe has verified two embedded RustScale peers exchanging bounded Spurfire gameplay UDP directly and then deleting the child tailnet. Restart recovery, cross-platform packaging, full gameplay replication/interpolation, migration under real process loss, and RustScale's platform/telemetry gaps remain production blockers.
