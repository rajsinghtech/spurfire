# Release evidence manifests

This directory intentionally contains **no completed `0.2.0.json` manifest**. A completed manifest is a terminal release input, not a planning claim.

Before any stable tag or publication, copy `template.json` to `<version>.json`, replace every placeholder with exact-SHA evidence, set a gate to `true` only after its owning reviewer approves it, and remove every blocker. Then run:

```bash
scripts/check-alpha-evidence.py docs/release-evidence/<version>.json \
  --version <version> \
  --source-sha <full-git-sha>
sha256sum docs/release-evidence/<version>.json
```

The independently reviewed SHA-256 is required by the protected manual client publisher. The validator deliberately requires Apple Developer ID signing and notarization, Windows Authenticode signing, exact-SHA provenance/SBOM/launch smoke, private-live exact-cleanup evidence, natural M2 evidence, activation approval, and independent release approval.

Candidate workflow artifacts do not satisfy this contract. They are short-lived, nonpublishing, and explicitly record the current macOS and Windows trust blockers.
