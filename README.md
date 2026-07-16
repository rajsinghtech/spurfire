# Spurfire

*High noon. Low ping.*

**Spurfire** is a third-person, peer-hosted open-range shooter where every player fights from
horseback, performs dangerous flying dismounts, develops a bond with their mount, and battles
across terrain that scales with the size of the lobby.

This repo is the **control plane**: the Rust workspace that provisions lobby tailnets, mints
one-use join credentials, and drives the lobby lifecycle via the `spurfire-ctl` CLI. Game
clients embed [RustScale](https://github.com/rajsinghtech/rustscale) (sibling repo) and play
peer-to-peer; no permanent dedicated gameplay server.

## Repo layout

```
crates/spurfire-control/   Control-plane library: Tailscale tailnet & auth-key lifecycle
crates/spurfire-cli/       spurfire-ctl binary: lobby lifecycle CLI
scripts/                   Helper scripts (Tailscale API smoke tests, etc.)
docs/design.md             Product spec (source of truth)
docs/architecture.md       Control plane vs data plane, trust boundaries, lobby lifecycle
docs/decisions.md          ADR-lite decision log and open questions
docs/tailscale-api.md      Tailscale API notes
.github/workflows/         CI (fmt, clippy, tests; manual e2e)
justfile                   Task runner recipes
```

## Quickstart

Prereqs: a stable Rust toolchain (`rustup`), [`just`](https://github.com/casey/just), and
Tailscale OAuth client credentials (admin console → Settings → OAuth clients).

```sh
cp .env.example .env   # fill in TS_CLIENT_ID / TS_CLIENT_SECRET
just setup             # verify toolchain + fetch dependencies
cargo test             # run the test suite
```

Other useful recipes: `just check` (fmt + lint + test), `just e2e` (live Tailscale API smoke
test, requires `.env`), `just --list` for everything.

## Docs

- [docs/design.md](docs/design.md) — game design & product spec
- [docs/architecture.md](docs/architecture.md) — system architecture & trust boundaries
- [docs/decisions.md](docs/decisions.md) — decision log (ADRs) & open questions
- [docs/tailscale-api.md](docs/tailscale-api.md) — Tailscale API reference notes

## Status

Early-stage scaffolding: the control plane and tooling are under active development and the
game itself is not yet playable.
