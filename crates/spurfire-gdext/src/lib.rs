#![deny(unsafe_code)]

//! Spurfire's Godot-facing mounted gameplay controllers and pure simulation kernels.

pub mod archetype;
mod horse_controller;
pub mod locomotion;
pub mod mounted_weapon_controller;
mod peer_session;

// godot-rust requires this one unsafe marker to acknowledge the engine ABI.
// Unsafe code remains denied everywhere else in this crate.
#[allow(unsafe_code)]
mod entrypoint {
    use godot::prelude::*;

    struct SpurfireGdExtension;

    #[gdextension]
    unsafe impl ExtensionLibrary for SpurfireGdExtension {}
}
