# Alpha playtest qualification

**Current boundary:** Alpha is the invited human-testing phase, not a public product release. A
source-bound, checksummed Linux/macOS candidate that passes the automated gates below is eligible to
enter Alpha. Human feel, gameplay telemetry, and natural multiplayer findings are collected during
Alpha and drive iteration; they are not prerequisites for producing the first Alpha build.

## Automated entry gates

Linux and macOS Godot qualification must emit each marker exactly once:

- `SPURFIRE_GODOT_SMOKE_OK`
- `SPURFIRE_POLISH_SMOKE_OK`
- `SPURFIRE_COMBAT_UI_SMOKE_OK`
- `SPURFIRE_ALPHA_LOBBY_SMOKE_OK`

`scripts/check-alpha-smoke-log.sh` rejects missing or duplicate markers and engine errors. The macOS
jobs permit only Godot 4.7.1's exact headless `RendererDummy` shader-RID teardown signature with the
observed allocation counts 1–3 tracked in
issue #17; every other engine, script, parse, ObjectDB, or smoke error remains fatal. These forced
scenarios protect the build from regressions. They do not pretend to measure whether the game is fun.

The ordinary `Client Preflight` path builds and launch-smokes:

- Linux x86_64;
- Linux ARM64; and
- macOS universal, with both Apple Silicon and x86_64 slices exercised.

Windows is outside the Alpha platform set. Its retained workflow job runs only for an explicit
future `trusted-release` dispatch and cannot block an ordinary PR, `main` build, or Alpha candidate.
The normal Rust CI matrix likewise covers Linux and macOS for this phase.

## Invited Alpha bundle

The `alpha-candidate-<sha>` artifact contains the three client archives, `SHA256SUMS`, deterministic
SPDX metadata, platform launch-smoke records, `candidate-manifest.json`, `PLAYTEST.md`, and the local
telemetry collector. A valid ordinary
manifest says:

- `candidate_mode: alpha-playtest`;
- `alpha_testing_eligible: true`;
- `human_evidence_status: collect_during_alpha`;
- `publication: invited_alpha_only`; and
- `release_eligible: false`.

Non-PR runs also request and verify GitHub build-provenance attestations. The Alpha bundle remains an
expiring GitHub Actions artifact rather than a GitHub Release. It may be shared with invited testers
who understand its test status. Linux archives are unsigned, and the macOS archive is ad-hoc signed;
no Apple Developer ID, notarization, Authenticode, Windows build, release tag, or public publisher is
required to start Alpha.

Each exported client stamps the manifest's full source SHA into the Godot project before its native
build and archive are produced. The title screen shows the short build ID, and the final archive
launch smoke verifies the full `SPURFIRE_BUILD_COMMIT=<sha>` marker. Playtest records therefore bind
directly to the candidate source instead of the development placeholder.

## Human and gameplay testing happens here

Alpha testers should play naturally before tuning against the forced smoke scenarios. The client
records secret-free schema-v1 JSONL in its log directory. Aggregate sessions locally with:

```bash
scripts/aggregate-playtest.py --strict user-logs > alpha-playtest-summary.json
```

Passing a log directory selects `m2-*.jsonl` and `m3-*.jsonl` and ignores presentation-only logs.
The summary measures M2 dives, airborne accuracy, landing danger, notifications, M3 remount and duel
behavior, M4 Spur/Charge use, M5 scoring pace and diversity, convergence gaps, and play-again rate.
Malformed or incomplete strict sessions and prohibited credential/topology fields fail closed.
Aggregation is local only; the repository supplies no automatic upload path.

Human review should additionally record camera comfort, animation and reversal quality, motion
sickness, control clarity, match pacing, and whether another round is desirable. Failing a target is
an Alpha finding and a reason to iterate—not evidence that the tester should never have received the
Alpha.

## Multiplayer paths

Credential-free simulated two-client orchestration remains available through
`scripts/run-alpha-two-client.sh`. Direct disposable-tailnet tooling has separately proved real
one-use client enrollment, Direct/DERP transport, migration, eight-peer routing, a fifteen-minute
changing-transform soak, and exact child deletion.

The protected hosted one-lobby deployment is an optional managed Alpha path with a higher security
bar. Enabling it still requires explicit owner authorization, the offline owner key, reviewed ingress
and operations, and integrated restrictive-policy/key/cleanup evidence. Those controls gate hosted
provider mutation; they do not gate local or directly coordinated human gameplay testing with the
checksummed Alpha bundle.

## Future stable release is separate

The repository retains dormant protected-release and evidence machinery for a future public release.
That later project may reintroduce Windows, Developer ID/notarization, Authenticode, signed OCI
artifacts, reviewed private-live lifecycle evidence, and publication approvals. None of those items is
an Alpha entry criterion, and Alpha progress must not be reported as blocked on them.
