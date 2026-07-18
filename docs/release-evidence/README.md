# Release evidence manifests

This directory intentionally contains **no completed `0.2.0.json` manifest**. A completed manifest is a terminal release input, not a planning claim.

Before any stable tag or publication, qualify source commit **S**, copy `template.json` to `<version>.json`, set `source_sha` and every run/artifact record to S, replace every placeholder with independently checked evidence, set a gate to `true` only after its owning reviewer approves it, and remove every blocker. Then run:

```bash
scripts/check-alpha-evidence.py docs/release-evidence/<version>.json \
  --version <version> \
  --source-sha <full-git-sha>
sha256sum docs/release-evidence/<version>.json
```

Commit only the completed manifest and `docs/release-notes-<version>.md` in metadata commit **T**. Before tagging T, run `scripts/check-release-tag-binding.sh <version> T`; it proves S is an ancestor and rejects every non-metadata change between S and T. This avoids an impossible self-referential manifest hash while keeping builds and evidence bound to S.

The independently reviewed SHA-256 is required by the protected manual client publisher. The validator deliberately requires Apple Developer ID signing and notarization, Windows Authenticode signing, exact-SHA provenance/SBOM/launch smoke, private-live exact-cleanup evidence, natural M2 evidence, activation approval, and independent release approval.

Candidate workflow artifacts do not satisfy this contract. They are short-lived, nonpublishing, and explicitly record the current macOS and Windows trust blockers.
