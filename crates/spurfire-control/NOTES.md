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

## Child policy gate

`ChildTailnetPolicy::restrictive_riders` generates the sole accepted dedicated-lobby policy: one `tag:spurfire-lobby-<uuid>` source may reach only the same tag on `udp:41643`. Empty ACL, SSH, node-attribute, route/exit-node auto-approval, and test sections are explicit. `ChildTailscaleClient` writes and reads `/tailnet/{typed-dns-name}/acl` on the configured origin with redirects disabled, normalizes set-like policy fields, and returns only a SHA-256 semantic digest after an exact match. Unknown/additional semantics, mismatch, 403, transport, decode, and timeout conditions must fail closed before key mint and enter exact cleanup.

This contract is mock/fault-tested only. No live child policy write/readback was performed for this change, so provider acceptance and enforcement remain activation gates.

## Historical wrong-route evidence

`POST /tailnet` and `POST /tailnets` returned 404. Those guessed top-level collection paths are still wrong, but they no longer imply that organization tailnet creation is unavailable. Do not reintroduce an `UnavailableApi404` capability verdict.

## Shared-tailnet evidence

Historical calls to `/tailnet/-/keys`, `/tailnet/-/devices`, and `/tailnet/-/acl` returned 403 for the tested organization OAuth client. Shared-tailnet methods remain API-compatible, but capability reporting and readiness must stay separate from organization-tailnet listing/creation.

## Secret lifecycle invariant

Child OAuth ID, secret, and token use explicit redacted wrappers and zeroized allocations where practical. They may enter only the server's encrypted exact-tuple child vault and process-local child-scoped client cache. They must not enter public DTOs, non-secret lobby records, logs, error bodies, policy evidence, or generic retained JSON. Durable lobby state may retain only the policy semantic digest/coarse status and exact non-secret cleanup identity. Missing or mismatched custody fails closed; production still needs workload identity/setec, external audit/backup/rotation, and exercised reconciliation.
