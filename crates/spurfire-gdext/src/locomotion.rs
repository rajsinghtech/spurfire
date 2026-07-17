//! Deterministic horse locomotion with no Godot types.

/// Discrete movement gait, ordered from stationary to fastest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(i32)]
pub enum Gait {
    #[default]
    Idle = 0,
    Walk = 1,
    Trot = 2,
    Gallop = 3,
}
