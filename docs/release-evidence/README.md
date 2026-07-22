# Release evidence manifests

This directory intentionally contains **no completed `0.2.0.json` manifest**. These manifests belong to a future stable public release and are not required to begin the invited Alpha playtest.

Before any stable tag or publication, qualify source commit **S**, copy `template.json` to `<version>.json`, set `source_sha` and every run/artifact record to S, replace every placeholder with independently checked evidence, set a gate to `true` only after its owning reviewer approves it, and remove every blocker. Then run:

```bash
scripts/check-alpha-evidence.py docs/release-evidence/<version>.json \
  --version <version> \
  --source-sha <full-git-sha>
sha256sum docs/release-evidence/<version>.json
```

Commit only the completed manifest and `docs/release-notes-<version>.md` in metadata commit **T**. Before tagging T, run `scripts/check-release-tag-binding.sh <version> T`; it proves S is an ancestor and rejects every non-metadata change between S and T. This avoids an impossible self-referential manifest hash while keeping builds and evidence bound to S.

The independently reviewed SHA-256 is required by the protected manual client publisher. The local validator checks manifest structure only; booleans and identifiers are never treated as external proof. During publication, GitHub resolves the private-live run through the repository-configured trusted harness, downloads and hashes its sole redacted evidence artifact, runs the live lifecycle validator, and resolves activation and release approvals as two distinct exact-SHA PR reviews whose bodies bind the source and evidence digests. Missing external configuration or inaccessible evidence fails closed. The validator also requires Apple Developer ID signing and notarization, Windows Authenticode signing, exact-SHA provenance/SBOM/launch smoke, natural M2 evidence, and telemetry gates.

Approval review bodies must contain exactly the applicable binding (with real values substituted):

```text
SPURFIRE_ALPHA_ACTIVATION_APPROVED source_sha=<S> evidence_digest=sha256:<digest>
SPURFIRE_ALPHA_RELEASE_APPROVED source_sha=<S> evidence_digest=sha256:<digest>
```

Alpha candidate artifacts do not satisfy this future publication contract. They are short-lived, checksummed Linux/macOS builds intentionally scoped to invited human testing.
