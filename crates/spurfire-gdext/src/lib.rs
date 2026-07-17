#![deny(unsafe_code)]

//! Spurfire's Godot-facing horse controller and engine-independent locomotion.

pub mod locomotion;

use godot::prelude::*;

struct SpurfireGdExtension;

// godot-rust requires this one unsafe marker to acknowledge the engine ABI.
// Unsafe code remains denied everywhere else in this crate.
#[allow(unsafe_code)]
#[gdextension]
unsafe impl ExtensionLibrary for SpurfireGdExtension {}
