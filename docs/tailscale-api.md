# Tailscale API research for lobby provisioning

Latest verification: 2026-07-16 UTC. API origin is redacted; `.env` sets `TS_API_BASE` to a root ending in `/api/v2`, so paths below are relative to `/api/v2`. OAuth credentials, bearer tokens, child OAuth credentials, and auth keys are never recorded here.

The latest organization-tailnet probe created exactly one disposable resource and deleted it in the same guarded run. Its display name, stable ID, and DNS name are intentionally redacted; a follow-up organization listing confirmed the exact stable ID was absent. The resource was already deleted before this implementation work began, and no additional live mutation was performed for the implementation.

## Verified organization authentication and listing

Organization token exchange succeeded:

```http
POST /api/v2/oauth/token
Accept: application/json
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials&client_id=<redacted>&client_secret=<redacted>
```

The bearer token was used for the verified organization collection:

```http
GET /api/v2/organizations/-/tailnets
Authorization: Bearer <redacted-organization-token>

200
{"tailnets":[...]}
```

The response contained a non-empty organization inventory at probe time. The exact count and entries are intentionally omitted; probe tooling suppresses entries by default rather than printing the organization inventory.

## Verified child-tailnet creation

The correct create route is the organization collection, not the previously guessed top-level `/tailnet` or `/tailnets` collections:

```http
POST /api/v2/organizations/-/tailnets
Authorization: Bearer <redacted-organization-token>
Content-Type: application/json

{"displayName":"<generated-probe-name>"}
```

The call returned success and a typed object containing these fields:

```json
{
  "id": "<stable-tailnet-id>",
  "dnsName": "<child-dns-name>",
  "displayName": "<generated-probe-name>",
  "oauthClient": {
    "id": "<redacted-child-oauth-id>",
    "secret": "<redacted-one-time-child-secret>"
  }
}
```

`oauthClient.secret` is shown once. `spurfire-control` decodes directly into typed fields and never retains the full create JSON as a generic value. Child OAuth ID, secret, and cached bearer token are redacted in diagnostics and zeroized on drop where practical.

The verified `displayName` grammar used by the reference tooling is `^[A-Za-z0-9' -]{1,50}$`. Spurfire generates an ASCII `spurfire-<lobby-uuid>` value instead of forwarding user-controlled lobby text.

## Verified child token and deletion

The returned child OAuth pair minted a child-scoped token:

```http
POST /api/v2/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials&client_id=<redacted-child-id>&client_secret=<redacted-child-secret>

200
{"access_token":"<redacted-child-token>", ...}
```

The organization token was not used for deletion. The child token deleted the child by DNS selector:

```http
DELETE /api/v2/tailnet/<redacted-child-dns-name>
Authorization: Bearer <redacted-child-token>

200
```

A follow-up `GET /api/v2/organizations/-/tailnets` no longer contained the exact stored stable ID. Spurfire treats a delete-time 404 as idempotent success.

## Earlier route probes, retained as negative evidence

Earlier probes against guessed collection routes returned 404:

```http
GET  /api/v2/tailnet
GET  /api/v2/tailnets
POST /api/v2/tailnet   {"name":"spurfire-probe-api-research"}
POST /api/v2/tailnets  {"name":"spurfire-probe-api-research"}
```

Those results only prove that the guessed top-level collection routes do not exist. They do **not** contradict the now-verified organization route and must not be used to report `TailnetPerLobby` as `unavailable_api_404`.

An initial diagnostic also accidentally appended `/api/v2` to a base that already contained it. Those doubled-base requests returned 404 and remain irrelevant to capability reporting.

## Shared-tailnet scope evidence

The same historical organization OAuth client was denied the shared-tailnet operations Spurfire needs:

```http
POST /api/v2/tailnet/-/keys
{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":true,"preauthorized":true,"tags":["tag:spurfire-probe"]}}},"expirySeconds":300}
# 403 actor lacks permission

GET /api/v2/tailnet/-/keys
# 403

GET /api/v2/tailnet/-/devices
# 403

GET /api/v2/tailnet/-/acl
# 403
```

`GET /api/v2/tailnet/-/settings` returned 200 with `{}`. A synthetic device-delete ID reached the recognized route but returned 404. These facts do not establish key issuance, ACL/tag ownership, device discovery, or cleanup permission in the shared tailnet.

## Capability verdict

| Capability | Latest status | Evidence |
|---|---:|---|
| Organization token exchange | Verified | `POST /oauth/token` succeeded |
| Organization list | Verified | `GET /organizations/-/tailnets`; inventory details intentionally omitted |
| API-only child create | Verified | `POST /organizations/-/tailnets` with `{displayName}` created exactly one probe |
| Child token exchange | Verified | Returned child OAuth pair minted a child-scoped token |
| Child tailnet delete | Verified | `DELETE /tailnet/{dnsName}` with child token; stable ID absent afterward |
| Child one-use auth-key mint | Implemented/mock-tested | No new live mutation was allowed in this implementation workflow |
| Shared auth-key/device/ACL scopes | Blocked in historical probe | Required reads/mutation returned 403 |

**API fact:** organization-tailnet creation is available and is independent of shared-tailnet key/device/ACL scopes. `/v1/capabilities` reports those dimensions separately.

**Production-readiness verdict:** the verified create/token/delete lifecycle is enough for a guarded prototype, not production. The server currently keeps each one-time child OAuth pair only in a provider-owned in-memory vault keyed by public lobby ID. A restart deliberately fails closed with `child_secret_unavailable_manual_remediation`; durable state never contains the secret. Production requires an encrypted secret manager plus live end-to-end verification of child-scoped one-use key issuance and cleanup/reconciliation under failure.
