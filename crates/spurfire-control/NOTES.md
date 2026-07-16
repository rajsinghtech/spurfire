# Tailscale API probe notes

Probed with OAuth client credentials from the gitignored `.env`. No credentials, OAuth tokens,
or auth keys are recorded here. Date: 2025-02-14.

`TS_API_BASE` already ends in `/api/v2`; paths below are relative to that base. JSON request
bodies are shown verbatim. Response bodies are redacted if they contain secret fields.

| Request | Body | Status | Response |
|---|---|---:|---|
| `POST /tailnets` | `{"name":"spurfire-probe"}` | 404 | `{"message":"404 page not found"}` |
| `POST /tailnet` | `{"name":"spurfire-probe"}` | 404 | `{"message":"404 page not found"}` |
| `POST /tailnet` | `{"displayName":"spurfire-probe"}` | 404 | `{"message":"404 page not found"}` |
| `POST /tailnet/-` | `{"name":"spurfire-probe"}` | 405 | empty |
| `POST /tailnet/create` | `{"name":"spurfire-probe"}` | 404 | `{"message":"tailnet \"create\" not found"}` |
| `POST /tailnets/create` | `{"name":"spurfire-probe"}` | 404 | `{"message":"404 page not found"}` |
| `POST /tailnet/-/create` | `{"name":"spurfire-probe"}` | 404 | `404 page not found` |
| `GET /tailnet/-/settings` | none | 200 | `{}` |
| `GET /tailnet/-/devices` | none | 403 | `{"message":"calling actor does not have enough permissions to perform this function"}` |
| `POST /tailnet/-/keys` | `{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":true,"preauthorized":true,"tags":["tag:spurfire-probe"]}}},"expirySeconds":300}` | 403 | `{"message":"calling actor does not have enough permissions to perform this function"}` |
| `DELETE /device/spurfire-probe-invalid-id` | none | 404 | `{"message":"no manageable device matching this ID found"}` |

An initial diagnostic accidentally appended `/api/v2` to the configured base a second time.
Every request below consequently returned `404 {"message":"404 page not found"}`; retained here
for completeness:

- `POST /api/v2/tailnets` with `{"name":"spurfire-probe"}`
- `POST /api/v2/tailnet` with `{"name":"spurfire-probe"}`
- `POST /api/v2/tailnet` with `{"displayName":"spurfire-probe"}`
- `POST /api/v2/tailnet/-` with `{"name":"spurfire-probe"}`
- `POST /api/v2/tailnet/create` with `{"name":"spurfire-probe"}`
- `POST /api/v2/tailnets/create` with `{"name":"spurfire-probe"}`
- `GET /api/v2/tailnet/-/settings`
- `GET /api/v2/tailnet/-/devices`
- `POST /api/v2/tailnet/-/keys` with the auth-key body shown above
- `DELETE /api/v2/device/spurfire-probe-invalid-id`

## Conclusion

The OAuth endpoint and tailnet settings endpoint work. This OAuth client lacks device-list and
auth-key permissions. No tested tailnet-create route exists, so dedicated tailnet provisioning
must report that the alpha API is unavailable. Shared-tailnet provisioning can use the standard
`/tailnet/{tailnet}/keys`, `/tailnet/{tailnet}/devices`, and `/device/{id}` endpoints when the
OAuth client is granted the corresponding scopes.
