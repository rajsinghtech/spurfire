#![deny(unsafe_code)]

//! Spurfire's Godot-facing mounted gameplay controllers and pure simulation kernels.

pub mod archetype;
mod horse_controller;
mod lobby_client;
pub mod locomotion;
mod m3_gameplay_controller;
pub mod mounted_weapon_controller;
mod network_rider;
mod peer_session;
mod saddle_dive_controller;

// godot-rust requires this one unsafe marker to acknowledge the engine ABI.
// Unsafe code remains denied everywhere else in this crate.
#[allow(unsafe_code)]
mod entrypoint {
    use godot::prelude::*;

    struct SpurfireGdExtension;

    #[gdextension]
    unsafe impl ExtensionLibrary for SpurfireGdExtension {}
}
