# Spurfire invited Alpha playtest

This bundle is for human gameplay testing. It is not a public or stable release. Pick the archive
for your computer, verify it against `SHA256SUMS`, and confirm the short build ID shown on the title
screen matches the beginning of `source_sha` in `candidate-manifest.json`.

## Start playing

- Linux x86_64: unpack `Spurfire-linux-x86_64.tar.gz`, then run `Spurfire.x86_64`.
- Linux ARM64: unpack `Spurfire-linux-arm64.tar.gz`, then run `Spurfire.arm64`.
- macOS: unzip `Spurfire-macos-universal.zip` and open `Spurfire.app`. This invited Alpha is ad-hoc
  signed, not notarized. If macOS blocks the first launch, control-click the app and choose **Open**,
  or approve it under **System Settings → Privacy & Security**.

Choose **PLAY ALPHA • OFFLINE BOUNTY RUN**. The 15-minute match starts immediately against three
local practice opponents and exercises the M2–M5 gameplay loop without an account, lobby, or network
credential. Click once in the arena to capture the mouse. Press Escape to release it; use the
on-screen Quit button to close cleanly and finish the telemetry session.

Core controls:

- W/S accelerate, brake, and reverse; A/D steer; Shift/Ctrl change gait; Space jumps; T resets.
- Mouse moves the camera; Mouse 1 fires; Mouse 2 aims; R reloads.
- E performs a Saddle Dive while mounted, grounded, and moving at least 8 m/s; E also remounts near
  your waiting horse. After a spook, W/A/S/D moves on foot, Shift sprints, and Ctrl crouches or rolls.
- Q spends full Spur on Majestic Charge while mounted or Majestic Return while horseless. The Alpha
  Bounty Run locks the Courser and Dustwalker loadout. Tab shows the scoreboard; F3 diagnostics.

The practice opponents are deterministic test actors, not a claim about finished bot AI. Their job
is to create moving targets, pressure your horse into the spook/on-foot/return loop, and make the
objective and scoring systems testable by one person. Treat their tactics separately from feedback
about controls, camera, combat readability, pacing, scoring, and whether the full loop is enjoyable.

Play naturally first and try to finish one Bounty Run. Useful written feedback includes camera comfort or motion sickness, control
clarity, whether Saddle Dive was discoverable, animation/reversal quality, match pacing, confusing
moments, and whether you wanted another round. Include the title-screen build ID with every report.

## Gameplay logs

The client writes secret-free M2/M3 gameplay JSONL under:

- Linux: `~/.local/share/godot/app_userdata/Spurfire/logs/`
- macOS: `~/Library/Application Support/Godot/app_userdata/Spurfire/logs/`

Keep `m2-*.jsonl` and `m3-*.jsonl`; `presentation-latest.jsonl` is not a gameplay aggregation input.
The bundled collector runs locally and does not upload anything:

```bash
python3 aggregate-playtest.py --strict "/path/to/Spurfire/logs" \
  --output alpha-playtest-summary.json
```

The strict collector rejects incomplete sessions, unmatched dives, an invalid build SHA, and any
credential or network-topology fields. If it reports an error, retain the original JSONL with the
build ID and report the failure instead of editing the log.
