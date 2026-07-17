# Spurfire

*High noon. Low ping.*

**Spurfire** is a third-person, peer-hosted open-range shooter where every player fights from horseback, performs dangerous flying dismounts, develops a bond with their mount, and battles across terrain that scales with the size of the lobby.

This repository is the Rust **control plane**. It provisions Tailscale-backed lobbies, mints one-use join credentials, exposes the `spurfire-server` HTTP service, and provides the `spurfire-ctl` operations CLI. Game clients embed [RustScale](https://github.com/rajsinghtech/rustscale) (sibling repository) and play peer-to-peer; `spurfire-server` is never a permanent gameplay server.

## Repository layout

```text
crates/spurfire-control/   Tailscale API client and provisioning primitives
crates/spurfire-protocol/  Wire DTOs, lobby state machine, deterministic election
crates/spurfire-server/    spurfire-server Axum lobby control service
crates/spurfire-cli/       spurfire-ctl development and operations CLI
scripts/                   Safe API probes and development helpers
docs/design.md             Product and game design source of truth
docs/architecture.md       Control/data planes and trust boundaries
docs/lobby-service.md      HTTP routes, lifecycle, dry-run, and operations
docs/decisions.md          ADR-lite decisions and blocking questions
docs/tailscale-api.md      Redacted Tailscale probe evidence and verdict
docs/rustscale-integration.md  RustScale readiness survey
docs/rustscale-tailnet-tooling.md  Organization-tailnet script comparison
.github/workflows/         Locked CI gates and manual credentialed e2e
justfile                   Task runner recipes
```

## Quickstart

Prerequisites: Rust 1.91 or newer and [`just`](https://github.com/casey/just).

```sh
just setup
just check

# Start a zero-mutation lobby service; no credentials required.
just serve-dry
# In another terminal:
curl -sS http://127.0.0.1:8080/healthz
curl -sS http://127.0.0.1:8080/v1/capabilities
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
- `just --list` — all recipes.

## Documentation

- [docs/design.md](docs/design.md) — game design and product source of truth.
- [docs/architecture.md](docs/architecture.md) — system architecture and boundaries.
- [docs/lobby-service.md](docs/lobby-service.md) — server routes, state machine, examples, and security limits.
- [docs/decisions.md](docs/decisions.md) — decisions and open questions.
- [docs/tailscale-api.md](docs/tailscale-api.md) — current API permission evidence.
- [docs/rustscale-integration.md](docs/rustscale-integration.md) — sibling integration readiness.
- [docs/rustscale-tailnet-tooling.md](docs/rustscale-tailnet-tooling.md) — reference script comparison and safe wrapper policy.

## Status

The control plane, protocol, CLI, and HTTP lobby prototype now implement organization tailnet-per-lobby provisioning with an in-memory, redacted child-secret vault. This is not production-ready: restart recovery requires an encrypted secret manager and child key issuance still needs live end-to-end verification. Shared-tailnet provisioning remains blocked by historical OAuth scope/ACL evidence; the game itself is not yet playable.
