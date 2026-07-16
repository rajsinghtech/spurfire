# AGENTS.md

## Project overview

**Pitch:** *High noon. Low ping.* Spurfire is a third-person, peer-hosted horseback shooter with flying dismounts, mount bonding, and lobby-scaled open terrain.

This repository is the Rust control plane. It provisions Tailscale-backed lobbies and join credentials through `spurfire-control`, and exposes development and operations workflows through the `spurfire-ctl` CLI. Gameplay is peer-to-peer; a future backend may handle matchmaking, provisioning, cleanup, identity, and results, but it is not a permanent gameplay server.

## Repository layout

- `crates/spurfire-control/` — Tailscale API client and lobby lifecycle library.
- `crates/spurfire-cli/` — `spurfire-ctl` command-line client.
- `scripts/` — safe API probes and development helpers.
- `docs/design.md` — product and game design source of truth.
- `docs/architecture.md` — control/data planes, trust boundaries, and lifecycle.
- `docs/decisions.md` — ADR-lite decisions and blocking questions.
- `docs/tailscale-api.md` — redacted API probe evidence and provisioning verdict.
- `docs/rustscale-integration.md` — RustScale readiness and integration survey.
- `.github/workflows/ci.yml` — continuous integration gates.

## Development commands

Use the `justfile` recipes when available:

- `just setup` — verify tools and fetch dependencies.
- `just fmt` — apply Rust formatting.
- `just lint` — run Clippy for all targets with warnings denied.
- `just test` — run all tests.
- `just check` — run the complete local gate (`fmt --check`, Clippy, tests).
- `just e2e` — run the live, credentialed Tailscale smoke test.
- `just clean` — remove build artifacts.

The workspace requires Rust 1.91 or newer.

## Environment variables and secrets

Copy `.env.example` to the gitignored `.env` for local live-API work:

- `TS_CLIENT_ID` — Tailscale OAuth client ID.
- `TS_CLIENT_SECRET` — Tailscale OAuth client secret.
- `TS_API_BASE` — API root, normally `https://api.tailscale.com/api/v2`.

OAuth credentials are control-plane secrets. They must never be committed, logged, embedded in binaries, or shipped in game clients. Clients may receive only narrowly scoped, one-use, short-lived join credentials. Keep `.env` gitignored; redact OAuth tokens and generated auth keys from reports and fixtures.

## Provisioning modes

- `TailnetPerLobby`: implemented as an explicit unavailable path, not operational. Verified singular and plural tailnet-create endpoints returned 404, so the alpha create API is unavailable to the tested deployment.
- `SharedTailnet`: implemented and the nearest viable mode, but not verified end to end with the current OAuth client. The tested client received 403 for auth-key, device-list, and ACL operations. It requires appropriate scopes plus successful key issuance, ACL/tag ownership, device cleanup, and live integration tests before production use.

See `docs/tailscale-api.md` and `crates/spurfire-control/NOTES.md` before changing either verdict.

## RustScale caveat

The sibling repository `/Users/rajsingh/Documents/GitHub/rustscale` is under active development. Connectivity, relay, enrollment, telemetry, FFI, or platform bugs may live there rather than in Spurfire. A Rust-only prototype is currently feasible, but production all-platform embedding is not. See `docs/rustscale-integration.md` and re-check its pinned survey revision before integrating.

## Agent worktree pattern

For parallel agent work, create one branch and sibling worktree per disjoint task (for example, `agent/<task>` and `../spurfire-wt-<task>`). Assign path ownership up front, commit each branch, then merge into `main` with `git merge --no-ff`. Run formatting, lint, and tests only after all merges. Remove completed worktrees with `git worktree remove --force` when they may contain untracked `.env` files, and verify with `git worktree list`. Never commit or copy worktree secrets.

## Model routing policy

- Design work: `moonshotai/kimi-k3` — limited quantity; use sparingly.
- Execution and coding: `openai-codex/gpt-5.6-sol`.

## Blocking open questions

1. **Game engine:** no engine is selected; this blocks data-plane implementation and determines whether RustScale's current Rust API or incomplete C ABI can be used.
2. **Ranked verification:** peer-hosted ranked results need a trust model, such as co-signing or a witness/replay-validation service; none is selected.

## Known issues

- Tailnet-per-lobby cannot operate because no verified tailnet-create API route is available.
- Shared-tailnet live provisioning is blocked for the tested OAuth client by insufficient auth-key, device, and ACL permissions.
- RustScale's C ABI lacks gameplay UDP, RTT/status telemetry is incomplete, and peer-relay Hostinfo advertisement may regress during refresh; track these in the sibling repository.
