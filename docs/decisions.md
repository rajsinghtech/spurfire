# Spurfire Decision Log (ADR-lite)

Numbered decisions with status: **accepted** | **provisional** | **open**.

## D1 — Peer-hosted authority

**Status:** accepted

Gameplay uses peer-hosted authoritative networking, not distributed simulation. All gameplay
machines are peers on a lobby tailnet; one player is elected match authority and validates
movement, shots, damage, score, and events. No permanent dedicated gameplay server. Peers
keep recent state snapshots for authority migration.

## D2 — Tailnet-per-lobby with shared-tailnet fallback

**Status:** provisional — organization lifecycle verified; production secret custody unresolved

Tailnet-per-lobby remains the preferred isolation model. The organization-scoped
`GET/POST /organizations/-/tailnets` route is now verified, as are child OAuth token exchange and
child-scoped `DELETE /tailnet/{dnsName}`. Earlier 404s came from guessed top-level collection
routes and are retained only as negative route evidence. `TailnetPerLobby` is implemented in the
server with a provider-owned in-memory child-secret vault and fail-closed restart behavior.

**Fallback:** one managed game tailnet with lobby-specific tags + ACLs. `SharedTailnet` remains
implemented, but historical probes received 403 for auth-key, device-list, and ACL operations.
Capability reporting keeps organization-tailnet access independent from these shared scopes.

Keep this decision provisional until child one-use key issuance is live-verified and the in-memory
vault is replaced by an encrypted secret manager with reconciliation, rotation, audit, and cleanup
recovery; see `docs/tailscale-api.md`.

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
7. Which encrypted secret manager and startup reconciliation design will safely retain one-time child OAuth credentials in production?
