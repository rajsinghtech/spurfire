# Spurfire 0.2.0

Saddle Dive is Spurfire's M2 release candidate: a deterministic flying dismount built on the mounted movement, rifle, and live peer-networking prototype.

## Saddle Dive

- Press the existing **E / combat interact** input while mounted, grounded, and moving at least 8 m/s. There is no separate dive button or cooldown.
- Rider choice is clamped to a ±75° cone from current planar velocity. The launch preserves momentum, adds a 6.0 m/s horizontal impulse and 6.0 m/s vertical pop, and uses the shipped 22.0 m/s² gravity.
- Dive-only airborne fire gets deterministic sway scaling from ×0.6 back to ×1.0 over the final 20% of nominal airtime. Ordinary jump-air fire remains rejected.
- Per-dive shot caps are Longspur 1, Dustwalker 3, and Rattler 5. Reloading and weapon switching cannot bypass a cap, and airborne reload is forbidden.
- The original horse keeps its identity, runs out for at most 25 m while decelerating over two seconds, then waits for a nearby remount. It is never teleported back.
- Landing costs 0.4 seconds prone plus 0.4 seconds at half movement speed, with firing blocked throughout. Slopes over 30° add 15 damage and 0.4 seconds of prone recovery.
- Secret-free deterministic events now drive the exact notifications **FLYING DISMOUNT**, **SADDLE DIVE HEADSHOT**, **FULL-GALLOP HIT**, and **AIRBORNE REVERSAL**. They award no points; M5 owns scoring.

## Determinism, wire compatibility, and telemetry

- The movement/combat kernel uses one absolute 60 Hz simulation tick and fixed, replayable launch, recovery, horse-runout, event, and instrumentation rules. Gameplay state does not use wall-clock time.
- Rider snapshots advance wire compatibility from 1.0 to 1.1 with a numeric stance field. Missing legacy stance defaults to Mounted; unknown future stance IDs are preserved but grant no gameplay capability.
- Every accepted dive records launch speed and angle, airtime, attempts and accepted shots, hits and damage, landing terrain and slope, death within the inclusive three-second window, and time to remount. Finalized/censored rows append to secret-free per-session JSONL under `user://logs`; no credentials, capabilities, join material, seeds, or endpoints are allowed in that recorder.
- Local physics interpolation and render-time camera/rider sampling smooth 60 Hz transforms on high-refresh displays without changing gameplay state. Teleport/reset guards clear interpolation history, and stance-aware camera anchors preserve the no-kick/no-dip/no-roll comfort contract at the shipped 70→78 FOV.
- Reload HUD state now follows native active ticks and visibly reports airborne/recovery/holstered rejection. The integrated smoke proves the observed `0 | 107` case after a real dive/remount completes as `30 | 77` at tick +126.
- A camera-relative preview reports the exact kernel-clamped direction and amber clamp state while dive-ready. The launch cone remains ±75°; this is feedback, not a geometry change.
- Candidate qualification covers Rust source gates on Ubuntu, macOS, and Windows; bounded Godot 4.7.1 gameplay plus one-lobby smoke on Linux; deterministic secret-free telemetry aggregation; and nonpublishing checksummed/attested Linux x86_64, Windows x86_64, and macOS universal client exports.
- Candidate artifacts are not releases. Ordinary macOS output is ad-hoc signed/not notarized and
  ordinary Windows output is unsigned. Only the protected trusted-release dispatch can replace them
  with verified Developer ID/notarized and timestamped Authenticode artifacts; credentials and live
  signing evidence remain activation requirements.

## Playtest status

**Implementation complete / playtest pending.** Automated deterministic and forced headless scenarios do not satisfy population or feel gates. M2 is not “done” until natural play demonstrates all of the following:

- 2–4 dives per player in a 15-minute match;
- airborne hit rate 25–40% above the gallop baseline;
- 25–40% of dives ending in death within three seconds after landing;
- all four notifications occurring naturally within three matches; and
- reversal presentation landing without animation popping.

The ±75° launch cone intentionally remains locked. For M2, **AIRBORNE REVERSAL** means an authority-confirmed dive hit fired with a horizontal direction opposed to pre-launch velocity. The older “behind launch” acceptance wording is geometrically incompatible with the cone and remains blocked for a product-documentation correction; this release does not silently widen the cone.

Top feel risks remain dive spam versus non-use (recovery is the first tuning dial), graybox pose quality, and camera sickness. The presentation adds no dive FOV kick, landing dip, roll, shake, or forced yaw recenter. Automated integration now covers interpolated 60/120/144 Hz sampling, the exact post-remount `0 | 107` reload, and exact ±75° preview vectors/clamp feedback. The original hands-on observations are not erased by those fixes: blind 60/144 Hz comfort, five-tester discoverability, and natural-match qualification remain required.

## Not included

M3 on-foot combat and Tactical Roll, M4 Spur, M5 Bounty Run scoring, and the remaining M6 authority-simulation/migration/keyframe/rewind work are not part of 0.2.0. The public control service remains a prototype: it has no user accounts or ranked-result trust model, and its encrypted file vault/reconciler still lacks the approved production setec/workload-identity, audit, backup, rotation, fencing, and private-operator posture. Hosted deployment remains zero-mutation dry-run.

Do not tag or publish 0.2.0 until the managed one-lobby client, security activation, exact cleanup, coherent peer-hosted M2, telemetry, natural-play, artifact trust, and release-governance gates all pass at one reviewed source SHA. Tag-triggered package jobs validate but do not publish stable OCI aliases. The protected manual client publisher requires an independently reviewed release-evidence digest, refuses unsigned/unnotarized candidates, and will not overwrite an existing draft or published release.
