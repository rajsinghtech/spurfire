# RustScale organization-tailnet tooling reference

Spurfire consulted, but did not modify, a sibling checkout of the RustScale repository. The reference scripts are under its `tools/tailnet/` directory:

| Script | Relevant behavior |
|---|---|
| `_lib.sh` | Form encoding; organization, cross-tailnet, and child token helpers |
| `ts-org-token.sh` | Organization client-credentials or WIF token exchange |
| `ts-create-tailnet.sh` | `POST /api/v2/organizations/-/tailnets` with `{displayName}` |
| `ts-list-tailnets.sh` | `GET /api/v2/organizations/-/tailnets` |
| `ts-child-token.sh` | Returned child OAuth pair to child-scoped token |
| `ts-cross-tailnet-token.sh` | Organization credential plus stable child ID flow where server-side flags/scopes allow it |
| `ts-delete-tailnet.sh` | Child-scoped `DELETE /api/v2/tailnet/{dnsName}`, with 404 treated idempotently |

These scripts established the correct endpoint and token boundaries. The raw RustScale create helper intentionally writes the full create response to stdout so an operator can capture the one-time child secret. That behavior is useful as reference tooling but is not safe as Spurfire's default operational interface.

## Spurfire wrapper policy

`scripts/ts-api.sh` therefore exposes a narrower surface:

- `token` prints status only and never the token;
- `list-tailnets` prints only a count and never prints the organization inventory;
- `probe-org-tailnet --confirm` is the only mutating command;
- the probe name is generated with the `spurfire-probe-*` prefix and the verified 50-byte grammar;
- an exit/signal cleanup trap is installed before creation;
- the create response is parsed in memory, child credentials and identifiers are not printed, deletion uses the child token, and a final organization list verifies stable-ID absence;
- failed cleanup returns an error and calls for manual remediation without printing child OAuth material;
- `self-test`, `bash -n`, and ShellCheck exercise the local safety path without network access.

Do not use the sibling scripts to bypass Spurfire's confirmation, prefix, redaction, or cleanup policy. Do not perform organization mutations in automated tests; Rust and HTTP behavior is covered by mocks.

## Production distinction

Reference tooling and one successful guarded probe prove API shape, not production readiness. Spurfire's integrated prototype uses an encrypted exact-tuple file vault with restart recovery and CAS erasure plus a process-local child-client cache. That is prototype custody, not an approved production secret manager: workload identity/setec, external audit/backup/rotation, least-privilege scope review, and separately authorized live restrictive-policy/key/cleanup failure-path validation remain required.
