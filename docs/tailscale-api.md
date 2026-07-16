# Tailscale API research for lobby provisioning

Probe time: 2026-07-16 UTC. API origin is redacted; `.env` sets `TS_API_BASE` to an origin whose path already ends in `/api/v2`. All requests below therefore show paths beginning at `/api/v2`. Credentials, bearer tokens, client IDs, client secrets, and generated auth-key material are redacted. No resources were created by these probes, so no cleanup was required.

## Authentication

Exact redacted request shape:

```http
POST /api/v2/oauth/token
Accept: application/json
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials&client_id=<redacted-client-id>&client_secret=<redacted-client-secret>
```

Response:

```http
200
{"access_token":"<redacted-token>","token_type":"Bearer","expires_in":3600,"scope":"tailnets"}
```

The token lasts one hour. A fresh token must be fetched after expiry; the scripts do not persist it.

Unless stated otherwise, every JSON API probe below used these exact headers:

```http
Accept: application/json
Authorization: Bearer <redacted-token>
Content-Type: application/json   # only when a JSON body is present
```

## Tailnet creation (multiple-tailnets alpha)

The plausible collection paths do not exist for this client/API deployment. Each response body was exactly `{"message":"404 page not found"}`.

```http
GET /api/v2/tailnet
# 404 {"message":"404 page not found"}

GET /api/v2/tailnets
# 404 {"message":"404 page not found"}

POST /api/v2/tailnet
{"name":"spurfire-probe-api-research"}
# 404 {"message":"404 page not found"}

POST /api/v2/tailnets
{"name":"spurfire-probe-api-research"}
# 404 {"message":"404 page not found"}

POST /api/v2/tailnet
{"tailnet":"spurfire-probe-api-research"}
# 404 {"message":"404 page not found"}

POST /api/v2/tailnets
{"tailnet":"spurfire-probe-api-research"}
# 404 {"message":"404 page not found"}
```

The singular instance path was also tested:

```http
GET /api/v2/tailnet/-
# 405 <empty body>
```

No create call succeeded. Consequently there was no new tailnet in which to mint a key and no tailnet to delete. The expected script-side deletion shape is `DELETE /api/v2/tailnet/{url-encoded-tailnet-name}`, but it could not safely be validated without a disposable tailnet.

## Auth keys

Exact requested lobby-key probe:

```http
POST /api/v2/tailnet/-/keys
Content-Type: application/json

{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":true,"preauthorized":true,"tags":["tag:spurfire-probe"]}}},"expirySeconds":300}
```

Response:

```http
403
{"message":"calling actor does not have enough permissions to perform this function"}
```

Key listing is denied identically:

```http
GET /api/v2/tailnet/-/keys
# 403 {"message":"calling actor does not have enough permissions to perform this function"}
```

A deliberately invalid key ID established the delete route shape without touching a real key:

```http
DELETE /api/v2/tailnet/-/keys/spurfire-probe-nonexistent
# 400 {"message":"invalid key ID"}
```

No key was minted, and no key material was exposed or persisted.

## Devices

```http
GET /api/v2/tailnet/-/devices
# 403 {"message":"calling actor does not have enough permissions to perform this function"}

DELETE /api/v2/device/spurfire-probe-nonexistent
# 404 {"message":"no manageable device matching this ID found"}
```

The delete probe confirms the route/request shape but not deletion permission: the synthetic ID cannot identify a manageable device. Device IDs cannot be discovered with this OAuth client because listing is denied.

## ACL and settings

```http
GET /api/v2/tailnet/-/acl
# 403 {"message":"calling actor does not have enough permissions to perform this function"}

GET /api/v2/tailnet/-/settings
# 200 {}
```

The empty settings object is the only successful tailnet read tested. It does not provide the operations needed for lobby isolation.

## Other provisioning-adjacent probes

```http
GET /api/v2/tailnet/-/dns/nameservers
# 403 {"message":"calling actor does not have enough permissions to perform this function"}

GET /api/v2/tailnet/-/dns/preferences
# 403 {"message":"calling actor does not have enough permissions to perform this function"}

GET /api/v2/tailnet/-/routes
# 404 404 page not found\n
GET /api/v2/tailnet/-/webhooks
# 404 {"message":"not found"}
```

DNS administration is permission-gated. The guessed tailnet-level routes and webhooks collection paths do not exist.

## Base-path correction probes

`TS_API_BASE` already includes `/api/v2`. An initial diagnostic intentionally confirmed that adding another `/api/v2` is wrong. These were real probes and are retained for completeness. Every request below returned `404 {"message":"404 page not found"}`:

```http
GET /api/v2/api/v2/tailnet
GET /api/v2/api/v2/tailnets
GET /api/v2/api/v2/tailnet/-
GET /api/v2/api/v2/tailnet/-/settings
GET /api/v2/api/v2/tailnet/-/acl
GET /api/v2/api/v2/tailnet/-/devices
POST /api/v2/api/v2/tailnet
{"name":"spurfire-probe-api-research"}
```

## Verdict

| Capability / endpoint | Status | Evidence |
|---|---:|---|
| OAuth token `POST /oauth/token` | 200 | Bearer token, `scope=tailnets`, `expires_in=3600` |
| Create tailnet `POST /tailnet` | 404 | Route absent for both `name` and `tailnet` bodies |
| Create tailnet `POST /tailnets` | 404 | Route absent for both `name` and `tailnet` bodies |
| Tailnet settings `GET /tailnet/-/settings` | 200 | Empty object `{}` |
| Create auth key `POST /tailnet/-/keys` | 403 | Actor lacks permission |
| List auth keys `GET /tailnet/-/keys` | 403 | Actor lacks permission |
| List devices `GET /tailnet/-/devices` | 403 | Actor lacks permission |
| Delete device `DELETE /device/{id}` | 404 with synthetic ID | Route recognized; no manageable matching device |
| Read ACL `GET /tailnet/-/acl` | 403 | Actor lacks permission |
| Read DNS configuration | 403 | Actor lacks permission |

**Provisioning verdict today:** this OAuth client supports **neither** tailnet-per-lobby nor shared-tailnet-plus-tags end to end. Tailnet-per-lobby is blocked because no tested alpha create endpoint exists. The shared-tailnet fallback is also blocked because the client cannot mint tagged ephemeral auth keys, read/update ACL policy, or list devices. If permissions are expanded, shared-tailnet-plus-tags is the nearer viable mode because its standard API routes exist; isolation still requires ACL grants/tag ownership and successful key/device cleanup tests before production use.
