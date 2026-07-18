# Alpha release qualification tooling

**Current verdict: NO-GO.** This document describes candidate-only automation. It does not authorize hosted real mutations, platform publication, or a `v0.2.0` tag.

## CI gates

Linux Godot qualification must emit each marker exactly once:

- `SPURFIRE_GODOT_SMOKE_OK`
- `SPURFIRE_POLISH_SMOKE_OK`
- `SPURFIRE_COMBAT_UI_SMOKE_OK`
- `SPURFIRE_ALPHA_LOBBY_SMOKE_OK`

`scripts/check-alpha-smoke-log.sh` rejects missing/duplicate markers and engine errors. The final marker is intentionally absent from the baseline graybox: the CI gate stays red until the integrated one-lobby Create/Join/roster/health/Leave/teardown smoke exists. Existing gameplay smoke remains the regression gate for M0–M2.

Release/check tooling has credential-free unit tests under `scripts/tests/`. Tests cover deterministic playtest aggregation, secret canaries, simulated versus private-live lifecycle evidence, exact cleanup ordering, platform trust blockers, and no-overwrite candidate metadata.

## Nonpublishing client candidates

`Client Preflight` builds on GitHub-hosted Linux, Windows, and macOS runners. It never creates a release, tag, package, or deployment. Each platform artifact contains its archive, SHA-256, launch-smoke result, and trust status. The final `alpha-candidate-<sha>` artifact combines:

- all three archives;
- `SHA256SUMS`;
- `candidate.spdx.json`;
- `candidate-manifest.json`; and
- platform trust records.

Non-PR runs request GitHub build-provenance attestations and verify them before marking `provenance_verified=true`. The candidate manifest still says `release_eligible=false`: current macOS export is ad-hoc signed and not notarized, and current Windows export has no Authenticode signature. Checksums and provenance do not waive publisher identity/trust.

## Secret-free playtest aggregation

The recorder writes append-only schema-v1 JSONL under the client log directory. Aggregate deterministically with:

```bash
scripts/aggregate-playtest.py --strict user-logs/*.jsonl > alpha-playtest-summary.json
```

The output includes player-session exposure, dives per player per 15 minutes, airborne and gallop hit rates/uplift, landing-window deaths, clamp share, median remount time, all-four notification coverage, reload rejection modes, and per-refresh render/jerk summaries. Input order does not affect output. Duplicate dive terminals, incomplete strict sessions, malformed numbers, and prohibited secret/topology fields fail closed.

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

## Terminal release gate

A stable tag is not a way to discover readiness. Before tagging, a reviewed exact-SHA `docs/release-evidence/<version>.json` must pass `scripts/check-alpha-evidence.py`. The manifest binds CI/client/live run IDs, artifact hashes, launch smoke, SBOM/provenance, platform trust, activation, private-live cleanup, natural M2 playtest, telemetry, and independent approvals.

Tag-triggered package workflows validate but do not publish stable OCI aliases. Client publication is a separate protected-environment dispatch requiring the independently reviewed evidence-manifest SHA-256. It refuses any existing draft or published release rather than overwriting it. Current candidate manifests cannot publish because Apple Developer ID/notarization and Windows Authenticode are absent.
