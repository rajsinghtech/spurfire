# spurfire-gdext

Rust GDExtension containing Spurfire's deterministic horse-locomotion kernel and
its thin Godot `HorseController` adapter.

## Compatibility

This crate pins `godot` (godot-rust/gdext) **0.5.4** with its `api-4-7`
feature. Version 0.5.4 is the first tagged crates.io release with the Godot 4.7
API, and a 4.7 API extension is ABI-compatible with the target Godot 4.7.1
runtime. It deliberately does not track git HEAD. godot-rust 0.5.4 requires
Rust 1.94 or newer, so this crate's MSRV is 1.94 even though the control-plane
workspace's other crates currently support Rust 1.91.
