# Lobby service

`spurfire-server` is the prototype HTTP control service. It creates lobby records, mints narrow Tailscale enrollment credentials, publishes deterministic authority elections, and coordinates teardown. Gameplay remains peer-to-peer; the service is not in the gameplay data path and, by D9, never joins a lobby tailnet.

> **Activation status:** public real provisioning is closed. Safe groundwork adds an independent default-off real-mutation switch and a creator-capability network view, but legacy lobby routes still use client assertions, capability migration is incomplete, and child credentials remain process-local. The chart rejects enabling real mutations. Ottawa is public dry-run only. The accepted target contract and operator runbook are in [control-plane-network-view.md](control-plane-network-view.md).

## Start safely

Dry-run is the default development workflow and requires no Tailscale credentials:

```sh
cargo run -p spurfire-server -- --dry-run --bind 127.0.0.1:8080
# or
just serve-dry
```

Dry-run guarantees that no organization-tailnet create, auth-key, ACL, device, or tailnet-delete mutation reaches Tailscale. It also creates no child-secret vault entry. Simulated join responses use `DRY_RUN_NO_KEY`, all dry-run responses contain `"dry_run": true` and `"planned_actions": [...]`, and dry-run lobbies expire in at most five minutes.

The current prototype can load Tailscale credentials and durable, non-secret state for a private, deliberately supervised real-mode probe:

```sh
SPURFIRE_BIND_ADDR=127.0.0.1:8080 \
SPURFIRE_STATE_PATH=.spurfire/server-state.json \
SPURFIRE_REAL_MUTATIONS_ENABLED=0 \
cargo run -p spurfire-server
```

Do not use that command for a public deployment. Safe-groundwork revisions recognize `SPURFIRE_REAL_MUTATIONS_ENABLED`, default it to `0`, and reject real create/mint/delete without it independently of credentials and provisioning mode. Older revisions without that switch are unsuitable for any exposed real configuration. The shipped chart keeps the switch at `0` and deliberately rejects `true`; no current deployment is authorized to enable it.

Current real startup performs bounded, read-only capability probes. Organization-tailnet listing is probed independently from shared-tailnet key/device/ACL scopes. Organization create/token, child-scoped one-use key issuance, and deletion have been live-proven in disposable lifecycles, while shared-tailnet scopes remain blocked for the historical client. Public use remains dry-run until the complete activation checklist passes.

## Common headers

Current migration surface:

- `Content-Type: application/json` on JSON requests.
- `Idempotency-Key: <opaque value>` on create, join, start, and results.
- `X-Spurfire-Player-Id: <UUIDv4>` supplies the prototype actor identity on mutations. It must equal the body player/creator/submitter where applicable.
- `X-Spurfire-Dry-Run: 1` makes a new create request simulated. It is rejected with `409 dry_run_mode_mismatch` if used to mutate an existing real lobby, so a simulation cannot alter live state.

Player identity is client-asserted and development-grade. These headers are not authentication. Before real activation, every lobby-specific read and mutation must instead require a scoped exact-lobby capability transported as `Authorization: Spurfire-Capability <token>` over TLS. Any player ID remains only an identifier that must match the capability subject.

## Current routes (migration surface)

| Method and path | Behavior |
|---|---|
| `GET /healthz` | Process health and cached provisioning readiness. A blocked provider reports `degraded` without exposing probe bodies. |
| `GET /v1/capabilities` | Cached booleans for token, organization-tailnet listing, and shared key/device/ACL access. Tailnet-per-lobby is `available` or `blocked_organization_access`; shared-tailnet is independently `available` or `blocked_scopes`. |
| `POST /v1/lobbies` | Persists `PROVISIONING` before provider work. In tailnet-per-lobby mode it then creates one API-only child and keeps the one-time child OAuth pair only in memory. `max_players` defaults to 8 and is capped at 16. Same idempotency key/body/actor replays without a second create; a mismatch returns 409. |
| `GET /v1/lobbies/{lobby_id}` | Currently unauthenticated, secret-free snapshot, including roster, TTLs, authority summary, and aggregate `cleanup_pending`. Secret-free is not authorization-safe: it must not expose real lobbies. The maintained read path can also advance expiry/cleanup/provider work, so it is not the target inspector. It never returns the tailnet FQDN. |
| `POST /v1/lobbies/{lobby_id}/join` | In `FORMING`/`READY`, validates wire major, rate limits mint attempts, and issues one ephemeral, preauthorized, non-reusable key with a 300-second expiry. Shared mode uses the lobby tag; child mode uses the child scope. The key appears only in the first 201 response; replay returns a receipt without key material. |
| `POST /v1/lobbies/{lobby_id}/leave` | Idempotently removes the actor and revokes its unconsumed credential receipt. Per-player device deletion lacks a safe mapping; terminal shared cleanup uses the lobby tag, while terminal child cleanup deletes the child tailnet. |
| `POST /v1/lobbies/{lobby_id}/measurements` | Last-write-wins integer connectivity report in `FORMING`, `READY`, or `IN_MATCH`. Unknown additive JSON fields are ignored. |
| `GET /v1/lobbies/{lobby_id}/authority` | Returns `election_v1`, scores, winner, SHA-256 `input_hash`, and the exact canonical input (evaluation time, wire context, roster size, and player-sorted measurement rows) needed for peer recomputation. |
| `POST /v1/lobbies/{lobby_id}/elect-authority` | Compatibility alias for the authority read/evaluation path. |
| `POST /v1/lobbies/{lobby_id}/start` | Creator-only. Requires `READY`, at least two players, fresh measurements, a winner, compatible wire majors, and one authority formula. Fixes `map_seed` and enters `STARTING`. |
| `POST /v1/lobbies/{lobby_id}/heartbeat` | Current authority only. The first matching heartbeat enters `IN_MATCH`; later heartbeats prevent migration. After two seconds of silence, a fresh measurement re-elects over a matrix excluding the silent winner. |
| `POST /v1/lobbies/{lobby_id}/results` | Last authority only. Performs shallow schema, roster, score, duration, and input-hash checks; returns 202/`CLOSING`, then runs teardown. Co-signers are recorded inputs, not ranked trust. |
| `DELETE /v1/lobbies/{lobby_id}` | Creator-only and idempotent. Shared mode revokes known unexpired keys before tagged-device cleanup. Child mode deletes the entire child tailnet with its child token and evicts the in-memory OAuth material only after success. Capability or vault failures set `cleanup_pending`. |

JSON bodies larger than 64 KiB return 413; missing or wrong JSON content type returns 415.

Planned (M6-complete, not yet implemented): a privacy-safe aggregate stats surface for the public landing page. It contains no lobby IDs, FQDNs, private addresses, join material, roster rows, provider IDs, or per-player detail. Real network aggregates are suppressed for cohorts smaller than three; with the alpha one-real-lobby quota they are always suppressed.

## Protected selected-lobby target

These routes are the accepted contract. Safe groundwork implements the schema, creator-capability cached read, browser/CLI clients, and default-off deployment plumbing; invitation/participant/report/operator routes and capability migration across every legacy route remain activation work. None is authorization for public real mode:

| Method and path | Required identity and behavior |
|---|---|
| `GET /inspect` | Static no-store shell only. Separate lobby-ID/capability inputs, capability held in memory, no embedded lobby data, search, or list. |
| `POST /v1/lobbies` | Dry-run follows the safe existing policy. Real additionally consumes one-use `lobby.create_real`, server-selects `tailnet_per_lobby`, acquires the singleton lease before provider work, and returns the creator capability once. |
| `POST /v1/lobbies/{id}/invitations` | Creator `lobby.invite`; returns one one-use invitation expiring within ten minutes. |
| `POST /v1/lobbies/{id}/join` | Consumes the invitation; first success returns one-use enrollment key plus participant capability. Replay returns receipts only. |
| `GET /v1/lobbies/{id}` | Creator, participant, or private operator. During migration, anonymous access is allowed only for a forced-dry-run reduced projection. |
| `GET /v1/lobbies/{id}/network` | Creator, participant, or operator. Pure cached read of one `LobbyNetworkView`; no provider I/O, cleanup, election update, or store mutation. |
| `POST /v1/lobbies/{id}/network/reports` | `lobby.report` participant capability bound to reporter/lobby/network generation; bounded, directional, rate-limited, inspection-only. |
| `GET /v1/operator/lobbies` | Private listener only; minimal summaries for selection. |
| `GET /v1/operator/lobbies/{id}/network` | Private listener only; selected-lobby view with operator-only identity/reconciliation extension. |

Absent, wrong-lobby, wrong-generation, expired, revoked, and insufficient-scope requests use the same `404 lobby_not_found` body. Sensitive responses carry `Cache-Control: private, no-store`, `Vary: Authorization`, `Referrer-Policy: no-referrer`, and `X-Content-Type-Options: nosniff`.

The selected view uses a complete provider-returned `tailnet_dns_name`/FQDN, such as `tail9a1c23.ts.net`, for authorized audiences only. The TLD is `.net`; `.ts.net` is not a TLD. Dry-run returns a null/not-applicable FQDN and never serializes the current internal `dry-run.invalid` placeholder. See [the complete audience, fact, report, and freshness contract](control-plane-network-view.md#network-view-contract).

## State machine

- **PROVISIONING** — persisted first. Shared mode evaluates cached key/device/ACL evidence. Tailnet-per-lobby performs the bounded organization create after persistence; success moves to `FORMING`, while denied or ambiguous work moves to `FAILED` with a safe machine reason.
- **FORMING** — accepts joins and measurements. It becomes `READY` once at least two roster members all have measurements fresher than 60 seconds. Ten minutes without join/leave/measurement activity causes `EXPIRED`.
- **READY** — authority is published. A roster or measurement change returns to `FORMING`; stale measurements also return it to `FORMING` on the next read. Creator start enters `STARTING`.
- **STARTING** — roster frozen and map seed fixed. The first authority heartbeat enters `IN_MATCH`; 120 seconds without one causes `FAILED`/`start_timeout`.
- **IN_MATCH** — gameplay is peer-hosted. Authority heartbeats and measurements support deterministic migration. Results enter `CLOSING`. The 60-minute absolute TTL causes `FAILED`.
- **CLOSING** — shared mode revokes keys then cleans lobby-tagged devices; child mode deletes the whole child tailnet. Teardown completion enters `DESTROYED`; denied work remains `cleanup_pending` without blocking the terminal response.
- **FAILED** — contains a mandatory machine reason and runs the same teardown. It remains queryable for 15 minutes. Explicit creator deletion can finalize its retained tombstone as `DESTROYED`.
- **EXPIRED** — idle/absolute expiry path with the same teardown and 15-minute debugging retention. Explicit creator deletion can finalize it.
- **DESTROYED** — rejects further mutations, except creator cleanup retry/idempotent delete. Tombstones and idempotency records are retained for 24 hours.

The lobby state machine does not prove provider-resource state. The protected design adds an independent network lifecycle from `SIMULATED`/`RESERVED` through `ACTIVE`, cleanup, and either `DEDICATED_ABSENT`, `SHARED_RESOURCES_CLEAN`, or `MANUAL_REMEDIATION`. In particular, `DESTROYED`, delete 200/404, or zero devices does not prove dedicated absence. Dedicated cleanup needs two parent listings at least five seconds apart with the exact stored stable ID absent, followed by verified encrypted child-secret erasure.

Alpha also adds one durable singleton lease across both real modes. Idempotency is resolved before capacity; a new request atomically writes `RESERVED` and acquires the lease before provider create. Ambiguous create/restart/poll/cleanup/vault state holds the lease. It releases only on definitive pre-resource `CREATE_REJECTED`, proven `DEDICATED_ABSENT`, or `SHARED_RESOURCES_CLEAN`.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `SPURFIRE_BIND_ADDR` | `127.0.0.1:8080` | Listen socket. Bind a non-loopback address only behind appropriate network controls. |
| `SPURFIRE_DRY_RUN` | `0` | `1` forces every created lobby into zero-mutation simulation. CLI `--dry-run` does the same. |
| `SPURFIRE_DRY_RUN_TTL_SECS` | `300` | Dry-run TTL, accepted range 1–300. Real lobbies always use the fixed 3,600-second absolute TTL. |
| `SPURFIRE_MAX_PLAYERS` | `16` | Deployment cap, never above the protocol cap of 16. |
| `SPURFIRE_PROVISIONING_MODE` | `shared_tailnet` | `shared_tailnet`, `tailnet_per_lobby`, or `dry_run`; dry mode also requires `SPURFIRE_DRY_RUN=1`. |
| `SPURFIRE_SHARED_TAILNET` | `-` | Tailscale tailnet selector. |
| `SPURFIRE_STATE_PATH` | `.spurfire/server-state.json` | Single-process JSON state used only in real mode. It stores non-secret tailnet selectors, receipts, and cleanup state—never auth keys, child OAuth credentials, or bearer tokens. |
| `TS_API_BASE` | none | Normally `https://api.tailscale.com/api/v2`. |
| `SPURFIRE_REAL_MUTATIONS_ENABLED` | `0` | Independent fail-closed gate in safe-groundwork revisions. The chart emits `0` and rejects `true`; enabling it remains activation-blocked. |
| `TS_CLIENT_ID` / `TS_CLIENT_SECRET` | none | Server-only organization OAuth credentials used by private activation-closed staging. |

An absent `.env` is allowed; a malformed or unreadable `.env` fails startup. `.env` and `.spurfire/` are gitignored. Credentials, `SPURFIRE_DRY_RUN=0`, and a real `SPURFIRE_PROVISIONING_MODE` are never sufficient without the independent switch; the switch is itself never sufficient without every remaining activation gate.

## Fake-value curl walkthrough

Start the server with `--dry-run`, then use only fake UUIDs:

```sh
BASE=http://127.0.0.1:8080
CREATOR=00000000-0000-4000-8000-000000000001
RIDER=00000000-0000-4000-8000-000000000002

curl -sS "$BASE/v1/capabilities"

curl -sS -X POST "$BASE/v1/lobbies" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: fake-create-1' \
  -H "X-Spurfire-Player-Id: $CREATOR" \
  -d '{"display_name":"Fake High Noon","max_players":8,"provisioning_mode":"dry_run"}'
# Copy the fake lobby_id from the response:
LOBBY=00000000-0000-4000-8000-000000000099

curl -sS -X POST "$BASE/v1/lobbies/$LOBBY/join" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: fake-join-1' \
  -H "X-Spurfire-Player-Id: $RIDER" \
  -d "{\"player_id\":\"$RIDER\",\"display_name\":\"Fake Rider\",\"client_wire_version\":\"1.0\",\"authority_formula_version\":\"election_v1\"}"
# auth_key is DRY_RUN_NO_KEY in this mode.

curl -sS "$BASE/v1/lobbies/$LOBBY"
curl -sS "$BASE/v1/lobbies/$LOBBY/authority"

curl -sS -X DELETE "$BASE/v1/lobbies/$LOBBY" \
  -H "X-Spurfire-Player-Id: $CREATOR"
```

A full start requires at least two joined players and one fresh report from each. A report example for a two-player roster is:

```sh
curl -sS -X POST "$BASE/v1/lobbies/$LOBBY/measurements" \
  -H 'Content-Type: application/json' \
  -H "X-Spurfire-Player-Id: $RIDER" \
  -d "{\"player_id\":\"$RIDER\",\"route_summary\":{\"direct_count\":1,\"peer_relay_count\":0,\"derp_count\":0},\"rtt_ms_median\":25,\"rtt_ms_worst\":40,\"jitter_ms\":3,\"loss_pct_milli\":0,\"upload_mbps_sustained\":20,\"device_perf_score\":800,\"observed_peer_count\":1}"
```

## Tailscale capability and readiness boundaries

The redacted probes in [tailscale-api.md](tailscale-api.md) remain authoritative:

- `GET/POST /organizations/-/tailnets`, child token exchange, and child-scoped tailnet deletion are verified;
- the older top-level `/tailnet` and `/tailnets` collection 404s were wrong-route evidence, not an API-unavailability verdict;
- shared auth-key create/list, device list, and ACL reads returned 403 for the historical organization OAuth client, independently of organization-tailnet access;
- child-scoped one-use auth-key issuance is implemented and live-proven by the later managed disposable P2P lifecycle; the dated probe document still distinguishes what its own earlier guarded run mutated;
- read-only capability probes are conservative scope evidence, not production isolation proof;
- safe per-player device deletion in shared mode still needs a trustworthy credential/device association. Leave revokes the key; terminal shared cleanup uses the lobby tag.

The service fails closed for both modes. In particular, child OAuth material is intentionally process-local. After restart, any retained child-backed lobby becomes `FAILED`, sets `cleanup_pending`, and reports `child_secret_unavailable_manual_remediation` rather than trying the organization token or exposing an identifier. Production must replace this prototype vault with a dynamic encrypted secret manager and mutation-closed startup reconciliation across store, vault, singleton lease, and exact upstream stable IDs.

The current `PreparedNetwork`/durable record keeps the FQDN selector but discards the provider stable ID. Activation work must retain stable ID, canonical FQDN, and network generation as one non-secret identity tuple. Deletion refuses every mismatch and never selects by display name.

## Trust boundaries and production limits

- Organization OAuth credentials, child OAuth pairs, and bearer tokens stay inside `spurfire-control`/`spurfire-server`; they are absent from public responses, durable records, logs, and diagnostic formatting.
- A child OAuth pair is inserted into a provider-owned in-memory vault keyed by public `lobby_id` immediately after typed create decoding. Successful child-tailnet deletion evicts it; drop zeroizes secret allocations where practical.
- The only client-visible secret is the first join response's short-lived auth key. Only its receipt ID and expiry are persisted.
- Tailnet membership grants data-plane connectivity, not Tailscale API access. There is no arbitrary provider proxy route.
- `player_id` and creator headers are client assertions, not public identity or authentication. Exact-lobby capabilities must replace them as authorization before real activation.
- A tailnet FQDN and private tailnet address are topology metadata rather than credentials, but member/operator audience and retention rules still protect them. Provider stable IDs and reconciliation detail are operator-only.
- Inspection facts distinguish control-authoritative, provider-observed, participant-reported, derived, stale, and unknown values. Provider `lastSeen` is not an online boolean; participant reports are not gameplay truth.
- Result verification is intentionally shallow. Ranked co-signing, witnesses, or replay validation remain a blocking design question.
- The JSON store is durable for one process but is not a multi-node transactional database. High availability, fencing, encrypted dynamic custody, and startup reconciliation against exact upstream resources remain production work.
- The exact activation checklist, reconciliation matrix, cleanup proof, and operator response steps are normative in [control-plane-network-view.md](control-plane-network-view.md).
