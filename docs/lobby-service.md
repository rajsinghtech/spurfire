# Lobby service

`spurfire-server` is the prototype HTTP control service. It creates lobby records, mints narrow Tailscale enrollment credentials, publishes deterministic authority elections, and coordinates teardown. Gameplay remains peer-to-peer; the service is not in the gameplay data path.

## Start safely

Dry-run is the default development workflow and requires no Tailscale credentials:

```sh
cargo run -p spurfire-server -- --dry-run --bind 127.0.0.1:8080
# or
just serve-dry
```

Dry-run guarantees that no organization-tailnet create, auth-key, ACL, device, or tailnet-delete mutation reaches Tailscale. It also creates no child-secret vault entry. Simulated join responses use `DRY_RUN_NO_KEY`, all dry-run responses contain `"dry_run": true` and `"planned_actions": [...]`, and dry-run lobbies expire in at most five minutes.

Real mode loads Tailscale credentials and durable, non-secret state:

```sh
SPURFIRE_BIND_ADDR=127.0.0.1:8080 \
SPURFIRE_STATE_PATH=.spurfire/server-state.json \
cargo run -p spurfire-server
```

Real startup performs bounded, read-only capability probes. Organization-tailnet listing is probed independently from shared-tailnet key/device/ACL scopes. The organization create/token/delete flow is verified, while shared-tailnet scopes remain blocked for the historical client; use dry-run unless the selected mode's requirements below have been reverified.

## Common headers

- `Content-Type: application/json` on JSON requests.
- `Idempotency-Key: <opaque value>` on create, join, start, and results.
- `X-Spurfire-Player-Id: <UUIDv4>` supplies the prototype actor identity on mutations. It must equal the body player/creator/submitter where applicable.
- `X-Spurfire-Dry-Run: 1` makes a new create request simulated. It is rejected with `409 dry_run_mode_mismatch` if used to mutate an existing real lobby, so a simulation cannot alter live state.

Player identity is client-asserted and development-grade. These headers are not authentication.

## Routes

| Method and path | Behavior |
|---|---|
| `GET /healthz` | Process health and cached provisioning readiness. A blocked provider reports `degraded` without exposing probe bodies. |
| `GET /v1/capabilities` | Cached booleans for token, organization-tailnet listing, and shared key/device/ACL access. Tailnet-per-lobby is `available` or `blocked_organization_access`; shared-tailnet is independently `available` or `blocked_scopes`. |
| `POST /v1/lobbies` | Persists `PROVISIONING` before provider work. In tailnet-per-lobby mode it then creates one API-only child and keeps the one-time child OAuth pair only in memory. `max_players` defaults to 8 and is capped at 16. Same idempotency key/body/actor replays without a second create; a mismatch returns 409. |
| `GET /v1/lobbies/{lobby_id}` | Pollable secret-free snapshot, including roster, TTLs, authority summary, and aggregate `cleanup_pending`. |
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
| `TS_CLIENT_ID` / `TS_CLIENT_SECRET` | none | Server-only OAuth credentials required in real mode. |

An absent `.env` is allowed; a malformed or unreadable `.env` fails startup. `.env` and `.spurfire/` are gitignored.

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
- child one-use auth-key issuance is implemented and mock-tested but was not live-mutated again in this workflow;
- read-only capability probes are conservative scope evidence, not production isolation proof;
- safe per-player device deletion in shared mode still needs a trustworthy credential/device association. Leave revokes the key; terminal shared cleanup uses the lobby tag.

The service fails closed for both modes. In particular, child OAuth material is intentionally process-local. After restart, any retained child-backed lobby becomes `FAILED`, sets `cleanup_pending`, and reports `child_secret_unavailable_manual_remediation` rather than trying the organization token or exposing an identifier. Production must replace this prototype vault with an encrypted secret manager and startup reconciliation.

## Trust boundaries and production limits

- Organization OAuth credentials, child OAuth pairs, and bearer tokens stay inside `spurfire-control`/`spurfire-server`; they are absent from public responses, durable records, logs, and diagnostic formatting.
- A child OAuth pair is inserted into a provider-owned in-memory vault keyed by public `lobby_id` immediately after typed create decoding. Successful child-tailnet deletion evicts it; drop zeroizes secret allocations where practical.
- The only client-visible secret is the first join response's short-lived auth key. Only its receipt ID and expiry are persisted.
- Tailnet membership grants data-plane connectivity, not Tailscale API access. There is no arbitrary provider proxy route.
- `player_id` and creator headers are client assertions, not public identity or authentication.
- Result verification is intentionally shallow. Ranked co-signing, witnesses, or replay validation remain a blocking design question.
- The JSON store is durable for one process but is not a multi-node transactional database. High availability, distributed idempotency, and startup reconciliation against upstream resources remain production work.
