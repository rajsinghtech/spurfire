# Spurfire Decision Log (ADR-lite)

Numbered decisions with status: **accepted** | **provisional** | **open**.

## D1 — Peer-hosted authority

**Status:** accepted

Gameplay uses peer-hosted authoritative networking, not distributed simulation. All gameplay
machines are peers on a lobby tailnet; one player is elected match authority and validates
movement, shots, damage, score, and events. No permanent dedicated gameplay server. Peers
keep recent state snapshots for authority migration.

## D2 — Tailnet-per-lobby with shared-tailnet fallback

**Status:** provisional — pending Tailscale API verification

Each lobby gets its own tailnet for isolation. Multiple tailnets is an **alpha** Tailscale
capability; one tailnet per match requires confirmed API access, quotas, and reliable cleanup.
**Fallback:** one managed game tailnet with lobby-specific tags + ACLs (changes the isolation
model).

## D3 — Rust monorepo for the control plane

**Status:** accepted

The control plane is a single Rust workspace: `spurfire-control` (library: Tailscale
tailnet/key lifecycle) and `spurfire-cli` (`spurfire-ctl` binary). A future backend service
builds on the same library.

## D4 — Game engine

**Status:** open — **blocking**

No game engine has been chosen. This blocks all data-plane (game client) work.

## D5 — Ranked result verification

**Status:** open

Peer-hosted matches need trustable results. Ranked results need co-signing or a lightweight
witness/replay-validation service. No mechanism selected yet.

## Open questions (from docs/design.md)

1. Visual setting (realistic Old West / arcade / fantasy / post-apoc)?
2. Win condition for main mode?
3. Lobby size target (provisional 6–16)?
4. Starting loadout vs found weapons?
5. Horse: persistent named companion vs per-match pick?
6. Friends-only party game vs public ranked?
7. Confirmed Tailnet Create API access?
