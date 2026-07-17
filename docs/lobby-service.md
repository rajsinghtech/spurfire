# Lobby service

`spurfire-server` is the prototype HTTP control service. It creates lobby records, mints narrow Tailscale enrollment credentials, publishes deterministic authority elections, and coordinates teardown. Gameplay remains peer-to-peer; the service is not in the gameplay data path.

## Start safely

Dry-run is the default development workflow and requires no Tailscale credentials:

```sh
cargo run -p spurfire-server -- --dry-run --bind 127.0.0.1:8080
# or
just serve-dry
```

Dry-run guarantees that no auth-key, ACL, or device mutation reaches Tailscale. Simulated join responses use `DRY_RUN_NO_KEY`, all dry-run responses contain `"dry_run": true` and `"planned_actions": [...]`, and dry-run lobbies expire in at most five minutes.

Real mode loads Tailscale credentials and durable, non-secret state:

```sh
SPURFIRE_BIND_ADDR=127.0.0.1:8080 \
SPURFIRE_STATE_PATH=.spurfire/server-state.json \
cargo run -p spurfire-server
```

Real startup performs bounded, read-only capability probes. The current tested OAuth client is blocked, so use dry-run unless the permission requirements below have been reverified.

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
| `GET /v1/capabilities` | Cached booleans for token/settings, auth keys, devices, and ACL access; shared-tailnet is `available` or `blocked_scopes`, and tailnet-per-lobby is always `unavailable_api_404`. |
| `POST /v1/lobbies` | Creates a lobby in `PROVISIONING`. `max_players` defaults to 8 and is capped at 16. The response never waits on Tailscale; cached evidence advances the stored record to `FORMING` or fail-closes it to `FAILED`. Same idempotency key/body/actor replays with 200; a mismatch returns 409. |
| `GET /v1/lobbies/{lobby_id}` | Pollable secret-free snapshot, including roster, TTLs, authority summary, and aggregate `cleanup_pending`. |
| `POST /v1/lobbies/{lobby_id}/join` | In `FORMING`/`READY`, validates wire major, rate limits mint attempts, and issues one ephemeral, preauthorized, non-reusable, tagged key with a 300-second expiry. The key appears only in the first 201 response; replay returns a receipt without key material. |
| `POST /v1/lobbies/{lobby_id}/leave` | Idempotently removes the actor from the roster and revokes its unconsumed credential receipt. Per-player device deletion remains blocked by the lack of a safe player-to-device mapping; terminal lobby cleanup deletes by lobby tag. |
| `POST /v1/lobbies/{lobby_id}/measurements` | Last-write-wins integer connectivity report in `FORMING`, `READY`, or `IN_MATCH`. Unknown additive JSON fields are ignored. |
| `GET /v1/lobbies/{lobby_id}/authority` | Returns `election_v1`, scores, winner, SHA-256 `input_hash`, and the exact canonical input (evaluation time, wire context, roster size, and player-sorted measurement rows) needed for peer recomputation. |
| `POST /v1/lobbies/{lobby_id}/elect-authority` | Compatibility alias for the authority read/evaluation path. |
| `POST /v1/lobbies/{lobby_id}/start` | Creator-only. Requires `READY`, at least two players, fresh measurements, a winner, compatible wire majors, and one authority formula. Fixes `map_seed` and enters `STARTING`. |
| `POST /v1/lobbies/{lobby_id}/heartbeat` | Current authority only. The first matching heartbeat enters `IN_MATCH`; later heartbeats prevent migration. After two seconds of silence, a fresh measurement re-elects over a matrix excluding the silent winner. |
| `POST /v1/lobbies/{lobby_id}/results` | Last authority only. Performs shallow schema, roster, score, duration, and input-hash checks; returns 202/`CLOSING`, then runs teardown. Co-signers are recorded inputs, not ranked trust. |
| `DELETE /v1/lobbies/{lobby_id}` | Creator-only and idempotent. Revokes known unexpired auth-key receipts before listing/deleting lobby-tagged devices, then records `DESTROYED`. Capability failures set `cleanup_pending` and are retried. |

JSON bodies larger than 64 KiB return 413; missing or wrong JSON content type returns 415.

## State machine

- **PROVISIONING** — persisted first. Cached capability evidence moves it to `FORMING`; missing token/key/device/ACL evidence moves it to `FAILED` with a machine reason.
- **FORMING** — accepts joins and measurements. It becomes `READY` once at least two roster members all have measurements fresher than 60 seconds. Ten minutes without join/leave/measurement activity causes `EXPIRED`.
- **READY** — authority is published. A roster or measurement change returns to `FORMING`; stale measurements also return it to `FORMING` on the next read. Creator start enters `STARTING`.
- **STARTING** — roster frozen and map seed fixed. The first authority heartbeat enters `IN_MATCH`; 120 seconds without one causes `FAILED`/`start_timeout`.
- **IN_MATCH** — gameplay is peer-hosted. Authority heartbeats and measurements support deterministic migration. Results enter `CLOSING`. The 60-minute absolute TTL causes `FAILED`.
- **CLOSING** — key revocation then lobby-tagged device cleanup is attempted in fixed order. Teardown completion enters `DESTROYED`; denied work remains `cleanup_pending` without blocking the terminal response.
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
| `SPURFIRE_PROVISIONING_MODE` | `shared_tailnet` | `shared_tailnet` or `dry_run`; dry mode also requires `SPURFIRE_DRY_RUN=1`. |
| `SPURFIRE_SHARED_TAILNET` | `-` | Tailscale tailnet selector. |
| `SPURFIRE_STATE_PATH` | `.spurfire/server-state.json` | Single-process JSON state file used only in real mode. It stores receipts and cleanup state, never auth-key material. |
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

## Tailscale permission blockers

The redacted probes in [tailscale-api.md](tailscale-api.md) remain authoritative:

- tested tailnet-create routes returned 404, so tailnet-per-lobby is unavailable;
- auth-key create/list, device list, and ACL reads returned 403 for the tested OAuth client;
- shared-tailnet isolation is not production-usable until key issuance, ACL/tag ownership, device discovery/deletion, and cleanup retries pass live integration tests;
- read-only capability probes are conservative scope evidence, not proof that ACL policy is correctly isolated;
- safe per-player device deletion needs a trustworthy credential/device association. Until then leave revokes the key, while terminal cleanup deletes devices by the lobby tag.

The service fails closed: a real lobby never becomes joinable when cached evidence says these capabilities are blocked.

## Trust boundaries and production limits

- OAuth client credentials and bearer tokens stay inside `spurfire-control`/`spurfire-server`; they are absent from responses, durable records, and diagnostic formatting.
- The only client-visible secret is the first join response's short-lived auth key. Only its receipt ID and expiry are persisted.
- Tailnet membership grants data-plane connectivity, not Tailscale API access. There is no arbitrary provider proxy route.
- `player_id` and creator headers are client assertions, not public identity or authentication.
- Result verification is intentionally shallow. Ranked co-signing, witnesses, or replay validation remain a blocking design question.
- The JSON store is durable for one process but is not a multi-node transactional database. High availability, distributed idempotency, and startup reconciliation against upstream resources remain production work.
