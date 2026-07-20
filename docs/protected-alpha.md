# Protected hosted Alpha execution plane

Protected Alpha is a one-lobby, one-generation exception. It is not enabled by ordinary `spurfire-server`; that binary always uses `AppState::new_deny_all`. The Helm default remains credential-free dry-run and preserves the normal game/chart behavior.

## Immutable authority

A signed `spurfire-protected-alpha/v1` receipt binds the exact source revision, runtime and broker OCI `sha256:` digests, worker/broker executable hashes, provenance/artifact/policy digests, installation and state-store IDs, initial state hash, named Lease UID/resourceVersion, lobby/generation, supervisor run/epoch, two-player cap, public and broker listeners, launch-code verifier, 45-minute admission/play deadline, and following 15-minute cleanup deadline. Receipt installation and launch-code consumption occur in the same durable singleton-lobby reservation transaction. A consumed receipt recovers only cleanup authority.

The named Kubernetes `coordination.k8s.io/v1` Lease is the external anti-rollback CAS authority. Its annotation repeats installation ID, store ID, receipt digest, lobby/generation, supervisor epoch, state hash, phase, and both deadlines. Every mutating broker frame first checks the exact UID/resourceVersion/binding and performs a Kubernetes optimistic `PUT` CAS. A PVC copy or rollback has no resourceVersion authority and cannot reopen admission. Ambiguous tuples quarantine; `cleanup_only`, `released`, and `quarantined` phases cannot admit.

RBAC grants `get/create/update/patch` only for `spurfire-protected-alpha`; it grants no `delete/list/watch` or arbitrary Lease access. The chart pre-creates and retains the Lease.

## Process and pod boundaries

The runtime Deployment has one container. `spurfire-alpha-launcher` is PID 1, verifies the owner signature and fixed sibling inodes, opens the listener, and spawns `spurfire-alpha-worker` as a measured process-group sibling. The child gets only fixed inherited socket/listener descriptors; environment is cleared, unrelated FDs close, Linux parent-death signaling is set, and receipt bytes remain launcher-only. The launcher changes to cleanup at 45 minutes and destroys the protected process group at 60 minutes. It then starts only the credential-free dry-run/deny-all worker even if GitOps has not removed protected mode. Non-Linux activation exits 78.

The provider broker is a separate one-replica/Recreate Deployment and private ClusterIP Service. There is no broker HTTPRoute or public Service. Pinned installation-CA mTLS authenticates both pods; every request also carries a per-run HMAC, strict sequence, fresh nonce, exact run/lobby/generation/epoch, operation, Lease UID/resourceVersion, and response binding. `CleanupOnlyBrokerTransport` has no prepare/mint methods. NetworkPolicies allow broker ingress only from runtime, and only broker has external provider HTTPS egress.

Runtime receives no `TS_*` environment or provider/vault mount. Broker OAuth and vault-key files come from separate SOPS-managed Secret file mounts with mode `0400`. Credentials are never accepted in argv, environment, logs, or public protocol fields. Runtime gets only its broker client identity and per-run MAC file.

## Owner key workflow

`spurfire-owner-key` is offline and macOS-only:

```text
spurfire-owner-key init       # emits public key + key ID only
spurfire-owner-key public     # emits public key + key ID only
spurfire-owner-key sign < claims.json > receipt.json
```

It calls Security.framework through the `security-framework` crate. The Ed25519 seed is generated in memory and stored/retrieved as a Keychain generic-password byte value. There is no secret argv/stdout, `security -w`, plaintext file, or temporary plaintext. Any unsupported/failed Security API path prints `KEYCHAIN_BLOCKED` and exits 78. Replace the zero bootstrap public key in `crates/spurfire-server/src/owner_key.rs` with the emitted public key before building; the private seed never enters Git, CI, an image, or the cluster.

## Launch and cleanup contract

Raj or the automated creator enters the one-use launch code through the native `NativeSecretInput` launch-code field. Rust consumes it directly; GDScript never receives or persists secret text. Create is available only in the protected exact-lobby router and atomically consumes the receipt-bound verifier. Invitation Join remains a separate lobby-scoped, one-use capability.

Only one lobby/generation and two riders are accepted. Cleanup deletes by stable provider ID, completes every pagination cursor, observes exact absence twice with at least five monotonic seconds measured after the first response completes, erases the exact vault version by CAS and readback, then releases the Lease. Any missing page, identity mismatch, timeout, stale Lease, unknown provider outcome, vault mismatch, restart ambiguity, or incomplete proof remains cleanup-only/quarantined.

Rendering these resources is not activation evidence. Do not deploy, access credentials, mutate a provider, or mark GO without a separately reviewed receipt, public-key update, SOPS mounts, signed image/provenance digests, NetworkPolicy/RBAC review, and credential-free rehearsal evidence.
