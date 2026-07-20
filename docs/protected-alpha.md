# Protected Alpha execution plane

Protected Alpha is a separately deployed, one-lobby execution plane. It does not turn `spurfire-server` into a real-mode server. The ordinary binary permanently constructs `AppState::new_deny_all`; chart defaults remain credential-free dry run.

## Authority and activation

1. Create the durable state path at its final canonical location. Opening it creates a persistent random store instance ID. Record only the two SHA-256 values returned by the store challenge.
2. Build the exact source SHA on the Linux deployment platform. Record signed worker, broker, provenance, artifact-set, and policy-profile digests.
3. Issue an Ed25519 `spurfire-protected-alpha/v1` receipt binding those digests, the exact HTTPS origin, private listener, lobby ID, nonzero generation, store challenge, supervisor run/epoch, participant cap, and immutable receipt/final-I/O/absolute deadlines.
4. Deliver receipt and signer material through protected local custody. Receipt plaintext is zeroized after verification; durable state retains hashes only.
5. The protected constructor installs the receipt only when the already-open store challenge matches. Exact lobby reservation, generation, idempotency record, receipt consumption, immutable bindings, and the existing `real_lobby_lease` are committed by the single create transaction before provider I/O.
6. Create through the private supervisor router, then install the exact-lobby public route. The Alpha public router has literal paths only and has no create, list, inspection, recovery, supervision, or broker route.

A consumed receipt is not admission authority after restart. `protected_alpha_recovery` returns only lobby/generation/run metadata while the matching lease remains held. Recovery is cleanup-only.

## Credential custody

Use an owner-only (`0700`) broker custody directory. The organization credential file must be regular, non-symlink, owned by the broker UID, and exactly `0400`. Keep the vault key in a different file/descriptor. The protected launcher opens with `O_NOFOLLOW`, passes only the descriptor to the broker, waits for authenticated custody confirmation, verifies the open/path inode pair, unlinks the source, and fsyncs the directory.

The HTTP worker receives no `TS_*` values, credential file, vault key, or broker vault mount. It has only the non-secret state and authenticated Unix-socket transport. Broker IPC requests contain typed non-secret provider DTOs and are fenced by exact run, epoch, lobby, generation, identity, operation, sequence, challenge, policy digest, deadline, and fsynced ledger head.

Never put OAuth values, child credentials, auth keys, IPC keys, vault keys, receipt bytes, or credential examples in Helm values, ConfigMaps, logs, state JSON, evidence manifests, or this repository.

## Deadline, crash, and quarantine rules

New create, policy, mint, invitation, and admission work stops at the earliest signed or monotonic deadline. Cleanup remains enabled. Worker exit, timeout, supervisor restart, unknown/changed boot identity, wall-clock ambiguity, stale sequence/fence/CAS, provider uncertainty, partial pagination, and persistence/readback failure force cleanup-only or durable quarantine. Child creation without durable custody is manual remediation; it is not retried as another create.

Cleanup order is fixed: persist delete intent; delete the exact stable-ID/FQDN/generation tuple; collect two fresh, uncached, terminal-cursor inventory observations at least five monotonic seconds apart; persist the proof; erase the exact vault version by CAS; fsync and reopen/read back the tombstone; release the existing lease; reload and verify `Released`; remove exact ingress. Observation one never survives supervisor restart.

## Quarantine response

1. Remove the exact-lobby HTTPRoute immediately; verify generic creation and a different lobby ID return 404/403.
2. Do not delete, move, copy, edit, or restore only part of the state, ledger, head, or vault files.
3. Preserve image/provenance digests, receipt hash, ledger head, store binding, stable provider ID/FQDN/generation, and coarse error class. Preserve no secret values.
4. Inspect the complete parent inventory by stable provider ID. A timeout, repeated/malformed cursor, page/item limit, partial response, or display-name-only match is `Unknown`.
5. Complete exact provider deletion and a fresh two-observation proof. Then perform vault CAS/readback and lease release. If exact identity or custody cannot be established, retain quarantine for manual provider remediation.

## Objective GO and completion evidence

One immutable manifest for the deployed SHA must verify clean source; signed artifact/provenance digests; Linux tests; ordinary deny-all construction; default dry-run credential-free Helm render; path ownership/mode/no-follow checks; no prior lease/quarantine/orphan; exact receipt bindings; exact-lobby route rejection; complete pagination tests; and operator acknowledgement of this runbook.

Completion additionally requires durable two-observation absence proof, vault CAS/readback receipt, released lease, reloaded `Released` ledger, and recorded ingress removal. Child create-to-custody host failure remains a documented manual-remediation window because the provider has no atomic create-and-custody primitive.
