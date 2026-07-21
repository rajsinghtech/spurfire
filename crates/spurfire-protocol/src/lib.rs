//! Engine-independent wire types and deterministic lobby rules for Spurfire.
//!
//! This crate is intentionally pure Rust and contains no game-engine, runtime,
//! networking, or platform dependency. Protocol decisions that must agree on
//! every peer—wire compatibility, lifecycle transitions, map radius, authority
//! scoring, tie-breaking, and canonical input hashing—live here.

#![forbid(unsafe_code)]

mod api;
mod combat;
mod credential;
mod election;
mod id;
mod m3;
mod m3_combat;
mod m4;
mod model;
mod network_inspection;
mod radius;
mod saddle_dive;
mod session;
mod stance;
mod state;
mod time;
mod version;

pub use api::*;
pub use combat::*;
pub use credential::{JoinCredential, DRY_RUN_AUTH_KEY};
pub use election::*;
pub use id::{IdValidationError, LobbyId, PlayerId};
pub use m3::*;
pub use m3_combat::*;
pub use m4::*;
pub use model::*;
pub use network_inspection::*;
pub use radius::{playable_radius_m, MAX_PLAYABLE_RADIUS_M, MIN_PLAYABLE_RADIUS_M};
pub use saddle_dive::*;
pub use session::*;
pub use stance::RiderStance;
pub use state::{LobbyState, LobbyTransitionError};
pub use time::UnixMillis;
pub use version::{
    is_wire_compatible, WireCompatibility, WireVersion, WireVersionMismatch, WireVersionParseError,
    CURRENT_WIRE_VERSION, WIRE_VERSION,
};
