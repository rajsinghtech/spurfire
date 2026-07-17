# Spurfire Godot graybox

Godot **4.7.1** text-only M0 test course for the native `HorseController` GDExtension.
No movement is implemented in GDScript: horse locomotion, gait transitions, jumping,
reset, rough/slope response, and telemetry belong to the Rust native class.

## Run

Build the native library into the platform path listed in
`bin/spurfire.gdextension`, then:

```sh
godot --path game
godot --headless --path game --scene res://scenes/headless_smoke.tscn
```

Without a native library, the normal bootstrap displays a clear diagnostic and the
smoke test intentionally exits nonzero. Telemetry is shown in the HUD and written
to `user://m0_telemetry.csv`.

Controls: W naturally accelerates through walk/trot/gallop; S brakes, pauses, then
reverses. A/D steer, Shift/Ctrl optionally change gait, Option/Alt hard-brakes,
Space jumps, and R resets. Escape releases the captured mouse; press
Escape again to quit. Left-click recaptures the mouse.

At rest, A/D performs a short horse-like lateral step; once moving, it blends into
rein steering. Select Courser/Warhorse/Mustang with 1/2/3 and toggle the attribute
panel with H. The course uses a minimal, license-tracked CC0 subset of Kenney Nature
Kit and Survival Kit; see `../docs/asset-licenses.md`.
