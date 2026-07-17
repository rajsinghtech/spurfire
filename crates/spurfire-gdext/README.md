# spurfire-gdext

Rust GDExtension containing Spurfire's deterministic horse-locomotion kernel and
its thin Godot `HorseController` adapter.

## Compatibility

This crate pins `godot` (godot-rust/gdext) **0.5.4** with its `api-4-7`
feature. Version 0.5.4 is the first tagged crates.io release with the Godot 4.7
API, and a 4.7 API extension is ABI-compatible with the target Godot 4.7.1
runtime. It deliberately does not track git HEAD. godot-rust 0.5.4 requires
Rust 1.94 or newer, so this crate's MSRV is 1.94 even though the control-plane
workspace's other crates currently support Rust 1.91. The produced library is
named `spurfire_godot` to match `game/bin/spurfire.gdextension`. Unsafe Rust is
denied crate-wide; the tiny entrypoint module narrowly permits the mandatory
`unsafe impl ExtensionLibrary` ABI acknowledgement required by godot-rust.

## M0.5 handling contract

`HorseController` defaults to Mustang (`archetype == 2`). Call
`set_archetype(0|1|2)` for Courser, Warhorse, or Mustang, read the immutable row
with `get_archetype_stats()`, and observe `archetype_changed(old, new)`. The
`archetype` property is read-only. Vitality, mass, and stagger threshold are
attributes only; combat is intentionally not implemented.

A/D sidesteps only while standing below 1 m/s with no forward command. The
kernel integrates signed lateral velocity, settles it before W or reverse can
accelerate, and forces it out at speed so reins remain turning rather than
shooter strafing. Every 10 Hz telemetry sample includes `archetype`,
`lateral_speed_mps`, and `max_vitality`.
