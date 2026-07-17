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
Space jumps, and T resets. Escape releases the captured mouse; press
Escape again to quit. Left-click recaptures the mouse.

At rest, A/D performs a short horse-like lateral step; once moving, it blends into
rein steering. Select Courser/Warhorse/Mustang with 1/2/3 and toggle the attribute
panel with H. Fire the mounted rifle with Mouse 1, aim with Mouse 2, reload with R,
and switch rifles with 4/5/6. The course uses license-tracked CC0 subsets of Kenney
Nature Kit, Survival Kit, and Blaster Kit; see `../docs/asset-licenses.md` and
`../docs/combat-m1.md`.
