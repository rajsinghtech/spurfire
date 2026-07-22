# Alpha release qualification tooling

**Current verdict: NO-GO.** This document describes candidate-only automation. It does not authorize hosted real mutations, platform publication, or a `v0.2.0` tag.

## CI gates

Linux Godot qualification must emit each marker exactly once:

- `SPURFIRE_GODOT_SMOKE_OK`
- `SPURFIRE_POLISH_SMOKE_OK`
- `SPURFIRE_COMBAT_UI_SMOKE_OK`
- `SPURFIRE_ALPHA_LOBBY_SMOKE_OK`

`scripts/check-alpha-smoke-log.sh` rejects missing/duplicate markers and engine errors on every
client-preflight platform. The macOS jobs permit only Godot 4.7.1's exact headless
`RendererDummy` shader-RID teardown line tracked in issue #17; every other engine, script, parse,
or smoke error remains fatal. The integrated source now emits the one-lobby contract marker, while
that marker remains a fixture/source contract—not HTTP, provider, two-download, coherent
multiplayer, cleanup, or human qualification evidence. Existing gameplay smoke remains the
regression gate for M0–M2.

Release/check tooling has credential-free unit tests under `scripts/tests/`. Tests cover deterministic playtest aggregation, secret canaries, simulated versus private-live lifecycle evidence, exact cleanup ordering, platform trust blockers, and no-overwrite candidate metadata.

## Nonpublishing client candidates

`Client Preflight` builds on GitHub-hosted Linux x86_64, Linux ARM64, Windows, and macOS runners. It never creates a release, tag, package, or deployment. Each platform artifact contains its archive, SHA-256, launch-smoke result, and trust status. The Linux ARM64 job natively builds the registered `linux.*.arm64` GDExtension libraries, runs the full headless smoke suite, exports the `Linux ARM64` preset, asserts the exported binary is an aarch64 ELF, and launch-smokes the final archive. The final `alpha-candidate-<sha>` artifact combines:

- all four archives;
- `SHA256SUMS`;
- `candidate.spdx.json`;
- `candidate-manifest.json`; and
- platform trust records.

Non-PR runs request GitHub build-provenance attestations and verify them before marking
`provenance_verified=true`. The ordinary candidate manifest always says `release_eligible=false`:
its macOS export is ad-hoc signed and not notarized, and its Windows export has no Authenticode
signature. Checksums and provenance do not waive publisher identity/trust.

### Protected desktop signing

A manual `trusted-release` dispatch adds two `alpha-release` environment jobs after the ordinary
builds. They download the source-bound desktop archives and replace them only after signing and
verification:

- Windows imports a temporary PFX, matches the signer's SHA-256 to the approved repository
  variable, Authenticode-signs and timestamps every shipped EXE/DLL, requires `Valid` signatures and
  timestamp certificates, launch-smokes the signed executable, and removes the temporary certificate.
- macOS imports a Developer ID Application identity into a temporary keychain, matches its team ID,
  signs Mach-O contents inside-out with hardened runtime and secure timestamps, submits the ZIP with
  `notarytool`, staples the accepted ticket, verifies strict/deep code signing and Gatekeeper, and
  launch-smokes both universal slices.
- The protected assembly replaces the untrusted archives and trust records, creates fresh GitHub
  build-provenance attestations for the signed bytes, verifies them at the exact source SHA, and only
  then evaluates trusted-release metadata.

Configure the protected `alpha-release` environment with required reviewers and a main-branch
deployment policy. Its variables are `SPURFIRE_APPLE_TEAM_ID`,
`SPURFIRE_WINDOWS_CERTIFICATE_SHA256`, and optionally `SPURFIRE_WINDOWS_TIMESTAMP_URL` (the default
is DigiCert's HTTP Authenticode timestamp service). Its secrets are:

- `SPURFIRE_APPLE_CERTIFICATE_P12_BASE64`
- `SPURFIRE_APPLE_CERTIFICATE_PASSWORD`
- `SPURFIRE_APPLE_NOTARY_KEY_P8_BASE64`
- `SPURFIRE_APPLE_NOTARY_KEY_ID`
- `SPURFIRE_APPLE_NOTARY_ISSUER_ID`
- `SPURFIRE_WINDOWS_PFX_BASE64`
- `SPURFIRE_WINDOWS_PFX_PASSWORD`

Missing credentials, mismatched signer identities, absent timestamps, notarization rejection,
unstapled tickets, Gatekeeper rejection, or a signed-archive launch failure all fail closed. Never
place these credentials in repository variables or ordinary CI. Apple requires Developer ID signing,
hardened runtime, secure timestamps, and notarization for this distribution path; Microsoft documents
that Authenticode timestamps preserve signature validity after certificate expiry. See
[Apple notarization requirements](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution),
[Microsoft Authenticode signing](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.security/set-authenticodesignature),
and [GitHub protected environment secrets](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments).

## Secret-free playtest aggregation

The recorder writes append-only schema-v1 JSONL under the client log directory. Aggregate deterministically with:

```bash
scripts/aggregate-playtest.py --strict user-logs/*.jsonl > alpha-playtest-summary.json
```

The output includes player-session exposure; M2 dive, hit, landing, notification, and render metrics; M3 remount/duel metrics; M4 Spur/Charge metrics; and M5 winner pacing, score-category diversity, objective share, Most Wanted pressure, dead/encounter/objective time, worst convergence gap, and play-again rate. Input order does not affect output. Duplicate dive terminals or M5 results, inconsistent category totals, incomplete strict sessions, malformed numbers, and prohibited secret/topology fields fail closed.

Prohibited data includes OAuth material, capabilities, invitations/join codes, enrollment/auth keys, seeds, raw endpoints, CGNAT/ULA addresses, bearer values, URLs, and credentials. Aggregation is local only; this tooling adds no upload path.

## Two-client and lifecycle entry points

Credential-free simulated two-client orchestration uses:

```bash
scripts/run-alpha-two-client.sh \
  --client-a /downloads/client-a.zip \
  --client-b /downloads/client-b.zip \
  --driver /path/to/reviewed-simulated-driver \
  --output /tmp/alpha-lifecycle.json
```

The entry point refuses known credential environment variables. The driver receives only archive paths, exact source SHA, output path, and `SPURFIRE_ALPHA_TEST_MODE=simulated`. `scripts/check-alpha-lifecycle-evidence.py` then requires both independent downloads, exact matching roster/generations, measured-or-unknown health, coherent M2 markers, both leaves, and simulated cleanup. Simulated evidence is explicitly `release_qualifying=false`.

A separately authorized private-live harness is outside ordinary CI. Its redacted output can be checked with:

```bash
scripts/check-alpha-lifecycle-evidence.py --require-live private-live.json
```

Private-live validation additionally requires the control service to be absent from gameplay membership, two exact stable-ID-absence observations at least five seconds apart, verified vault erasure after absence, atomic `DEDICATED_ABSENT`, and lease release. The validator performs no mutation and rejects credential/topology material.

## Governed readiness states

These decisions are separate and none implies the next. Each decision must name its owner and bind immutable evidence to the exact source SHA:

| State | Decision owner | Required binding | Current state |
|---|---|---|---|
| **Source-ready** | source maintainer | successful exact-SHA CI and all client-preflight jobs/artifacts | **No — exact-head gates must complete successfully** |
| **Alpha-ready** | activation reviewer (not the PR author) | trusted-harness private-live artifact plus natural M2/telemetry evidence | **No — credentialed and human evidence is pending** |
| **Merge-ready** | independent PR reviewer | Source-ready and Alpha-ready evidence plus an exact-SHA approving review | **No — independent review and prior states are pending** |
| **Release-ready** | distinct release reviewer | merged source, distribution trust, completed manifest, and digest-bound approving review | **No — signing, notarization, evidence, and approvals are pending** |

The completed manifest records bindings; it does not create any of these decisions. The protected publisher resolves the configured private-live workflow run and artifact through GitHub, validates the artifact, and resolves activation and release approvals as distinct exact-SHA PR reviews. Repository variables `ALPHA_PRIVATE_LIVE_REPOSITORY` and `ALPHA_PRIVATE_LIVE_WORKFLOW` identify the separately governed external trusted harness; it cannot be this source repository. If that repository is not readable by `github.token`, the protected environment must provide a read-only `ALPHA_EVIDENCE_TOKEN`. Missing configuration, inaccessible external evidence, self-approval, stale/dismissed reviews, or duplicate reviewers fail closed.

## Terminal release gate

A stable tag is not a way to discover readiness. First qualify source commit **S** and record S in a reviewed `docs/release-evidence/<version>.json`. Commit only that manifest and its release notes in metadata commit **T**, then `scripts/check-release-tag-binding.sh <version> T` must prove S is an ancestor and S..T changes only those two files. The tag may identify T; all CI/client/live run IDs and artifact evidence remain bound to S. The manifest also binds launch smoke, SBOM/provenance, platform trust, activation, private-live cleanup, natural M2 playtest, telemetry, and independent approvals.

Tag-triggered package workflows validate but do not publish stable OCI aliases. Client publication is
a separate protected-environment dispatch requiring the independently reviewed evidence-manifest
SHA-256. It refuses any existing draft or published release rather than overwriting it. Ordinary
candidate manifests can never publish; a protected trusted-release candidate additionally requires
configured Apple/Windows credentials and successful live signing/notarization evidence.

GitHub forbids overriding `GITHUB_*` default variables, so build-provenance attestations always bind the workflow commit. Tag-triggered candidate runs therefore attest **T** and re-prove — inside the verifying job — that **T** adds only release metadata on top of **S**; non-tag runs attest **S** directly and assert the two SHAs are equal. Trusted-release eligibility and client publication each independently require **S** to be contained in the protected `main` branch, so a manual dispatch at an arbitrary off-main ref can never satisfy the evidence chain.
