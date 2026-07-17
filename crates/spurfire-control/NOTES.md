# Tailscale API implementation notes

Latest verified evidence is documented in `docs/tailscale-api.md`. Credentials, bearer tokens, child OAuth values, and auth keys are not recorded here.

## Organization tailnets

The correct API-only child-tailnet collection is:

```text
GET  /organizations/-/tailnets
POST /organizations/-/tailnets  {"displayName":"spurfire-probe-*"}
```

The create response includes typed `id`, `dnsName`, `displayName`, and one-time `oauthClient.{id,secret}` fields. `spurfire-control` must decode that shape directly. Never restore the former generic `raw: serde_json::Value` field: it would retain the one-time child secret.

The returned child OAuth pair exchanges at `POST /oauth/token`. Tailnet deletion requires that child scope:

```text
DELETE /tailnet/{dnsName}
```

A successful guarded probe created exactly one child, deleted it with the child token, and confirmed its stable ID was absent from the organization listing. No further live mutation was performed during implementation.

## Historical wrong-route evidence

`POST /tailnet` and `POST /tailnets` returned 404. Those guessed top-level collection paths are still wrong, but they no longer imply that organization tailnet creation is unavailable. Do not reintroduce an `UnavailableApi404` capability verdict.

## Shared-tailnet evidence

Historical calls to `/tailnet/-/keys`, `/tailnet/-/devices`, and `/tailnet/-/acl` returned 403 for the tested organization OAuth client. Shared-tailnet methods remain API-compatible, but capability reporting and readiness must stay separate from organization-tailnet listing/creation.

## Secret lifecycle invariant

Child OAuth ID, secret, and token use explicit redacted wrappers and zeroized allocations where practical. They may enter only a child-scoped client held by the server provider's in-memory vault. They must not enter public DTOs, durable records, logs, error bodies, or generic retained JSON. After restart, the server fails closed and reports `child_secret_unavailable_manual_remediation`; production needs encrypted secret custody and reconciliation.
