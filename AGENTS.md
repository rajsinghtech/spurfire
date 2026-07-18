# AGENTS.md

## Project overview

**Pitch:** *High noon. Low ping.* Spurfire is a third-person, peer-hosted horseback shooter with flying dismounts, mount bonding, and lobby-scaled open terrain.

This repository contains the Rust control plane and the Godot 4.7.1 game prototype. The control plane provisions Tailscale-backed lobbies and join credentials through `spurfire-control`, exposes the `spurfire-server` HTTP lobby service, and provides development and operations workflows through the `spurfire-ctl` CLI. Gameplay systems are Rust GDExtension classes hosted by Godot. Gameplay will be peer-to-peer after M0 movement validation; the control service handles metadata, provisioning, cleanup, and prototype results but is not a permanent gameplay server.

## Repository layout

- `crates/spurfire-control/` — Tailscale API client and lobby lifecycle library.
- `crates/spurfire-protocol/` — wire DTOs, lobby state machine, and deterministic authority election.
- `crates/spurfire-server/` — `spurfire-server` Axum HTTP lobby control service.
- `crates/spurfire-cli/` — `spurfire-ctl` command-line client.
- `crates/spurfire-gdext/` — Rust gameplay classes exposed to Godot through GDExtension.
- `game/` — Godot 4.7.1 project, scenes, input, camera, archetype UI, smoke tests, and curated CC0 assets under `game/assets/kenney/`.
- `scripts/` — safe API probes plus portable GDExtension build and Godot test helpers.
- `docs/design.md` — product and game design source of truth.
- `docs/architecture.md` — control/data planes, trust boundaries, and lifecycle.
- `docs/decisions.md` — ADR-lite decisions and blocking questions.
- `docs/tailscale-api.md` — redacted API probe evidence and provisioning verdict.
- `docs/rustscale-integration.md` — RustScale readiness and integration survey.
- `docs/rustscale-tailnet-tooling.md` — RustScale organization-tailnet script comparison and safe probe policy.
- `docs/lobby-service.md` — HTTP routes, lifecycle, dry-run, configuration, and trust boundaries.
- `docs/godot-m0.md` — Godot/GDExtension setup, commands, M0/M0.5 handling contract, and platform caveats.
- `docs/combat-m1.md` — mounted rifle controls, deterministic shot authority, presentation, and M1 limits.
- `docs/asset-licenses.md` — verified source URLs, licenses, and hashes for every imported asset.
- `.github/workflows/ci.yml` — continuous integration gates.

## Development commands

Use the `justfile` recipes when available:

- `just setup` — verify tools and fetch dependencies.
- `just fmt` — apply Rust formatting.
- `just lint` — run Clippy for all targets with warnings denied.
- `just test` — run all tests.
- `just check` — run the complete local gate (`fmt --check`, Clippy, tests).
- `just serve-dry` — run `spurfire-server` on loopback with zero provider mutations.
- `cargo run -p spurfire-server -- --help` — inspect server options.
- `just e2e` — run the live, credentialed Tailscale smoke test.
- `just game-build [debug|release]` — build and install the native GDExtension.
- `just game-test` — run the bounded headless Godot M0 smoke scene.
- `just game-editor` / `just game-run` — open or run the Godot project.
- `just clean` — remove build artifacts.

The workspace requires Rust 1.91 or newer.

## Environment variables and secrets

Copy `.env.example` to the gitignored `.env` for local live-API work:

- `TS_CLIENT_ID` — Tailscale OAuth client ID.
- `TS_CLIENT_SECRET` — Tailscale OAuth client secret.
- `TS_API_BASE` — API root, normally `https://api.tailscale.com/api/v2`.
- `SPURFIRE_DRY_RUN=1` — force zero-mutation server mode.
- `SPURFIRE_BIND_ADDR` — server listen socket, loopback by default.
- `SPURFIRE_STATE_PATH` — durable non-secret real-mode state file.

Organization and child OAuth credentials are control-plane secrets. They must never be committed, logged, embedded in binaries, persisted in the prototype JSON store, or shipped in game clients. Clients may receive only narrowly scoped, one-use, short-lived join credentials. Keep `.env` gitignored; redact OAuth tokens, one-time child secrets, and generated auth keys from reports and fixtures.

## Provisioning modes

- `TailnetPerLobby`: implemented through verified `GET/POST /organizations/-/tailnets`, child OAuth token exchange, and child-scoped tailnet deletion. The one-time child OAuth pair lives only in a provider-owned in-memory vault keyed by lobby ID. Restart fails closed with manual remediation; production requires an encrypted secret manager and reconciliation. Child auth-key minting is mock-tested but still needs live end-to-end verification.
- `SharedTailnet`: remains implemented but historically received 403 for auth-key, device-list, and ACL operations. Its readiness is reported independently from organization-tailnet access and still requires appropriate scopes, ACL/tag ownership, device cleanup, and live integration tests.

See `docs/tailscale-api.md` and `crates/spurfire-control/NOTES.md` before changing either verdict.

## Godot and RustScale caveats

Godot 4.7.1 with Rust GDExtension is the accepted game stack. Keep engine-facing scenes and content in `game/` and gameplay classes in `crates/spurfire-gdext/`; never commit generated libraries under `game/bin/`. M0 is movement-only, and RustScale integration starts only after the movement acceptance checks in `docs/godot-m0.md` pass.

A sibling checkout of the RustScale repository (conventionally `../rustscale`) is under active development. Connectivity, relay, enrollment, telemetry, FFI, or platform bugs may live there rather than in Spurfire. In-process Rust integration is feasible, but production all-platform embedding and Godot packaging are not validated. See `docs/rustscale-integration.md` and re-check its pinned survey revision before integrating.

## Agent worktree pattern

For parallel agent work, create one branch and sibling worktree per disjoint task (for example, `agent/<task>` and `../spurfire-wt-<task>`). Assign path ownership up front, commit each branch, then merge into `main` with `git merge --no-ff`. Run formatting, lint, and tests only after all merges. Remove completed worktrees with `git worktree remove --force` when they may contain untracked `.env` files, and verify with `git worktree list`. Never commit or copy worktree secrets.

## Model routing policy

- Game/control-plane design work: `ai/moonshotai/kimi-k3` — limited quantity; use sparingly.
- Rust, GDExtension, Godot integration, automation, and other execution work: `openai-codex/gpt-5.6-sol`.

## Blocking open questions

1. **Ranked verification:** peer-hosted ranked results need a trust model, such as co-signing or a witness/replay-validation service; none is selected.

## Known issues

- Tailnet-per-lobby prototype secrets are process-local; restart makes retained child lobbies `cleanup_pending` with manual remediation until an encrypted secret manager/reconciler is integrated.
- Child one-use auth-key issuance is implemented and mock-tested but was not live-mutated in this correction workflow.
- Shared-tailnet live provisioning is blocked by the historical OAuth client's insufficient auth-key, device, and ACL permissions.
- Godot desktop native-library packaging is automated but mobile, console, Windows ARM64, and large-world/16-rider performance remain unvalidated.
- RustScale's C ABI lacks gameplay UDP, RTT/status telemetry is incomplete, and peer-relay Hostinfo advertisement may regress during refresh; track these in the sibling repository after M0 movement validation.
