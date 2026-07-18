# Control-plane network ownership and selected-lobby view

- **Decision status:** accepted architecture and safe-groundwork contract.
- **Public real activation:** **closed**. Ottawa remains public dry-run.
- **Gameplay boundary:** peer-to-peer; the control plane never joins a lobby tailnet.

This document is the source of truth for dedicated lobby-network ownership, the capability-protected one-lobby inspector, telemetry provenance, cleanup proof, activation gates, and operator response. It does not authorize a public real deployment.

## Current posture and target

| Area | Status |
|---|---|
| Organization tailnet list/create, typed stable ID and `dnsName`, child OAuth exchange, and child-scoped deletion | Implemented and live-proven in bounded disposable probes |
| Child-scoped one-use auth-key issuance | Implemented and mock-tested; live end-to-end verification remains required |
| Dedicated `tailnet_per_lobby` isolation | Selected real-lobby path |
| `shared_tailnet` | Separate compatibility path; never represented as dedicated isolation |
| Child OAuth custody | Process-local and zeroized today; restart fails closed and may require manual remediation |
| Durable provider identity, selected-lobby DTO/cache, creator read capability, one-real-lobby lease, browser/CLI view, and default-off deployment switch | Safe groundwork; older revisions that discard stable ID or lack the switch are unsuitable, and this slice is not a basis for enabling public real mode |
| Invitation/participant/report authorization, migration of every legacy route, encrypted vault, startup reconciler, private operator listener, and abuse controls | Activation work remains open |
| Ottawa | Public, credential-free, non-persistent dry-run; no real provider mutation |

The server can exercise real provider code only when deliberately configured past its independent default-off switch. That private integration ability is not production authorization. The chart fixes the switch off and rejects enabling it. Until every gate in [Activation gates](#activation-gates) is green, public deployments must have no credential path and must force `dry_run`.

## Invariants

1. `tailnet_per_lobby` is the preferred real isolation mode. `shared_tailnet` remains separately labeled compatibility mode and cannot bypass real-lobby controls or quota.
2. Owning a network means creating it, binding its identity, issuing narrow enrollment credentials, inspecting scoped metadata/reports, reconciling it, and deleting it. Ownership does **not** mean membership.
3. `spurfire-server`, `spurfire-control`, and the operator path in `spurfire-ctl` never run a Tailscale/RustScale node in a lobby tailnet.
4. The control plane is never a gameplay server, relay, peer, permanent network member, trusted gameplay witness, or ranked-trust source. Inspection never decides gameplay authority.
5. A lobby ID identifies a record; it grants no authorization.
6. Inspection GETs read cached state only. They perform no provider I/O, cleanup, credential mint, election change, or other mutation.
7. Every displayed fact carries its source, assurance, observation/receipt time, and freshness. A failed source produces stale or unknown data, never a fabricated `offline`, zero, unavailable, or absent value.
8. `LobbyState::DESTROYED`, a successful/404 delete response, and an empty device list are not proof that a dedicated tailnet is absent.
9. No auth key, OAuth material, bearer token, capability plaintext, device/node private key, raw provider response, decrypted SOPS/setec value, or packet content may enter an inspector response, URL, argument, log, metric, durable JSON record, or support artifact.
10. Network reports are inspection inputs only. They cannot affect gameplay authority, hit validation, results, ACLs, ranked trust, or destructive cleanup.

## Ownership without membership

### Decision: never join

The main control plane must not join any lobby tailnet. This applies to `spurfire-server`, `spurfire-control`, and normal `spurfire-ctl` operator workflows.

Joining supplies no trustworthy player-to-player truth and creates material risk:

- A node must hold private node state, expose peer-facing sockets, and process untrusted traffic. Provider API access and authenticated participant reports do not require that attack surface.
- One process joined to multiple dedicated tailnets becomes a cross-tailnet bridge in the compromise blast radius, even when it does not intentionally forward traffic.
- An Ottawa node measures Ottawa-to-rider paths. It cannot infer player A-to-player B route class, application RTT, loss, or reachability.
- The extra member distorts device counts and complicates exact cleanup and stale-device alerts.
- Speaking the gameplay protocol would make the service a participant or witness and violate D1/D6.
- Joining introduces a runtime dependency and lifecycle coupling to RustScale/Tailscale node state. Control-plane availability must not depend on data-plane enrollment.

All required observables have non-member sources:

- durable control events for intent, lobby/network lifecycle, lease, generations, heartbeat receipts, and cleanup;
- typed provider create/list/device responses for resource identity and coarse enrollment metadata;
- authenticated-but-untrusted participant reports for directional path class and application measurements;
- deterministic derivations for aggregates and election formula output.

CI must reject a `spurfire-server` dependency on RustScale, `spurfire-net`, a gameplay listener, or an observer-node package. The server may depend on typed control/API DTOs; it may not acquire a data-plane runtime.

### Optional observer boundary

No observer is retained in the current architecture. A future diagnostic observer requires a separate security ADR and activation review, and only when a concrete question cannot be answered by provider observations or participant reports. It must be a separate, operator-triggered process for one selected lobby, run for at most 120 seconds with one-use enrollment, private temporary node state, and no organization/child OAuth or Kubernetes identity. It must not accept/advertise routes, act as exit node, use SSH/Serve/Funnel/LocalAPI, provide peer relay, or speak gameplay. Its output must be labeled `observer_path` and `non_authoritative`. It is not permitted in Ottawa under this decision.

## Tailnet DNS name terminology and privacy

The canonical field is `tailnet_dns_name`. It is the complete provider-returned tailnet DNS name/FQDN, for example `tail9a1c23.ts.net`, canonicalized to lowercase ASCII without a trailing root dot.

In that example:

- the TLD is **`.net`**;
- `.ts.net` is a provider DNS suffix, not a TLD;
- the useful operational value is the complete FQDN `tail9a1c23.ts.net`.

Do not introduce fields named `tailnet_tld`, `tld`, or `tailnet_domain_suffix`. The FQDN must originate in a typed provider response or trusted server configuration, never request input. Before it is used as a provider path segment, validate DNS label/total limits and reject empty labels, controls, slash, percent, userinfo, port, query, or fragment syntax; then use a URL path-segment builder/encoder. Dry-run must use `null`, never a guessed FQDN or the internal `dry-run.invalid` selector.

A tailnet FQDN and private `100.x`/applicable ULA addresses are metadata, not credentials, but they reveal topology. Audience policy is therefore explicit:

| Audience | Selection | FQDN and topology policy |
|---|---|---|
| Public/anonymous | No real-lobby selection | No lobby ID, FQDN, private address, roster, route/RTT row, authority detail, provider ID, or cleanup diagnostic |
| Invitation holder | One exact invitation | Join preview only (`display_name`, state, available slots, expiry, wire version, and REAL/SIMULATED label); no FQDN, addresses, roster telemetry, provider identity, or authority detail |
| Participant/member | Capability-bound lobby/player/generation | FQDN while active or cleanup is pending; participant-safe roster/network/authority/cleanup view; private application endpoints only when the session requires them |
| Creator | Exact created lobby | Member view plus bounded invitation/start/destroy controls |
| Operator | Private infrastructure identity, then one exact lobby | Member view plus stable provider ID, generation, lease/reconciliation codes, retries, and bounded tombstone; private addresses require separate topology-read scope |

After `DEDICATED_ABSENT`, omit the FQDN from participant output. An operator may retain stable ID and FQDN only for the existing 24-hour tombstone window, then purge them. Private endpoints and peer reports are memory-only and are erased immediately when cleanup starts or no later than 60 seconds after becoming stale.

## Exact-lobby authorization and selection

### Capability model

Lobby-scoped capabilities are compatible with D8: they are ephemeral authorization, not accounts or persistent user profiles.

- Token: opaque base64url with at least 256 bits from the OS CSPRNG.
- Storage: only a domain-separated SHA-256 verifier, exact scope, `lobby_id`, optional `player_id`, `network_generation`, expiry, and revocation/consumption state. Compare in constant time.
- Transport: `Authorization: Spurfire-Capability <token>` over TLS only.
- Scopes: `lobby.read`, `lobby.report`, `lobby.invite`, `lobby.manage`, `lobby.destroy`, and operator-issued one-use `lobby.create_real`.
- Creator capability: returned only by the first successful create response; expires after lobby TTL plus a short cleanup grace.
- Invitation capability: one-use, expires within ten minutes, and reveals only the minimal join preview.
- Participant capability: returned only on the first successful invitation join; bound to lobby, player, network generation, scopes, and an expiry no later than the lobby absolute TTL.
- Real-create grant: one-use and operator-issued during alpha.

Never put capability plaintext in a URL/query, log, metric, CLI argument, environment variable, browser local/session storage, durable plaintext, or replay response. The browser inspector keeps it only in memory. `spurfire-ctl` reads it from `--cap-file <path|->`; `-` means standard input. Lost capabilities have no account-recovery path in alpha; use creator revocation/reissue or operator remediation.

Authorization is evaluated before exact-lobby state is disclosed. Absent, wrong-lobby, wrong-generation, expired, revoked, and insufficient-scope requests all return the same `404 lobby_not_found` body. Sensitive responses include:

```text
Cache-Control: private, no-store
Vary: Authorization
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
```

### Selection experience

- The final `GET /inspect` route is a static, no-store shell with separate lobby-ID and capability inputs. It embeds no lobby data, uses no third-party assets, keeps the capability in memory, and offers no search/list endpoint. Safe-groundwork route integration must converge on this separate path before activation.
- An authorized participant or creator can inspect only the exact lobby bound to the capability.
- Operators authenticate to a private listener with mTLS/OIDC or a Kubernetes-authenticated port-forward. A minimal internal list may be used to choose a lobby; the operator then inspects exactly that lobby.
- The public listener has no real-lobby existence oracle. Aggregate real-network metrics are suppressed when fewer than three real lobbies contribute; the alpha quota of one therefore suppresses them unconditionally.

The selected-lobby screen begins with one unambiguous truth label:

- `SIMULATED — NO TAILNET EXISTS`
- `REAL — DEDICATED TAILNET`
- `REAL — SHARED COMPATIBILITY`

It then shows backing identity (as allowed for the audience), lobby and network lifecycles, qualified counts, directional routes, application quality, authority layers, freshness, and cleanup. Every value has visible source/assurance/freshness; `UNKNOWN` and `STALE` are first-class display states.

The safe-groundwork CLI is:

```sh
spurfire-ctl lobby inspect --lobby <uuid> --server <https-url> --cap-file <path|-> [--json]
```

It never accepts the capability as an argument value or environment variable and never echoes or persists it.

Default application limits are additive to gateway limits: dry-run create 5/IP/10 minutes (burst 2), real create one attempt per one-use grant plus 3/IP/hour, invitation issue 10/creator/lobby/minute with at most 32 active, join mint at least the existing 4/player and 32/lobby/60 seconds plus invitation/IP budgets, report 1 accepted/participant/2 seconds (burst 2, 30/minute), inspect 60/capability/minute and 120/IP/minute (burst 5), and authorization failures 60/IP/minute before a non-enumerating 429. Provider polling is never user-triggered.

## Network-view contract

The safe protocol model is `spurfire_protocol::network_inspection::LobbyNetworkView` (re-exported from `spurfire_protocol`). Its validation is pure and performs no provider I/O. HTTP authorization, audience projection, caches, and provider lifecycle remain separate activation work.

Schema version 1 uses the top-level shape:

```text
schema_version, lobby_id, served_at, truth_label, backing,
lobby_lifecycle, counts, routes, application_quality, authority, cleanup
```

`backing` includes `backing_mode`, `simulates_mode`, `isolation`, `network_generation`, `network_lifecycle`, `tailnet_dns_name`, and `control_service_member`. For every mode, `control_service_member` is control-authoritative `false`.

A dry-run view is exactly simulated:

- `backing_mode=dry_run`;
- `simulates_mode=tailnet_per_lobby`;
- `isolation=simulated`;
- `network_lifecycle=SIMULATED`;
- `tailnet_dns_name.value=null`, source `none`, freshness/unknown reason `not_applicable`;
- zero provider calls.

Real dedicated views expose the FQDN only through an authorized projection. Provider stable ID, secret-reference presence, reconciliation details, lease state, and poll codes are operator-only and are not part of a participant DTO. Unauthorized fields are omitted server-side, not returned as redacted/null facts.

### Fact envelope

Each displayed fact has this envelope:

```json
{
  "value": "T or null",
  "source": "control_store | provider_api | participant_report | derived | none",
  "assurance": "authoritative | observed | reported | derived | unknown",
  "as_of": "Unix milliseconds or null",
  "received_at": "Unix milliseconds or null",
  "freshness": "current | fresh | stale | unknown | not_applicable",
  "unknown_reason": "reason or null"
}
```

Rules:

- `value=null` requires `assurance=unknown` and an explicit reason such as `never_observed`, `unsupported`, `permission_denied`, `timeout`, `source_error`, `conflict`, `reconciliation_pending`, `stale_beyond_retention`, or `not_applicable`.
- A stale fact keeps its last known value and original assurance.
- Freshness uses service `received_at` or provider poll-completion time; peer wall clocks are advisory only.
- `authoritative` is narrowly scoped: control intent/lifecycle, provider resource identity, accepted receipt, or deterministic formula application. It never upgrades gameplay truth.

### Sources and interpretation

| Displayed area | Source and assurance | Required interpretation |
|---|---|---|
| Lobby/network lifecycle and roster count | Durable control store; authoritative for service state | Not proof of provider absence or gameplay health |
| Stable tailnet ID and FQDN | Typed provider create/list response; authoritative for provider identity | Stable ID is operator-only; FQDN is audience-gated |
| Enrolled-device count | Child-scoped device list; observed at successful poll | Device enrollment is not player identity and can differ from roster |
| Provider online count | Unknown/unsupported until a live field and semantics are verified | `lastSeen` is coarse metadata, not an online boolean or app health |
| Route class | Latest fresh directional participant report; reported | RustScale `Relay` is labeled **Peer Relay**; never infer reverse direction |
| Application RTT/jitter/loss | Participant application nonce/reply and sequence-window report; reported | Do not substitute DERP-region, WireGuard/discovery, election, or observer latency |
| Control election | Deterministic formula over identified inputs; derived/authoritative only for formula application | Inputs remain partly peer-reported |
| Heartbeat | Accepted service receipt event; authoritative only as receipt | Not proof of match-state correctness |
| Match authority | Participant reports with agreement/conflict counts; reported | Not ranked proof and cannot drive the election |
| Cleanup | Durable events, child delete acknowledgement, and exact parent listing | Absence requires the complete proof below |

### Qualified counts and routes

Never expose an unqualified `peer_count`. Required counts are `roster_count`, `provider_enrolled_device_count`, `provider_online_device_count`, `fresh_reporter_count`, and `fresh_directional_observation_count`. The control service is never included.

Every route row is directional: `from_player_id -> target_player_id`. For roster size `n`, `expected_direction_count=n*(n-1)`. Use each reporter's latest fresh row per target and classify it as `direct`, `peer_relay`, `derp_relay`, `unavailable`, or `unknown`.

```text
reported_direction_count = direct + peer_relay + derp_relay + unavailable + unknown
reachable_known_count = direct + peer_relay + derp_relay
direct_ratio_milli = floor(1000 * direct / reachable_known_count)
```

`direct_ratio_milli` is `null` when `reachable_known_count` is zero. `unavailable` means the reporter attempted but has no usable path; `unknown` means no usable claim exists. Neither direction may be inferred from the other.

Application RTT is directional application nonce/reply RTT. Unknown metrics serialize as `null`, never zero. The aggregate exposes `sample_count`, `application_rtt_ms_median`, nearest-rank `application_rtt_ms_p95`, `application_rtt_ms_worst`, and `application_loss_ppm_median`. Existing `ConnectivityReport.rtt_ms_*` remains a peer-reported election input until its application-level contract is proven; it must not be relabeled as application RTT.

Authority is three separate fact envelopes: `control_election` (`formula_version`, winner, score, input hash, evaluation time, degraded flag, and input assurance), `last_accepted_heartbeat` (player, epoch, input hash, service receipt time), and `peer_reported_match_authority` (reported player/epoch/input hash plus fresh reporter, agreement, and conflict counts).

Participant cleanup exposes only network lifecycle, `requested_at`, `delete_acknowledged_at`, `absence_confirmed_at`, and a participant-safe reason. Stable provider identity, attempts, poll result, vault deletion, lease state, and remediation code are operator-only.

### Participant network reports

The participant capability supplies `reporter_player_id`; the body cannot choose another subject. A report includes network/session generations, a strictly increasing sequence, advisory client measurement time, up to four validated tailnet addresses, up to 15 directional observations, and optional reported match authority.

Server controls:

- maximum body 16 KiB; no more than `roster_size-1` distinct current-roster targets and 15 rows;
- no self, unknown, or duplicate target;
- application RTT/jitter `0..=10000 ms`, loss `0..=1000000 ppm`, packet age `0..=60000 ms`;
- exact lobby/player/network-generation binding, current session generation, and increasing per-reporter sequence;
- one accepted report every two seconds, burst two, no more than 30 per minute;
- only live-validated Tailscale CGNAT/applicable ULA addresses; never physical/public NAT endpoints or arbitrary ports;
- address/provider-device overlap is only corroboration, never player/device identity;
- packet contents are never accepted or retained.

An authenticated participant can still lie. Show reporter and disagreement counts, and never use reports for gameplay or destructive decisions.

### Refresh and failure model

The target architecture uses background workers to refresh caches; this groundwork exposes only explicit bounded internal refresh calls and does not start a production scheduler. Request handlers never call the provider.

| Source | Collection | Freshness/retention |
|---|---|---|
| Child device inventory | Every 15 s while active/cleanup states need it | Fresh for 30 s |
| Parent exact-ID presence | Every 60 s | Fresh for 120 s; discard unrelated entries immediately |
| Participant report | Client target 5 s with ±1 s jitter; minimum accepted interval 2 s | Fresh for 15 s; stale value retained at most 60 s in memory |
| Election measurement | Existing independent window | Fresh for 60 s; do not conflate with inspector report freshness |
| Provider call | 5 s timeout, one in flight per lobby | Honor `Retry-After`; back off with jitter from 15 s to 5 min |

Timeout, 403, decode failure, source error, or conflict preserves the last successful value as stale. If no successful value exists, return unknown with the precise reason. Never translate collection failure into offline, zero, absent, or successful cleanup.

## Independent network lifecycle

Lobby lifecycle and network lifecycle are separate dimensions:

| Network state | Meaning |
|---|---|
| `SIMULATED` | No provider mutation was permitted; no tailnet exists |
| `RESERVED` | Durable real-lobby lease and create intent exist; no provider request sent |
| `CREATING` | Create is in flight or its durable intent awaits a result |
| `ACTIVE` | Backing identity, exact tuple cross-check, and encrypted child-credential commit succeeded |
| `CREATE_REJECTED` | Provider definitively rejected create before a resource could exist |
| `CREATE_UNKNOWN` | Create may have succeeded but result/custody commit is ambiguous; joins and new real creates close |
| `CLEANUP_REQUESTED` | Durable cleanup intent exists |
| `CLEANUP_PENDING` | Delete/revoke failed, was denied, or has not completed |
| `VERIFYING_ABSENCE` | Delete returned success/404, but exact stable-ID absence is not yet proven |
| `DEDICATED_ABSENT` | Exact stable ID is confirmed absent and encrypted child material is erased |
| `SHARED_RESOURCES_CLEAN` | Known lobby keys and tagged devices are clean; the shared tailnet still exists |
| `MANUAL_REMEDIATION` | Identity, credential, or reconciliation evidence is insufficient for safe automation |

The current process-local prototype cannot satisfy the production meaning of `ACTIVE` after restart. Do not infer a network state from a legacy lobby state.

## One-real-lobby lease and idempotency

Alpha permits one active real lobby across both real backing modes. Shared compatibility counts against the same quota and cannot bypass it. Public real create, when eventually enabled, is server-selected `tailnet_per_lobby`; a client-selected shared mode is rejected.

For a new real request, the store must resolve idempotency first, then atomically acquire a singleton `RealLobbyLease` and persist the `PROVISIONING`/`RESERVED` network intent in one transaction, before any provider call. The lease binds holder lobby, network generation, idempotency digest, acquisition time, and lifecycle.

- Same key/body/actor replay returns the original lobby without another create, even while capacity is full.
- A different request while held returns `409 real_lobby_capacity_reached` without revealing the holder.
- Reusing a key with different input returns `409 idempotency_conflict`.
- Hold the lease through every ambiguous, active, cleanup, polling, restart, or vault-deletion state.
- Release only after definitive `CREATE_REJECTED`, proven `DEDICATED_ABSENT`, or `SHARED_RESOURCES_CLEAN`.

Safety intentionally wins over availability: one ambiguous orphan can lock the only slot until operator remediation.

## Secret custody and startup reconciliation

Production requires a dynamic encrypted child-credential vault, intended to be setec-backed, with workload identity, audit logging, backup/recovery, compare-and-swap/versioning, and verified deletion. A static Kubernetes Secret, SOPS manifest, JSON store, log, or metric is not suitable for one-time dynamically generated child credentials.

The non-secret store binds `lobby_id`, `network_generation`, provider stable ID, FQDN, secret reference, lifecycle, and timestamps. The encrypted vault record binds the same identity tuple plus child OAuth client ID/secret and creation time. Every use or delete cross-checks the complete tuple.

Provider create returns the child secret once, so there is an unavoidable store/provider/vault crash window. Public activation requires either live-verified provider idempotency/credential recovery or an explicitly approved orphan-detection and manual exact-ID procedure that closes mutations and locks the quota. Do not claim cross-system transactionality.

Startup begins mutation-closed. Reconcile all retained real records, vault records, lease state, and exact upstream stable IDs before enabling joins or any real mutation:

| Store | Vault | Upstream exact tuple | Required action |
|---|---|---|---|
| present | present | present and exact match | Authenticate child scope, resume `ACTIVE`, hold lease |
| present | present | absent in two qualifying polls | Erase vault record, mark `DEDICATED_ABSENT`, release lease |
| present | missing | present or unknown | `MANUAL_REMEDIATION`; disable joins/new creates; hold lease |
| present | missing | absent in two qualifying polls | Mark `DEDICATED_ABSENT`, release lease |
| missing | present | any | Quarantine; do not delete or erase automatically; close real mutations |
| missing | missing | unmatched Spurfire child | Quarantine orphan; do not guess; lock real creation |
| any identity mismatch | any | any | Stop reconciliation and mutation; never delete by display name or guessed selector |

## Cleanup proof

Dedicated cleanup order is exact and crash-safe:

1. Persist `CLEANUP_REQUESTED`.
2. Load and cross-check lobby ID, network generation, stable provider ID, FQDN, and vault tuple.
3. Delete only with the matching child-scoped credential and validated stored FQDN.
4. Enter `VERIFYING_ABSENCE` after success or 404; neither result proves absence.
5. Obtain two successful parent-organization listings at least five seconds apart in which the exact stored stable ID is absent. Discard unrelated entries immediately.
6. Erase encrypted child credentials and verify vault deletion.
7. Atomically mark `DEDICATED_ABSENT`, release the real-lobby lease, and retain only the bounded non-secret tombstone.

`LobbyState::DESTROYED`, provider delete 200/404, DNS-name absence, display-name matching, and device count zero are each insufficient.

## Operator runbook

### Safe inspection

1. Use the private operator listener through mTLS/OIDC or a Kubernetes-authenticated port-forward. Never send an operator token through the public Gateway.
2. Select one row from the minimal operator list, then request only that lobby's cached network view.
3. Confirm truth label, backing mode, network generation, network lifecycle, and lease holder generation before reading details.
4. Check each fact's source and freshness. Treat stale as historical and unknown as no claim. Do not trigger provider reads from the HTTP request.
5. Share FQDN/private-address data only with its approved audience. Never paste capabilities or provider bodies into a ticket.

### `CREATE_UNKNOWN`, restart uncertainty, or quota lock

1. Leave the singleton lease held; disable joins, all new real creates, and cleanup automation that lacks the exact tuple.
2. Record only non-secret lobby/generation/stable-ID/FQDN/event references in the incident. Never record OAuth, bearer, auth-key, or capability plaintext.
3. Compare store, vault metadata, lease, and parent organization listing by exact stable ID. Do not retry create and do not match by display name/FQDN alone.
4. Apply the reconciliation matrix above. Any mismatch enters `MANUAL_REMEDIATION` and stops automated mutation.
5. Release the slot only through an allowed terminal transition with its complete evidence.

### Cleanup pending

1. Verify persisted cleanup intent and the complete identity tuple.
2. If the matching vault credential exists, retry only child-scoped deletion with the validated stored FQDN and bounded backoff.
3. After success/404, run the two exact stable-ID absence observations at least five seconds apart.
4. Erase and verify vault deletion, then atomically mark absent/release. A vault deletion failure keeps `CLEANUP_PENDING` and the lease.
5. If provider polling fails, retain last-good evidence as stale; never infer absence.

### Missing vault material or orphan

1. Freeze real mutations and joins and retain the lease/quarantine.
2. If a store record exists, locate only the exact stable ID in the parent inventory. If identity fields disagree, stop and escalate; do not delete.
3. If the exact child exists but its vault record is missing, use an approved provider-administrator remediation procedure/change record against that exact stable resource. Do not make normal server code fall back to organization OAuth for child deletion and do not invent child credentials.
4. If a vault record exists without store state, preserve it in quarantine; do not erase or delete automatically.
5. If an unmatched upstream child exists without either record, correlate audited create events manually. A Spurfire-like display name is not sufficient deletion evidence.
6. Following authorized remediation, require the normal two-poll exact-ID absence proof. Erase any matching vault record, record the non-secret evidence, and only then release the slot.

### Ottawa guardrail

Ottawa's required GitOps values remain:

```yaml
config:
  dryRun: true
  provisioningMode: dry_run
tailscale:
  existingSecret: ""
persistence:
  enabled: false
```

The public Gateway may serve the static inspector shell and dry-run APIs only. Operator routes are forbidden. This documentation change does not edit Ottawa, create/delete a tailnet, or authorize a future value flip.

## Activation gates

Every item is mandatory; order does not imply partial activation:

- [ ] `SPURFIRE_REAL_MUTATIONS_ENABLED` exists, defaults false, and independently gates every real create/mint/delete path even when credentials and a real provisioning mode are present.
- [ ] Capability authentication/authorization protects every lobby-specific read and mutation; player IDs are identifiers only; real create consumes an operator-issued one-use grant.
- [ ] TLS, capability expiry/revocation, body limits, gateway/IP and application subject limits, abuse alerting, and uniform 404 anti-enumeration are deployed and tested.
- [ ] The static inspector has strict CSP, no third-party assets, in-memory-only capability handling, no-referrer/no-store behavior, and XSS tests.
- [ ] A dynamic encrypted child-OAuth vault has workload identity, audit, backup/recovery, CAS/versioning, and deletion semantics. No dynamic credential is in JSON, SOPS, logs, or Kubernetes manifests.
- [ ] Mutation-closed startup reconciliation covers store, vault, lease, and exact upstream identities; every mismatch fails closed.
- [ ] The create-to-vault crash window and exact-ID manual orphan-remediation procedure are approved and exercised.
- [ ] Atomic one-real-lobby lease behavior is proven under concurrent create, replay, crash, timeout, restart, and cleanup/vault failure; deployment is single writer or uses fencing.
- [ ] Public real mode accepts only `tailnet_per_lobby`; shared compatibility remains separately configured and consumes the same quota.
- [ ] A restrictive child policy is applied and read back: rider tags reach only required gameplay UDP, with no SSH, Serve/Funnel, subnet/exit routes, unintended service, or control-plane membership. Direct, DERP, and Peer Relay paths are live-tested.
- [ ] Child-scoped device-list scopes and field semantics are live-verified. Provider online remains unknown if no verified field exists.
- [ ] Application-level lobby/player/session binding is deployed; tailnet membership, hostname, address, device count, and peer report never become identity.
- [ ] The operator listener is private and unreachable through Ottawa's public Gateway.
- [ ] Privacy approval covers FQDN, private addresses, participant reports, and tombstone retention; public small-cohort suppression is enabled.
- [ ] Exact-ID cleanup, quota lock, vault, and reconciliation alerts plus this runbook are deployed and exercised.
- [ ] Persistent non-secret state and an approved parent-credential path are deployed; no credential is copied to `raj-builder`.
- [ ] Clean credential-free Linux checks pass on `ssh ubuntu@raj-builder`; cross-platform GitHub Actions pass. No Mac build is used for this activation.
- [ ] A separate GitOps review attests every preceding gate and explicitly changes Ottawa. Until then Ottawa remains forced dry-run.

## Required test plan

These are activation requirements, not claims that the current public service implements them all.

### Contract, projection, and secrets

- Dry-run view is `SIMULATED`, has null/not-applicable FQDN, and makes zero provider calls.
- FQDN validation accepts typed canonical names and rejects slash/query/fragment/control/percent/port/userinfo/empty-label injection; provider paths cannot become an arbitrary proxy.
- Audience golden snapshots prove anonymous/member/creator/operator omission boundaries for FQDN, addresses, provider ID, reconciliation, and tombstones.
- Secret canaries are absent from `Debug`, JSON, errors, logs, metrics, CLI output, inspector HTML, and durable state.
- Sensitive responses carry no-store/vary/security headers; unauthorized and unknown exact lobbies share one 404 shape.
- Public aggregates suppress real metrics when the contributing cohort is smaller than three.

### Identity, capability, and reports

- Scope, exact lobby/player/generation binding, constant-time verifier path, expiry, revocation, invitation one-use, first-response-only plaintext, and replay receipts are tested.
- CLI reads capability from a file/stdin, never argv/environment, and neither persists nor echoes it.
- Reports reject forged subject, wrong generation/session, stale or duplicate sequence, self/unknown/duplicate targets, more than 15 rows, oversized body, invalid route/range/address, and excess rate.
- Directional aggregation proves no reverse inference and the exact reachable-only `direct_ratio_milli` denominator.
- Application RTT unknown is null; transport/DERP/election/observer latency cannot populate it.
- Authority output keeps formula result, accepted heartbeat receipt, and peer-reported authority/agreement/conflict separate.

### Provider cache and freshness

- Typed create preserves stable ID, FQDN, and generation; any identity/generation/vault mismatch blocks deletion.
- Provider timeout, 403, decode failure, and conflict preserve last-good stale facts and never synthesize offline, zero, or absent.
- Boundaries cover device inventory at 30 seconds, organization presence at 120 seconds, reports at 15/60 seconds, and election input at 60 seconds.
- Inspection GET performs no provider I/O, cleanup, store write, election update, or other mutation.
- Provider inventory never persists raw responses, IDs/tags, or unrelated organization tailnets.

### Lease, reconciliation, and cleanup

- Concurrent distinct real creates yield exactly one lease and at most one provider create.
- Same idempotency key/body/actor replays the original lobby while full; changed input conflicts.
- `CREATE_UNKNOWN`, restart uncertainty, poll failure, cleanup failure, and vault deletion failure retain the lease.
- Lease releases only for definitive `CREATE_REJECTED`, proven `DEDICATED_ABSENT`, or `SHARED_RESOURCES_CLEAN`.
- Every startup reconciliation matrix row is tested, including mismatches and orphan quarantine.
- Cleanup requires two exact stable-ID absence observations separated by at least five seconds plus vault erasure. Delete 200/404 and lobby `DESTROYED` alone fail.
- Crash/fault injection covers every boundary between reserve, create, typed decode, vault commit, active, cleanup intent, delete, absence polls, vault erase, and lease release.

### Architecture and deployment

- CI fails if `spurfire-server` gains a RustScale, `spurfire-net`, gameplay listener, relay, or observer-node dependency.
- Rendered chart/default tests prove credential-free dry-run, one replica, no dynamic child secret in manifests, and no public operator route.
- Ottawa policy tests assert `dryRun=true`, `provisioningMode=dry_run`, `existingSecret=""`, and `persistence.enabled=false` until a separate approved activation change.
- Linux and GitHub Actions gates run without copying `.env` or credentials; no release tag or package publication is part of activation testing.

## Related decisions and docs

- [architecture.md](architecture.md) — control/data-plane boundary and dependency direction.
- [lobby-service.md](lobby-service.md) — current HTTP surface and protected target routes.
- [decisions.md](decisions.md) — D2 and D8–D11.
- [tailscale-api.md](tailscale-api.md) — dated provider API probe evidence; child key issuance remains mock-tested and needs live end-to-end verification.
- [p2p-networking.md](p2p-networking.md) — peer data plane and RustScale route classes.
- [testing.md](testing.md) — execution environment and validation entry points.
