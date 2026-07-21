//! Deterministic horse locomotion with no Godot types.
//!
//! The kernel owns command/state transitions and computes a requested velocity.
//! An engine adapter may call [`HorseKernel::resolve_motion`] after collision
//! resolution to feed the actual transform and velocity back into the kernel.

use std::f64::consts::PI;
use std::ops::{Add, AddAssign, Mul, Sub};

use spurfire_protocol::{CHARGE_ACCEL_MULTIPLIER_MILLI, CHARGE_TURN_MULTIPLIER_MILLI};

use crate::archetype::{HorseArchetype, HorseStats};

const SIDESTEP_FORWARD_LIMIT_MPS: f64 = 1.0;
const SIDESTEP_RAMP_OUT_S: f64 = 0.15;
const SIDESTEP_BLOCK_SLOPE_DEGREES: f64 = 25.0;
const SIDESTEP_LANDING_RECOVERY_S: f64 = 0.2;
const SIDESTEP_EPSILON_MPS: f64 = 1.0e-6;

/// Small engine-independent 3D vector used at the Godot boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn dot(self, rhs: Self) -> f64 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    #[must_use]
    pub fn horizontal_length(self) -> f64 {
        self.x.hypot(self.z)
    }

    #[must_use]
    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    #[must_use]
    pub fn normalized_horizontal(self) -> Self {
        let length = self.horizontal_length();
        if length <= f64::EPSILON || !length.is_finite() {
            Self::ZERO
        } else {
            Self::new(self.x / length, 0.0, self.z / length)
        }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Add for Vec3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Vec3 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl Mul<f64> for Vec3 {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

/// Discrete movement gait, ordered from stationary to fastest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(i64)]
pub enum Gait {
    #[default]
    Idle = 0,
    Walk = 1,
    Trot = 2,
    Gallop = 3,
}

impl Gait {
    #[must_use]
    pub const fn up(self) -> Self {
        match self {
            Self::Idle => Self::Walk,
            Self::Walk => Self::Trot,
            Self::Trot | Self::Gallop => Self::Gallop,
        }
    }

    #[must_use]
    pub const fn down(self) -> Self {
        match self {
            Self::Idle | Self::Walk => Self::Idle,
            Self::Trot => Self::Walk,
            Self::Gallop => Self::Trot,
        }
    }
}

/// Why a gait transition occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReason {
    PlayerUp,
    PlayerDown,
    /// Forward input from rest automatically enters Walk so the primary movement key always moves.
    ThrottleStart,
    AutoUpshift,
    AutoDownshift,
    Reset,
}

/// A transition to emit through the engine-facing `gait_changed` signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GaitTransition {
    pub old: Gait,
    pub new: Gait,
    pub reason: TransitionReason,
}

/// Player commands sampled once for a physics tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InputFrame {
    /// Forward intent in the range `0..=1`.
    pub throttle: f64,
    /// Reverse/soft-brake intent in the range `0..=1`.
    pub brake: f64,
    /// Right-positive steering intent in the range `-1..=1`.
    pub steer: f64,
    pub hard_brake: bool,
    pub gait_up: bool,
    pub gait_down: bool,
    pub jump_pressed: bool,
    pub reset: bool,
}

impl InputFrame {
    fn sanitized(self) -> Self {
        Self {
            throttle: finite_or_zero(self.throttle).clamp(0.0, 1.0),
            brake: finite_or_zero(self.brake).clamp(0.0, 1.0),
            steer: finite_or_zero(self.steer).clamp(-1.0, 1.0),
            ..self
        }
    }
}

/// Terrain category used by the archetype rough-ground table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerrainSurface {
    #[default]
    Flat,
    Scrub,
    Mud,
    Riverbed,
}

/// Why a requested stationary sidestep was unavailable this tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(i64)]
pub enum SidestepBlockReason {
    #[default]
    None = 0,
    ForwardInput = 1,
    ForwardMotion = 2,
    Airborne = 3,
    LandingRecovery = 4,
    OffCamber = 5,
    PowerTurn = 6,
    Stagger = 7,
}

impl SidestepBlockReason {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ForwardInput => "forward_input",
            Self::ForwardMotion => "forward_motion",
            Self::Airborne => "airborne",
            Self::LandingRecovery => "landing_recovery",
            Self::OffCamber => "off_camber",
            Self::PowerTurn => "power_turn",
            Self::Stagger => "stagger",
        }
    }
}

/// Collision and terrain observations for one physics tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    pub grounded: bool,
    /// Backward-compatible rough marker. When no explicit surface is supplied,
    /// this is interpreted as scrub.
    pub rough: bool,
    pub terrain: TerrainSurface,
    /// Absolute floor angle for telemetry.
    pub slope_angle_radians: f64,
    /// Grade along the horse's heading. Positive is uphill.
    pub signed_slope_radians: f64,
    /// Whether a non-floor slope should force a controlled slide.
    pub steep_slope: bool,
    /// Horizontal downhill direction when [`Self::steep_slope`] is true.
    pub downhill_direction: Vec3,
    /// Reserved movement-state gates; neither implements combat behavior.
    pub power_turning: bool,
    pub staggered: bool,
    /// Headshot stagger remains effective during Majestic Charge.
    pub headshot_staggered: bool,
}

impl Environment {
    #[must_use]
    const fn effective_terrain(self) -> TerrainSurface {
        match (self.terrain, self.rough) {
            (TerrainSurface::Flat, true) => TerrainSurface::Scrub,
            (terrain, _) => terrain,
        }
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            grounded: true,
            rough: false,
            terrain: TerrainSurface::Flat,
            slope_angle_radians: 0.0,
            signed_slope_radians: 0.0,
            steep_slope: false,
            downhill_direction: Vec3::ZERO,
            power_turning: false,
            staggered: false,
            headshot_staggered: false,
        }
    }
}

/// Editor-facing locomotion values. Rates are per second, not per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub walk_speed: f64,
    pub trot_speed: f64,
    pub gallop_speed: f64,
    pub reverse_speed: f64,
    pub walk_acceleration: f64,
    pub trot_acceleration: f64,
    pub gallop_acceleration: f64,
    pub coast_deceleration: f64,
    pub soft_brake_deceleration: f64,
    pub hard_brake_deceleration: f64,
    pub idle_yaw_rate_degrees: f64,
    pub walk_yaw_rate_degrees: f64,
    pub trot_yaw_rate_degrees: f64,
    pub gallop_yaw_rate_degrees: f64,
    pub steering_response: f64,
    pub walk_trot_lateral_damping: f64,
    pub gallop_lateral_damping: f64,
    pub auto_downshift_delay: f64,
    pub idle_speed_floor: f64,
    pub walk_speed_floor: f64,
    pub trot_speed_floor: f64,
    pub gallop_speed_floor: f64,
    pub gravity: f64,
    pub terminal_fall_speed: f64,
    pub jump_impulse: f64,
    pub coyote_time: f64,
    pub jump_buffer_time: f64,
    pub air_control: f64,
    pub rough_speed_multiplier: f64,
    pub rough_acceleration_multiplier: f64,
    pub uphill_reference_degrees: f64,
    pub uphill_speed_multiplier: f64,
    pub downhill_speed_bonus: f64,
    pub max_climb_degrees: f64,
    pub steep_slide_speed: f64,
    pub floor_snap_length: f64,
    pub kill_plane_y: f64,
    pub telemetry_interval: f64,
    pub maximum_dt: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            // Editor values are the Courser reference row. HorseKernel applies the active
            // archetype's immutable ratios, yielding the exact design defaults while still
            // allowing deliberate global tuning overrides.
            walk_speed: 2.0,
            trot_speed: 5.0,
            gallop_speed: 14.5,
            reverse_speed: 1.5,
            walk_acceleration: 14.5 / 3.0,
            trot_acceleration: 14.5 / 3.0,
            gallop_acceleration: 14.5 / 3.0,
            coast_deceleration: 0.8,
            soft_brake_deceleration: 6.0,
            hard_brake_deceleration: 8.0,
            idle_yaw_rate_degrees: 90.0,
            walk_yaw_rate_degrees: 140.0,
            trot_yaw_rate_degrees: 120.8,
            gallop_yaw_rate_degrees: 60.0,
            steering_response: 6.0,
            walk_trot_lateral_damping: 8.0,
            gallop_lateral_damping: 3.0,
            auto_downshift_delay: 0.4,
            idle_speed_floor: 0.3,
            walk_speed_floor: 1.2,
            trot_speed_floor: 3.0,
            gallop_speed_floor: 7.0,
            gravity: 8.0 * 1.8 / (0.7 * 0.7),
            terminal_fall_speed: 30.0,
            jump_impulse: 4.0 * 1.8 / 0.7,
            coyote_time: 0.15,
            jump_buffer_time: 0.12,
            air_control: 0.2,
            rough_speed_multiplier: 0.90,
            rough_acceleration_multiplier: 0.90,
            uphill_reference_degrees: 30.0,
            uphill_speed_multiplier: 0.55,
            downhill_speed_bonus: 0.15,
            max_climb_degrees: 40.0,
            steep_slide_speed: 4.0,
            floor_snap_length: 0.4,
            kill_plane_y: -20.0,
            telemetry_interval: 0.1,
            maximum_dt: 0.25,
        }
    }
}

impl Tuning {
    /// Replace malformed editor values with safe defaults and clamp ratios.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let defaults = Self::default();
        Self {
            walk_speed: positive_or(self.walk_speed, defaults.walk_speed),
            trot_speed: positive_or(self.trot_speed, defaults.trot_speed),
            gallop_speed: positive_or(self.gallop_speed, defaults.gallop_speed),
            reverse_speed: positive_or(self.reverse_speed, defaults.reverse_speed),
            walk_acceleration: positive_or(self.walk_acceleration, defaults.walk_acceleration),
            trot_acceleration: positive_or(self.trot_acceleration, defaults.trot_acceleration),
            gallop_acceleration: positive_or(
                self.gallop_acceleration,
                defaults.gallop_acceleration,
            ),
            coast_deceleration: positive_or(self.coast_deceleration, defaults.coast_deceleration),
            soft_brake_deceleration: positive_or(
                self.soft_brake_deceleration,
                defaults.soft_brake_deceleration,
            ),
            hard_brake_deceleration: positive_or(
                self.hard_brake_deceleration,
                defaults.hard_brake_deceleration,
            ),
            idle_yaw_rate_degrees: positive_or(
                self.idle_yaw_rate_degrees,
                defaults.idle_yaw_rate_degrees,
            ),
            walk_yaw_rate_degrees: positive_or(
                self.walk_yaw_rate_degrees,
                defaults.walk_yaw_rate_degrees,
            ),
            trot_yaw_rate_degrees: positive_or(
                self.trot_yaw_rate_degrees,
                defaults.trot_yaw_rate_degrees,
            ),
            gallop_yaw_rate_degrees: positive_or(
                self.gallop_yaw_rate_degrees,
                defaults.gallop_yaw_rate_degrees,
            ),
            steering_response: positive_or(self.steering_response, defaults.steering_response),
            walk_trot_lateral_damping: positive_or(
                self.walk_trot_lateral_damping,
                defaults.walk_trot_lateral_damping,
            ),
            gallop_lateral_damping: positive_or(
                self.gallop_lateral_damping,
                defaults.gallop_lateral_damping,
            ),
            auto_downshift_delay: positive_or(
                self.auto_downshift_delay,
                defaults.auto_downshift_delay,
            ),
            idle_speed_floor: positive_or(self.idle_speed_floor, defaults.idle_speed_floor),
            walk_speed_floor: positive_or(self.walk_speed_floor, defaults.walk_speed_floor),
            trot_speed_floor: positive_or(self.trot_speed_floor, defaults.trot_speed_floor),
            gallop_speed_floor: positive_or(self.gallop_speed_floor, defaults.gallop_speed_floor),
            gravity: positive_or(self.gravity, defaults.gravity),
            terminal_fall_speed: positive_or(
                self.terminal_fall_speed,
                defaults.terminal_fall_speed,
            ),
            jump_impulse: positive_or(self.jump_impulse, defaults.jump_impulse),
            coyote_time: nonnegative_or(self.coyote_time, defaults.coyote_time),
            jump_buffer_time: nonnegative_or(self.jump_buffer_time, defaults.jump_buffer_time),
            air_control: finite_or(self.air_control, defaults.air_control).clamp(0.0, 1.0),
            rough_speed_multiplier: finite_or(
                self.rough_speed_multiplier,
                defaults.rough_speed_multiplier,
            )
            .clamp(0.05, 1.0),
            rough_acceleration_multiplier: finite_or(
                self.rough_acceleration_multiplier,
                defaults.rough_acceleration_multiplier,
            )
            .clamp(0.05, 1.0),
            uphill_reference_degrees: positive_or(
                self.uphill_reference_degrees,
                defaults.uphill_reference_degrees,
            ),
            uphill_speed_multiplier: finite_or(
                self.uphill_speed_multiplier,
                defaults.uphill_speed_multiplier,
            )
            .clamp(0.05, 1.0),
            downhill_speed_bonus: finite_or(
                self.downhill_speed_bonus,
                defaults.downhill_speed_bonus,
            )
            .clamp(0.0, 1.0),
            max_climb_degrees: finite_or(self.max_climb_degrees, defaults.max_climb_degrees)
                .clamp(1.0, 89.0),
            steep_slide_speed: positive_or(self.steep_slide_speed, defaults.steep_slide_speed),
            floor_snap_length: nonnegative_or(self.floor_snap_length, defaults.floor_snap_length),
            kill_plane_y: finite_or(self.kill_plane_y, defaults.kill_plane_y),
            telemetry_interval: positive_or(self.telemetry_interval, defaults.telemetry_interval),
            maximum_dt: positive_or(self.maximum_dt, defaults.maximum_dt),
        }
    }

    #[must_use]
    pub const fn target_speed(self, gait: Gait) -> f64 {
        match gait {
            Gait::Idle => 0.0,
            Gait::Walk => self.walk_speed,
            Gait::Trot => self.trot_speed,
            Gait::Gallop => self.gallop_speed,
        }
    }

    #[must_use]
    pub const fn acceleration(self, gait: Gait) -> f64 {
        match gait {
            Gait::Idle | Gait::Walk => self.walk_acceleration,
            Gait::Trot => self.trot_acceleration,
            Gait::Gallop => self.gallop_acceleration,
        }
    }

    #[must_use]
    pub const fn yaw_rate_degrees(self, gait: Gait) -> f64 {
        match gait {
            Gait::Idle => self.idle_yaw_rate_degrees,
            Gait::Walk => self.walk_yaw_rate_degrees,
            Gait::Trot => self.trot_yaw_rate_degrees,
            Gait::Gallop => self.gallop_yaw_rate_degrees,
        }
    }

    #[must_use]
    pub const fn speed_floor(self, gait: Gait) -> f64 {
        match gait {
            Gait::Idle => self.idle_speed_floor,
            Gait::Walk => self.walk_speed_floor,
            Gait::Trot => self.trot_speed_floor,
            Gait::Gallop => self.gallop_speed_floor,
        }
    }
}

/// Mutable deterministic simulation state.
#[derive(Debug, Clone, PartialEq)]
pub struct HorseState {
    pub gait: Gait,
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw_radians: f64,
    /// Canonical longitudinal component, signed positive in local forward.
    pub forward_speed_mps: f64,
    /// Canonical right-positive sidestep component used by future reconciliation.
    pub lateral_speed_mps: f64,
    pub steering_axis: f64,
    pub yaw_rate_radians: f64,
    pub acceleration_mps2: f64,
    pub slope_angle_radians: f64,
    pub rough: bool,
    pub terrain: TerrainSurface,
    pub terrain_speed_multiplier: f64,
    pub grounded: bool,
    pub air_time: f64,
    pub sidestep_blocked_reason: SidestepBlockReason,
    coyote_remaining: f64,
    jump_buffer_remaining: f64,
    landing_recovery_remaining: f64,
    low_speed_time: f64,
    reverse_hold_time: f64,
    telemetry_elapsed: f64,
}

impl HorseState {
    #[must_use]
    pub fn horizontal_speed(&self) -> f64 {
        self.velocity.horizontal_length()
    }

    #[must_use]
    pub fn forward_speed_abs(&self) -> f64 {
        self.forward_speed_mps.abs()
    }

    #[must_use]
    pub const fn coyote_remaining(&self) -> f64 {
        self.coyote_remaining
    }

    #[must_use]
    pub const fn jump_buffer_remaining(&self) -> f64 {
        self.jump_buffer_remaining
    }

    #[must_use]
    pub const fn landing_recovery_remaining(&self) -> f64 {
        self.landing_recovery_remaining
    }
}

/// Stable snapshot used by the Godot telemetry adapter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Telemetry {
    pub archetype: HorseArchetype,
    pub speed_mps: f64,
    pub speed_kmh: f64,
    pub lateral_speed_mps: f64,
    pub max_vitality: f64,
    pub gait: Gait,
    pub acceleration_mps2: f64,
    pub yaw_rate_degrees: f64,
    pub slope_angle_degrees: f64,
    pub rough: bool,
    pub terrain: TerrainSurface,
    pub position: Vec3,
    pub is_airborne: bool,
    pub air_time: f64,
    pub speed_fraction: f64,
    pub turn_radius_m: f64,
    pub sidestep_blocked_reason: SidestepBlockReason,
}

/// Output for one accepted simulation step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepOutcome {
    pub velocity: Vec3,
    pub yaw_radians: f64,
    pub gait_transition: Option<GaitTransition>,
    pub jumped: bool,
    pub reset: bool,
    pub telemetry_due: bool,
}

/// Invalid time-step rejection. Invalid steps never mutate state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepError {
    NonFiniteDelta,
    NonPositiveDelta,
    DeltaTooLarge { delta: f64, maximum: f64 },
}

/// Pure deterministic horse locomotion kernel.
#[derive(Debug, Clone, PartialEq)]
pub struct HorseKernel {
    tuning: Tuning,
    archetype: HorseArchetype,
    state: HorseState,
    spawn_position: Vec3,
    spawn_yaw_radians: f64,
    majestic_charge_active: bool,
}

impl HorseKernel {
    #[must_use]
    pub fn new(tuning: Tuning, spawn_position: Vec3, spawn_yaw_radians: f64) -> Self {
        let tuning = tuning.sanitized();
        let spawn_position = if spawn_position.is_finite() {
            spawn_position
        } else {
            Vec3::ZERO
        };
        let spawn_yaw_radians = finite_or_zero(spawn_yaw_radians);
        Self {
            tuning,
            archetype: HorseArchetype::default(),
            state: Self::fresh_state(tuning, spawn_position, spawn_yaw_radians),
            spawn_position,
            spawn_yaw_radians,
            majestic_charge_active: false,
        }
    }

    fn fresh_state(tuning: Tuning, position: Vec3, yaw_radians: f64) -> HorseState {
        HorseState {
            gait: Gait::Idle,
            position,
            velocity: Vec3::ZERO,
            yaw_radians,
            forward_speed_mps: 0.0,
            lateral_speed_mps: 0.0,
            steering_axis: 0.0,
            yaw_rate_radians: 0.0,
            acceleration_mps2: 0.0,
            slope_angle_radians: 0.0,
            rough: false,
            terrain: TerrainSurface::Flat,
            terrain_speed_multiplier: 1.0,
            grounded: true,
            air_time: 0.0,
            sidestep_blocked_reason: SidestepBlockReason::None,
            coyote_remaining: tuning.coyote_time,
            jump_buffer_remaining: 0.0,
            landing_recovery_remaining: 0.0,
            low_speed_time: 0.0,
            reverse_hold_time: 0.0,
            telemetry_elapsed: 0.0,
        }
    }

    #[must_use]
    pub const fn tuning(&self) -> &Tuning {
        &self.tuning
    }

    pub fn set_tuning(&mut self, tuning: Tuning) {
        self.tuning = tuning.sanitized();
        self.clamp_canonical_motion();
    }

    #[must_use]
    pub const fn archetype(&self) -> HorseArchetype {
        self.archetype
    }

    #[must_use]
    pub fn archetype_stats(&self) -> &'static HorseStats {
        self.archetype.stats()
    }

    /// Switch the immutable active row and clamp canonical movement to its caps.
    /// Returns `false` when the requested row was already active.
    pub fn set_archetype(&mut self, archetype: HorseArchetype) -> bool {
        if archetype == self.archetype {
            return false;
        }
        self.archetype = archetype;
        self.clamp_canonical_motion();
        true
    }

    #[must_use]
    pub const fn state(&self) -> &HorseState {
        &self.state
    }

    /// Installs authority-owned M4 Charge handling for subsequent fixed steps.
    pub fn set_majestic_charge_active(&mut self, active: bool) {
        self.majestic_charge_active = active;
        if active {
            self.state.terrain_speed_multiplier = 1.0;
        }
    }

    #[must_use]
    pub const fn majestic_charge_active(&self) -> bool {
        self.majestic_charge_active
    }

    pub fn set_spawn(&mut self, position: Vec3, yaw_radians: f64) {
        if position.is_finite() {
            self.spawn_position = position;
        }
        if yaw_radians.is_finite() {
            self.spawn_yaw_radians = yaw_radians;
        }
    }

    /// Feed collision-resolved engine motion into the canonical movement state.
    pub fn resolve_motion(&mut self, position: Vec3, velocity: Vec3, grounded: bool) {
        if position.is_finite() {
            self.state.position = position;
        }
        if velocity.is_finite() {
            self.state.velocity = velocity;
            let horizontal = Vec3::new(velocity.x, 0.0, velocity.z);
            self.state.forward_speed_mps = horizontal.dot(heading_forward(self.state.yaw_radians));
            self.state.lateral_speed_mps =
                horizontal.dot(heading_right(self.state.yaw_radians)).clamp(
                    -self.archetype_stats().sidestep_mps,
                    self.archetype_stats().sidestep_mps,
                );
        }
        if !self.state.grounded && grounded {
            self.state.landing_recovery_remaining = SIDESTEP_LANDING_RECOVERY_S;
        }
        self.state.grounded = grounded;
    }

    /// Reset synchronously and return the transition that must be signalled.
    pub fn reset(&mut self) -> GaitTransition {
        let old = self.state.gait;
        self.state = Self::fresh_state(self.tuning, self.spawn_position, self.spawn_yaw_radians);
        GaitTransition {
            old,
            new: Gait::Idle,
            reason: TransitionReason::Reset,
        }
    }

    /// Advance exactly one physics tick.
    pub fn step(
        &mut self,
        input: InputFrame,
        environment: Environment,
        delta: f64,
    ) -> Result<StepOutcome, StepError> {
        self.validate_delta(delta)?;
        let input = input.sanitized();

        if input.reset || self.state.position.y < self.tuning.kill_plane_y {
            let gait_transition = self.reset();
            return Ok(StepOutcome {
                velocity: self.state.velocity,
                yaw_radians: self.state.yaw_radians,
                gait_transition: Some(gait_transition),
                jumped: false,
                reset: true,
                telemetry_due: true,
            });
        }

        self.state.terrain = environment.effective_terrain();
        self.state.rough = self.state.terrain != TerrainSurface::Flat;
        self.state.slope_angle_radians = finite_or_zero(environment.slope_angle_radians)
            .abs()
            .clamp(0.0, PI / 2.0);

        let mut gait_transition = self.apply_player_gait_command(input);
        let previous_horizontal = Vec3::new(self.state.velocity.x, 0.0, self.state.velocity.z);
        let (jumped, vertical_displacement) =
            self.integrate_vertical(input, environment.grounded, delta);
        let sidestep_blocked_reason = self.sidestep_block_reason(input, environment);
        let sidestep_requested = input.steer.abs() > f64::EPSILON
            && sidestep_blocked_reason == SidestepBlockReason::None;
        self.state.sidestep_blocked_reason = sidestep_blocked_reason;
        self.integrate_steering(input.steer, environment, sidestep_requested, delta);

        let previous_speed = self.state.forward_speed_abs();
        self.integrate_horizontal(input, environment, sidestep_requested, delta);
        let current_speed = self.state.forward_speed_abs();
        self.state.acceleration_mps2 = (current_speed - previous_speed) / delta;
        self.state.position.x += (previous_horizontal.x + self.state.velocity.x) * 0.5 * delta;
        self.state.position.y += vertical_displacement;
        self.state.position.z += (previous_horizontal.z + self.state.velocity.z) * 0.5 * delta;

        if gait_transition.is_some() {
            self.state.low_speed_time = 0.0;
        } else {
            gait_transition = self
                .apply_auto_upshift(input, current_speed)
                .or_else(|| self.apply_auto_downshift(input, current_speed, delta));
        }

        self.state.telemetry_elapsed += delta;
        let periodic_telemetry =
            self.state.telemetry_elapsed + f64::EPSILON >= self.tuning.telemetry_interval;
        if periodic_telemetry {
            // Clamp an epsilon-early boundary to zero rather than `%`-retaining a value
            // infinitesimally below the interval and emitting again on the next frame.
            self.state.telemetry_elapsed =
                (self.state.telemetry_elapsed - self.tuning.telemetry_interval).max(0.0);
        }

        Ok(StepOutcome {
            velocity: self.state.velocity,
            yaw_radians: self.state.yaw_radians,
            gait_transition,
            jumped,
            reset: false,
            telemetry_due: periodic_telemetry || gait_transition.is_some(),
        })
    }

    #[must_use]
    pub fn telemetry(&self) -> Telemetry {
        let stats = self.archetype_stats();
        let speed_mps = self.state.forward_speed_abs();
        let yaw_rate = self.state.yaw_rate_radians.abs();
        let lateral_speed_mps = if speed_mps + f64::EPSILON >= SIDESTEP_FORWARD_LIMIT_MPS {
            0.0
        } else {
            self.state.lateral_speed_mps
        };
        Telemetry {
            archetype: self.archetype,
            speed_mps,
            speed_kmh: speed_mps * 3.6,
            lateral_speed_mps,
            max_vitality: stats.max_vitality,
            gait: self.state.gait,
            acceleration_mps2: self.state.acceleration_mps2,
            yaw_rate_degrees: self.state.yaw_rate_radians.to_degrees(),
            slope_angle_degrees: self.state.slope_angle_radians.to_degrees(),
            rough: self.state.rough,
            terrain: self.state.terrain,
            position: self.state.position,
            is_airborne: !self.state.grounded,
            air_time: self.state.air_time,
            speed_fraction: (speed_mps / self.target_speed(Gait::Gallop)).clamp(0.0, 1.0),
            turn_radius_m: if yaw_rate > 1.0e-5 {
                speed_mps / yaw_rate
            } else {
                0.0
            },
            sidestep_blocked_reason: self.state.sidestep_blocked_reason,
        }
    }

    fn validate_delta(&self, delta: f64) -> Result<(), StepError> {
        if !delta.is_finite() {
            Err(StepError::NonFiniteDelta)
        } else if delta <= 0.0 {
            Err(StepError::NonPositiveDelta)
        } else if delta > self.tuning.maximum_dt {
            Err(StepError::DeltaTooLarge {
                delta,
                maximum: self.tuning.maximum_dt,
            })
        } else {
            Ok(())
        }
    }

    fn target_speed(&self, gait: Gait) -> f64 {
        let active = self.archetype_stats();
        let reference = HorseArchetype::Courser.stats();
        let ratio = match gait {
            Gait::Idle => return 0.0,
            Gait::Walk => active.walk_mps / reference.walk_mps,
            Gait::Trot => active.trot_mps / reference.trot_mps,
            Gait::Gallop => active.gallop_mps / reference.gallop_mps,
        };
        self.tuning.target_speed(gait) * ratio
    }

    fn forward_acceleration(&self, gait: Gait) -> f64 {
        let active = self.archetype_stats().forward_acceleration_mps2();
        let reference = HorseArchetype::Courser.stats().forward_acceleration_mps2();
        self.tuning.acceleration(gait) * active / reference
    }

    fn reverse_speed(&self) -> f64 {
        let active = self.archetype_stats().reverse_mps;
        let reference = HorseArchetype::Courser.stats().reverse_mps;
        self.tuning.reverse_speed * active / reference
    }

    fn soft_brake_deceleration(&self) -> f64 {
        let active = self.archetype_stats().brake_deceleration_mps2;
        let reference = HorseArchetype::Courser.stats().brake_deceleration_mps2;
        self.tuning.soft_brake_deceleration * active / reference
    }

    fn hard_brake_deceleration(&self) -> f64 {
        let active = self.archetype_stats();
        let reference = HorseArchetype::Courser.stats();
        self.tuning.hard_brake_deceleration * active.brake_deceleration_mps2
            / reference.brake_deceleration_mps2
            / active.hard_brake_slide_multiplier
    }

    fn jump_velocity(&self) -> f64 {
        let active = self.archetype_stats().jump_velocity_mps();
        let reference = HorseArchetype::Courser.stats().jump_velocity_mps();
        self.tuning.jump_impulse * active / reference
    }

    fn jump_gravity(&self) -> f64 {
        let active = self.archetype_stats().jump_gravity_mps2();
        let reference = HorseArchetype::Courser.stats().jump_gravity_mps2();
        self.tuning.gravity * active / reference
    }

    fn turn_rate_degrees(&self) -> f64 {
        let active = self.archetype_stats();
        let reference = HorseArchetype::Courser.stats();
        let active_span = (active.gallop_mps - active.walk_mps).max(f64::EPSILON);
        let fraction =
            ((self.state.forward_speed_abs() - active.walk_mps) / active_span).clamp(0.0, 1.0);
        let editor_rate = self.tuning.walk_yaw_rate_degrees
            + (self.tuning.gallop_yaw_rate_degrees - self.tuning.walk_yaw_rate_degrees) * fraction;
        let reference_rate = reference.turn_walk_deg_s
            + (reference.turn_gallop_deg_s - reference.turn_walk_deg_s) * fraction;
        let active_rate =
            active.turn_walk_deg_s + (active.turn_gallop_deg_s - active.turn_walk_deg_s) * fraction;
        let rate = editor_rate * active_rate / reference_rate;
        if self.majestic_charge_active {
            rate * f64::from(CHARGE_TURN_MULTIPLIER_MILLI) / 1_000.0
        } else {
            rate
        }
    }

    fn clamp_canonical_motion(&mut self) {
        self.state.forward_speed_mps = self
            .state
            .forward_speed_mps
            .clamp(-self.reverse_speed(), self.target_speed(Gait::Gallop));
        let sidestep_cap = self.archetype_stats().sidestep_mps;
        self.state.lateral_speed_mps = self
            .state
            .lateral_speed_mps
            .clamp(-sidestep_cap, sidestep_cap);
        self.compose_canonical_horizontal();
    }

    fn compose_canonical_horizontal(&mut self) {
        let horizontal = heading_forward(self.state.yaw_radians) * self.state.forward_speed_mps
            + heading_right(self.state.yaw_radians) * self.state.lateral_speed_mps;
        self.state.velocity.x = horizontal.x;
        self.state.velocity.z = horizontal.z;
    }

    fn apply_player_gait_command(&mut self, input: InputFrame) -> Option<GaitTransition> {
        let (new, reason) = match (input.gait_up, input.gait_down) {
            (true, false) => (self.state.gait.up(), TransitionReason::PlayerUp),
            (false, true) => (self.state.gait.down(), TransitionReason::PlayerDown),
            (false, false) if self.state.gait == Gait::Idle && input.throttle > 0.0 => {
                (Gait::Walk, TransitionReason::ThrottleStart)
            }
            _ => return None,
        };
        if new == self.state.gait {
            return None;
        }
        let old = self.state.gait;
        self.state.gait = new;
        Some(GaitTransition { old, new, reason })
    }

    fn integrate_vertical(&mut self, input: InputFrame, grounded: bool, delta: f64) -> (bool, f64) {
        if !self.state.grounded && grounded {
            self.state.landing_recovery_remaining = SIDESTEP_LANDING_RECOVERY_S;
        }
        self.state.grounded = grounded;
        self.state.landing_recovery_remaining =
            (self.state.landing_recovery_remaining - delta).max(0.0);
        if grounded {
            self.state.coyote_remaining = self.tuning.coyote_time;
            self.state.air_time = 0.0;
        } else {
            self.state.coyote_remaining = (self.state.coyote_remaining - delta).max(0.0);
            self.state.air_time += delta;
        }

        if input.jump_pressed {
            self.state.jump_buffer_remaining = self.tuning.jump_buffer_time;
        }

        let can_jump = grounded || self.state.coyote_remaining > 0.0;
        if can_jump && self.state.jump_buffer_remaining > 0.0 {
            let initial_velocity = self.jump_velocity();
            let gravity = self.jump_gravity();
            self.state.velocity.y =
                (initial_velocity - gravity * delta).max(-self.tuning.terminal_fall_speed);
            self.state.grounded = false;
            self.state.coyote_remaining = 0.0;
            self.state.jump_buffer_remaining = 0.0;
            self.state.landing_recovery_remaining = 0.0;
            let displacement = initial_velocity * delta - 0.5 * gravity * delta * delta;
            return (true, displacement);
        }

        self.state.jump_buffer_remaining = (self.state.jump_buffer_remaining - delta).max(0.0);
        if grounded {
            if self.state.velocity.y < 0.0 {
                self.state.velocity.y = 0.0;
            }
            (false, 0.0)
        } else {
            let initial_velocity = self.state.velocity.y;
            self.state.velocity.y = (initial_velocity - self.jump_gravity() * delta)
                .max(-self.tuning.terminal_fall_speed);
            (
                false,
                (initial_velocity + self.state.velocity.y) * 0.5 * delta,
            )
        }
    }

    fn sidestep_block_reason(
        &self,
        input: InputFrame,
        environment: Environment,
    ) -> SidestepBlockReason {
        if input.steer.abs() <= f64::EPSILON {
            return SidestepBlockReason::None;
        }
        if input.throttle > f64::EPSILON {
            return SidestepBlockReason::ForwardInput;
        }
        if self.state.gait != Gait::Idle
            || self.state.forward_speed_abs() + f64::EPSILON >= SIDESTEP_FORWARD_LIMIT_MPS
        {
            return SidestepBlockReason::ForwardMotion;
        }
        if !self.state.grounded {
            return SidestepBlockReason::Airborne;
        }
        if self.state.landing_recovery_remaining > f64::EPSILON {
            return SidestepBlockReason::LandingRecovery;
        }
        if environment.headshot_staggered || (environment.staggered && !self.majestic_charge_active)
        {
            return SidestepBlockReason::Stagger;
        }
        if environment.power_turning || input.hard_brake {
            return SidestepBlockReason::PowerTurn;
        }
        if environment.steep_slope
            || self.state.slope_angle_radians.to_degrees()
                > SIDESTEP_BLOCK_SLOPE_DEGREES + f64::EPSILON
        {
            return SidestepBlockReason::OffCamber;
        }
        SidestepBlockReason::None
    }

    fn integrate_steering(
        &mut self,
        raw_steer: f64,
        environment: Environment,
        sidestep_requested: bool,
        delta: f64,
    ) {
        self.state.steering_axis = move_towards(
            self.state.steering_axis,
            raw_steer,
            self.tuning.steering_response * delta,
        );

        // Sidestep replaces the old free in-place spin. Suppress residual yaw while the
        // canonical lateral component ramps in or settles before forward acceleration.
        if sidestep_requested || self.state.lateral_speed_mps.abs() > SIDESTEP_EPSILON_MPS {
            self.state.yaw_rate_radians = 0.0;
            return;
        }

        let control = if self.state.grounded {
            1.0
        } else {
            self.tuning.air_control
        };
        let target_yaw_rate = if self.state.gait == Gait::Idle {
            if self.state.sidestep_blocked_reason == SidestepBlockReason::OffCamber {
                // Refuse a lateral step off-camber but permit a bounded rein pivot.
                -self.state.steering_axis * 40.0_f64.to_radians()
            } else {
                0.0
            }
        } else {
            // Godot's positive Y rotation turns local -Z toward -X (left). Input steering is
            // right-positive, so right input must produce a negative Godot yaw rate.
            -self.state.steering_axis * self.turn_rate_degrees().to_radians() * control
        };
        let previous_yaw_rate = self.state.yaw_rate_radians;
        let blend = 1.0 - (-self.tuning.steering_response * delta).exp();
        self.state.yaw_rate_radians += (target_yaw_rate - self.state.yaw_rate_radians) * blend;
        let average_yaw_rate = (previous_yaw_rate + self.state.yaw_rate_radians) * 0.5;
        self.state.yaw_radians = wrap_angle(self.state.yaw_radians + average_yaw_rate * delta);

        if environment.steep_slope {
            self.state.yaw_rate_radians = self
                .state
                .yaw_rate_radians
                .clamp(-40.0_f64.to_radians(), 40.0_f64.to_radians());
        }
    }

    fn integrate_horizontal(
        &mut self,
        input: InputFrame,
        environment: Environment,
        sidestep_requested: bool,
        delta: f64,
    ) {
        if environment.steep_slope {
            self.integrate_steep_slide(environment.downhill_direction, delta);
            return;
        }

        let (terrain_speed, terrain_acceleration) = self.terrain_multipliers(environment, delta);
        let gait_cap = if self.majestic_charge_active {
            self.archetype_stats().sprint_mps
        } else {
            self.target_speed(self.state.gait) * terrain_speed
        };
        let mut longitudinal = self.state.forward_speed_mps;
        let mut lateral = self.state.lateral_speed_mps;
        let settling_lateral = !sidestep_requested && lateral.abs() > SIDESTEP_EPSILON_MPS;

        let (target_longitudinal, rate) = if input.hard_brake
            && self.majestic_charge_active
            && input.steer.abs() > f64::EPSILON
        {
            self.state.reverse_hold_time = 0.0;
            (longitudinal, 0.0)
        } else if input.hard_brake {
            self.state.reverse_hold_time = 0.0;
            (0.0, self.hard_brake_deceleration())
        } else if sidestep_requested || (input.throttle > 0.0 && settling_lateral) {
            // Lateral input wins over reverse, and W waits for the 0.15 s settle rather
            // than creating an exploitable diagonal launch.
            self.state.reverse_hold_time = 0.0;
            (0.0, self.soft_brake_deceleration())
        } else if input.brake > 0.0 {
            if longitudinal > 0.05 {
                self.state.reverse_hold_time = 0.0;
                (0.0, self.soft_brake_deceleration())
            } else if longitudinal < -0.05 {
                (
                    -self.reverse_speed() * input.brake,
                    self.forward_acceleration(Gait::Walk),
                )
            } else {
                self.state.reverse_hold_time += delta;
                if self.state.reverse_hold_time >= 0.35 {
                    (
                        -self.reverse_speed() * input.brake,
                        self.forward_acceleration(Gait::Walk),
                    )
                } else {
                    (0.0, self.soft_brake_deceleration())
                }
            }
        } else if input.throttle > 0.0 {
            (
                gait_cap * input.throttle,
                self.forward_acceleration(self.state.gait)
                    * terrain_acceleration
                    * if self.majestic_charge_active {
                        f64::from(CHARGE_ACCEL_MULTIPLIER_MILLI) / 1_000.0
                    } else {
                        1.0
                    },
            )
        } else {
            (0.0, self.tuning.coast_deceleration)
        };
        if input.brake <= 0.0 || sidestep_requested {
            self.state.reverse_hold_time = 0.0;
        }
        longitudinal = move_towards(longitudinal, target_longitudinal, rate * delta);

        let sidestep_cap = self.archetype_stats().sidestep_mps;
        if sidestep_requested {
            let target_lateral = input.steer * sidestep_cap;
            let ramp_rate = sidestep_cap / self.archetype_stats().sidestep_ramp_s;
            lateral = move_towards(lateral, target_lateral, ramp_rate * delta);
        } else {
            lateral = move_towards(lateral, 0.0, sidestep_cap / SIDESTEP_RAMP_OUT_S * delta);
        }
        lateral = lateral.clamp(-sidestep_cap, sidestep_cap);

        if self.state.gait == Gait::Idle
            && input.throttle == 0.0
            && input.brake == 0.0
            && longitudinal.abs() <= self.tuning.idle_speed_floor
        {
            longitudinal = 0.0;
        }

        self.state.forward_speed_mps = longitudinal;
        self.state.lateral_speed_mps = lateral;
        self.compose_canonical_horizontal();
    }

    fn integrate_steep_slide(&mut self, downhill_direction: Vec3, delta: f64) {
        let downhill = downhill_direction.normalized_horizontal();
        let target = downhill * self.tuning.steep_slide_speed;
        let horizontal = Vec3::new(self.state.velocity.x, 0.0, self.state.velocity.z);
        let horizontal = move_vector_towards(horizontal, target, self.tuning.gravity * delta);
        self.state.velocity.x = horizontal.x;
        self.state.velocity.z = horizontal.z;
        self.state.velocity.y = self.state.velocity.y.min(0.0);
        self.state.forward_speed_mps = horizontal.dot(heading_forward(self.state.yaw_radians));
        self.state.lateral_speed_mps = 0.0;
    }

    fn design_terrain_factor(&self, terrain: TerrainSurface) -> f64 {
        let stats = self.archetype_stats();
        match terrain {
            TerrainSurface::Flat => 1.0,
            TerrainSurface::Scrub => stats.terrain_scrub,
            TerrainSurface::Mud => stats.terrain_mud,
            TerrainSurface::Riverbed => stats.terrain_riverbed,
        }
    }

    fn terrain_multipliers(&mut self, environment: Environment, delta: f64) -> (f64, f64) {
        if self.majestic_charge_active {
            self.state.terrain_speed_multiplier = 1.0;
            return (1.0, 1.0);
        }
        let terrain = environment.effective_terrain();
        let global_speed_scale =
            self.tuning.rough_speed_multiplier / HorseArchetype::Courser.stats().terrain_scrub;
        let global_acceleration_scale = self.tuning.rough_acceleration_multiplier
            / HorseArchetype::Courser.stats().terrain_scrub;
        let design_factor = self.design_terrain_factor(terrain);
        let target_speed = if terrain == TerrainSurface::Flat {
            1.0
        } else {
            (design_factor * global_speed_scale).clamp(0.05, 1.0)
        };
        if target_speed < self.state.terrain_speed_multiplier {
            self.state.terrain_speed_multiplier = target_speed;
        } else {
            // Three time constants leave five percent of the original penalty at the
            // design recovery time, independent of fixed-step rate.
            let recovery = self.archetype_stats().terrain_recovery_s;
            let blend = 1.0 - (-3.0 * delta / recovery).exp();
            self.state.terrain_speed_multiplier +=
                (target_speed - self.state.terrain_speed_multiplier) * blend;
        }

        let mut speed = self.state.terrain_speed_multiplier;
        let mut acceleration = if terrain == TerrainSurface::Flat {
            1.0
        } else {
            (design_factor * global_acceleration_scale).clamp(0.05, 1.0)
        };
        let signed_slope = finite_or_zero(environment.signed_slope_radians);
        let reference = self.tuning.uphill_reference_degrees.to_radians();
        if signed_slope > 0.0 {
            let fraction = (signed_slope / reference).clamp(0.0, 1.0);
            speed *= 1.0 - (1.0 - self.tuning.uphill_speed_multiplier) * fraction;
        } else if signed_slope < 0.0 {
            let fraction = (-signed_slope / reference).clamp(0.0, 1.0);
            let bonus = self.tuning.downhill_speed_bonus * fraction;
            speed *= 1.0 + bonus;
            acceleration *= 1.0 + bonus;
        }
        (speed, acceleration)
    }

    fn apply_auto_upshift(
        &mut self,
        input: InputFrame,
        horizontal_speed: f64,
    ) -> Option<GaitTransition> {
        if input.throttle <= 0.0 || input.brake > 0.0 || input.hard_brake {
            return None;
        }
        let threshold = match self.state.gait {
            Gait::Walk => self.target_speed(Gait::Walk) * 0.85,
            Gait::Trot => self.target_speed(Gait::Trot) * 0.85,
            Gait::Idle | Gait::Gallop => return None,
        };
        if horizontal_speed + f64::EPSILON < threshold {
            return None;
        }
        let old = self.state.gait;
        let new = old.up();
        self.state.gait = new;
        Some(GaitTransition {
            old,
            new,
            reason: TransitionReason::AutoUpshift,
        })
    }

    fn apply_auto_downshift(
        &mut self,
        input: InputFrame,
        horizontal_speed: f64,
        delta: f64,
    ) -> Option<GaitTransition> {
        // A held forward command preserves the selected gait on rough ground and
        // slopes; coasting or braking still downshifts as momentum decays.
        let decaying = input.throttle <= 0.0 || input.brake > 0.0 || input.hard_brake;
        if self.state.gait != Gait::Idle
            && decaying
            && horizontal_speed < self.tuning.speed_floor(self.state.gait)
        {
            self.state.low_speed_time += delta;
            if self.state.low_speed_time + f64::EPSILON >= self.tuning.auto_downshift_delay {
                let old = self.state.gait;
                let new = old.down();
                self.state.gait = new;
                self.state.low_speed_time = 0.0;
                return Some(GaitTransition {
                    old,
                    new,
                    reason: TransitionReason::AutoDownshift,
                });
            }
        } else {
            self.state.low_speed_time = 0.0;
        }
        None
    }
}

fn heading_forward(yaw_radians: f64) -> Vec3 {
    // Godot convention: local forward is -Z and positive Y rotation turns it toward -X.
    Vec3::new(-yaw_radians.sin(), 0.0, -yaw_radians.cos())
}

fn heading_right(yaw_radians: f64) -> Vec3 {
    Vec3::new(yaw_radians.cos(), 0.0, -yaw_radians.sin())
}

fn move_towards(current: f64, target: f64, maximum_delta: f64) -> f64 {
    let difference = target - current;
    if difference.abs() <= maximum_delta {
        target
    } else {
        current + difference.signum() * maximum_delta
    }
}

fn move_vector_towards(current: Vec3, target: Vec3, maximum_delta: f64) -> Vec3 {
    let difference = target - current;
    let distance = difference.length();
    if distance <= maximum_delta || distance <= f64::EPSILON {
        target
    } else {
        current + difference * (maximum_delta / distance)
    }
}

fn wrap_angle(angle: f64) -> f64 {
    (angle + PI).rem_euclid(2.0 * PI) - PI
}

fn finite_or_zero(value: f64) -> f64 {
    finite_or(value, 0.0)
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn positive_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn nonnegative_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ_VALUES: [u32; 3] = [30, 60, 120];

    fn kernel() -> HorseKernel {
        kernel_for(HorseArchetype::Mustang)
    }

    fn kernel_for(archetype: HorseArchetype) -> HorseKernel {
        let mut kernel = HorseKernel::new(Tuning::default(), Vec3::ZERO, 0.0);
        kernel.set_archetype(archetype);
        kernel
    }

    fn step(
        kernel: &mut HorseKernel,
        input: InputFrame,
        environment: Environment,
        hz: u32,
    ) -> StepOutcome {
        kernel
            .step(input, environment, 1.0 / f64::from(hz))
            .expect("valid fixed time step")
    }

    fn select_gallop(kernel: &mut HorseKernel, hz: u32) {
        for _ in 0..3 {
            let outcome = step(
                kernel,
                InputFrame {
                    gait_up: true,
                    throttle: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            assert!(outcome.gait_transition.is_some());
        }
        assert_eq!(kernel.state().gait, Gait::Gallop);
    }

    fn reach_gallop(kernel: &mut HorseKernel, hz: u32) {
        select_gallop(kernel, hz);
        let frames =
            ((kernel.archetype_stats().accel_0_to_gallop_s + 1.0) * f64::from(hz)).ceil() as u32;
        for _ in 0..frames {
            step(
                kernel,
                InputFrame {
                    throttle: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
        }
        let expected = kernel.archetype_stats().gallop_mps;
        assert!((kernel.state().forward_speed_abs() - expected).abs() < 1.0e-9);
    }

    #[test]
    fn frame_rates_are_bounded_and_acceleration_meets_course_target() {
        let mut final_states = Vec::new();
        for hz in HZ_VALUES {
            // Courser is the explicit M0 reference archetype; Mustang remains the runtime default.
            let mut horse = kernel_for(HorseArchetype::Courser);
            let mut reached = None;
            let mut elapsed = 0.0;
            for frame in 0..(5 * hz) {
                let outcome = step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        gait_up: frame < 3,
                        steer: if frame >= hz { 0.35 } else { 0.0 },
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                assert!(outcome.velocity.is_finite());
                elapsed += 1.0 / f64::from(hz);
                if reached.is_none() && horse.state().horizontal_speed() >= 10.5 {
                    reached = Some((elapsed, horse.state().position.horizontal_length()));
                }
            }
            let (time, distance) = reached.expect("horse reaches acceptance speed");
            assert!(time <= 3.5, "{hz} Hz took {time:.3} s");
            assert!(distance <= 30.0, "{hz} Hz used {distance:.3} m");
            assert!(horse.state().horizontal_speed() <= 14.5 + 1.0e-9);
            final_states.push(horse.state().clone());
        }

        for pair in final_states.windows(2) {
            assert!((pair[0].horizontal_speed() - pair[1].horizontal_speed()).abs() < 0.03);
            assert!((pair[0].yaw_radians - pair[1].yaw_radians).abs() < 0.04);
            assert!((pair[0].position - pair[1].position).length() < 0.5);
        }
    }

    #[test]
    fn forward_from_idle_enters_walk_and_moves_along_godot_forward() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            let first = step(
                &mut horse,
                InputFrame {
                    throttle: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            assert_eq!(
                first.gait_transition,
                Some(GaitTransition {
                    old: Gait::Idle,
                    new: Gait::Walk,
                    reason: TransitionReason::ThrottleStart,
                })
            );
            assert!(first.velocity.z < 0.0, "{hz} Hz forward must be local -Z");
            assert!(first.velocity.x.abs() < 1.0e-9);
        }
    }

    #[test]
    fn right_input_turns_visual_and_velocity_toward_positive_x() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            for _ in 0..hz {
                step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert!(horse.state().yaw_radians < 0.0, "{hz} Hz Godot right yaw");
            assert!(horse.state().position.x > 0.0, "{hz} Hz rightward movement");
        }
    }

    #[test]
    fn held_forward_naturally_reaches_gallop() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            let mut transitions = Vec::new();
            for _ in 0..(4 * hz) {
                if let Some(transition) = step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                )
                .gait_transition
                {
                    transitions.push(transition);
                }
            }
            assert_eq!(horse.state().gait, Gait::Gallop, "{hz} Hz gait");
            assert!(horse.state().horizontal_speed() >= 12.5, "{hz} Hz speed");
            assert!(transitions.iter().any(|transition| {
                transition.old == Gait::Walk
                    && transition.new == Gait::Trot
                    && transition.reason == TransitionReason::AutoUpshift
            }));
            assert!(transitions.iter().any(|transition| {
                transition.old == Gait::Trot
                    && transition.new == Gait::Gallop
                    && transition.reason == TransitionReason::AutoUpshift
            }));
        }
    }

    #[test]
    fn brake_stops_before_reverse_delay_engages() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            for _ in 0..hz {
                step(
                    &mut horse,
                    InputFrame {
                        brake: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                if horse.state().position.z > 0.0 {
                    break;
                }
            }
            let delay_frames = (0.3 * f64::from(hz)) as usize;
            let mut stopped = kernel();
            for _ in 0..delay_frames {
                step(
                    &mut stopped,
                    InputFrame {
                        brake: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert_eq!(stopped.state().horizontal_speed(), 0.0, "{hz} Hz delay");
            for _ in 0..((0.2 * f64::from(hz)) as usize + 1) {
                step(
                    &mut stopped,
                    InputFrame {
                        brake: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert!(stopped.state().velocity.z > 0.0, "{hz} Hz reverse");
        }
    }

    #[test]
    fn gait_commands_and_auto_downshift_transition_once() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            for (old, new) in [
                (Gait::Idle, Gait::Walk),
                (Gait::Walk, Gait::Trot),
                (Gait::Trot, Gait::Gallop),
            ] {
                let outcome = step(
                    &mut horse,
                    InputFrame {
                        gait_up: true,
                        throttle: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                assert_eq!(
                    outcome.gait_transition,
                    Some(GaitTransition {
                        old,
                        new,
                        reason: TransitionReason::PlayerUp,
                    })
                );
                assert!(outcome.telemetry_due, "{hz} Hz command telemetry");
            }

            horse.resolve_motion(Vec3::ZERO, Vec3::new(0.0, 0.0, -6.5), true);
            let mut auto = Vec::new();
            for _ in 0..(hz / 2) {
                if let Some(transition) = step(
                    &mut horse,
                    InputFrame::default(),
                    Environment::default(),
                    hz,
                )
                .gait_transition
                {
                    auto.push(transition);
                }
            }
            assert_eq!(auto.len(), 1, "{hz} Hz auto transition count");
            assert_eq!(auto[0].old, Gait::Gallop);
            assert_eq!(auto[0].new, Gait::Trot);
            assert_eq!(auto[0].reason, TransitionReason::AutoDownshift);

            let down = step(
                &mut horse,
                InputFrame {
                    gait_down: true,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            assert_eq!(
                down.gait_transition.expect("player downshift").new,
                Gait::Walk
            );
        }
    }

    #[test]
    fn speed_caps_and_terrain_modifiers_are_stable() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            reach_gallop(&mut horse, hz);
            let flat_speed = horse.state().horizontal_speed();

            for _ in 0..(2 * hz) {
                step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        ..InputFrame::default()
                    },
                    Environment {
                        rough: true,
                        ..Environment::default()
                    },
                    hz,
                );
            }
            let rough_speed = horse.state().horizontal_speed();
            let drop = 1.0 - rough_speed / flat_speed;
            assert!((0.045..=0.055).contains(&drop), "{hz} Hz scrub drop {drop}");

            for _ in 0..(2 * hz) {
                step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert!(horse.state().horizontal_speed() >= flat_speed * 0.95);

            for _ in 0..(2 * hz) {
                step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        ..InputFrame::default()
                    },
                    Environment {
                        slope_angle_radians: 25.0_f64.to_radians(),
                        signed_slope_radians: 25.0_f64.to_radians(),
                        ..Environment::default()
                    },
                    hz,
                );
            }
            let uphill_fraction = horse.state().horizontal_speed() / flat_speed;
            assert!(
                (0.55..=0.75).contains(&uphill_fraction),
                "{hz} Hz uphill fraction {uphill_fraction}"
            );
        }
    }

    #[test]
    fn majestic_charge_uses_locked_speed_accel_turn_terrain_and_drift_rows() {
        for hz in HZ_VALUES {
            let mut charged = kernel();
            charged.set_majestic_charge_active(true);
            select_gallop(&mut charged, hz);
            for _ in 0..(3 * hz) {
                step(
                    &mut charged,
                    InputFrame {
                        throttle: 1.0,
                        ..InputFrame::default()
                    },
                    Environment {
                        terrain: TerrainSurface::Mud,
                        rough: true,
                        ..Environment::default()
                    },
                    hz,
                );
            }
            let sprint = charged.archetype_stats().sprint_mps;
            assert!((charged.state().forward_speed_abs() - sprint).abs() < 1.0e-9);
            assert_eq!(charged.state().terrain_speed_multiplier, 1.0);

            let before_drift = charged.state().forward_speed_abs();
            step(
                &mut charged,
                InputFrame {
                    steer: 1.0,
                    hard_brake: true,
                    ..InputFrame::default()
                },
                Environment {
                    power_turning: true,
                    ..Environment::default()
                },
                hz,
            );
            assert!((charged.state().forward_speed_abs() - before_drift).abs() < 1.0e-9);

            let mut normal_turn = kernel();
            let mut charge_turn = kernel();
            select_gallop(&mut normal_turn, hz);
            select_gallop(&mut charge_turn, hz);
            normal_turn.resolve_motion(Vec3::ZERO, Vec3::new(0.0, 0.0, -13.0), true);
            charge_turn.resolve_motion(Vec3::ZERO, Vec3::new(0.0, 0.0, -13.0), true);
            charge_turn.set_majestic_charge_active(true);
            step(
                &mut normal_turn,
                InputFrame {
                    throttle: 1.0,
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            step(
                &mut charge_turn,
                InputFrame {
                    throttle: 1.0,
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            let ratio = charge_turn.state().yaw_rate_radians / normal_turn.state().yaw_rate_radians;
            assert!((ratio - 1.3).abs() < 1.0e-9, "{hz} Hz ratio {ratio}");
        }
    }

    #[test]
    fn steering_has_gait_dependent_turn_radius_and_air_control() {
        for hz in HZ_VALUES {
            let mut gallop = kernel();
            reach_gallop(&mut gallop, hz);
            for _ in 0..(3 * hz) {
                step(
                    &mut gallop,
                    InputFrame {
                        throttle: 1.0,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            let telemetry = gallop.telemetry();
            let stats = HorseArchetype::Mustang.stats();
            let expected_radius = stats.gallop_mps / stats.turn_gallop_deg_s.to_radians();
            assert!(
                (telemetry.turn_radius_m - expected_radius).abs() < 0.02,
                "{hz} Hz radius {}",
                telemetry.turn_radius_m
            );
            assert!((telemetry.yaw_rate_degrees + stats.turn_gallop_deg_s).abs() < 0.1);

            let mut walk = kernel();
            step(
                &mut walk,
                InputFrame {
                    gait_up: true,
                    throttle: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            for _ in 0..(2 * hz) {
                step(
                    &mut walk,
                    InputFrame {
                        // Analog partial throttle deliberately stays in Walk while full W input
                        // naturally progresses through the faster gaits.
                        throttle: 0.7,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert_eq!(walk.state().gait, Gait::Walk);
            assert!(walk.telemetry().turn_radius_m < 3.0);

            let ground_rate = walk.state().yaw_rate_radians.abs();
            for _ in 0..hz {
                step(
                    &mut walk,
                    InputFrame {
                        throttle: 0.7,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment {
                        grounded: false,
                        ..Environment::default()
                    },
                    hz,
                );
            }
            assert!(
                (walk.state().yaw_rate_radians.abs() / ground_rate - 0.2).abs() < 0.02,
                "{hz} Hz air-control ratio"
            );
        }
    }

    #[test]
    fn jump_buffer_coyote_and_gravity_are_deterministic() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            step(
                &mut horse,
                InputFrame::default(),
                Environment::default(),
                hz,
            );
            let airborne = Environment {
                grounded: false,
                ..Environment::default()
            };
            let delay_frames = (0.12 * f64::from(hz)).floor() as u32;
            for _ in 0..delay_frames {
                step(&mut horse, InputFrame::default(), airborne, hz);
            }
            let takeoff_y = horse.state().position.y;
            let jump = step(
                &mut horse,
                InputFrame {
                    jump_pressed: true,
                    ..InputFrame::default()
                },
                airborne,
                hz,
            );
            assert!(jump.jumped, "{hz} Hz coyote jump was rejected");
            let stats = HorseArchetype::Mustang.stats();
            let expected_velocity =
                stats.jump_velocity_mps() - stats.jump_gravity_mps2() / f64::from(hz);
            assert!((horse.state().velocity.y - expected_velocity).abs() < 1.0e-9);

            let mut apex = horse.state().position.y;
            for _ in 0..hz {
                step(&mut horse, InputFrame::default(), airborne, hz);
                apex = apex.max(horse.state().position.y);
            }
            let height = apex - takeoff_y;
            assert!(
                (height - stats.jump_apex_m).abs() < 0.08,
                "{hz} Hz apex was {height}"
            );
        }

        let mut late = kernel();
        step(&mut late, InputFrame::default(), Environment::default(), 60);
        for _ in 0..10 {
            step(
                &mut late,
                InputFrame::default(),
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                60,
            );
        }
        assert!(
            !step(
                &mut late,
                InputFrame {
                    jump_pressed: true,
                    ..InputFrame::default()
                },
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                60,
            )
            .jumped
        );

        let mut buffered = kernel();
        buffered.resolve_motion(Vec3::ZERO, Vec3::new(0.0, -1.0, 0.0), false);
        for _ in 0..10 {
            step(
                &mut buffered,
                InputFrame::default(),
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                60,
            );
        }
        assert!(
            !step(
                &mut buffered,
                InputFrame {
                    jump_pressed: true,
                    ..InputFrame::default()
                },
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                60,
            )
            .jumped
        );
        assert!(
            step(
                &mut buffered,
                InputFrame::default(),
                Environment::default(),
                60,
            )
            .jumped
        );
    }

    #[test]
    fn stationary_sidestep_has_bounded_signed_rate_and_no_yaw() {
        for hz in HZ_VALUES {
            for archetype in HorseArchetype::ALL {
                let stats = archetype.stats();
                for direction in [-1.0, 1.0] {
                    let mut horse = kernel_for(archetype);
                    for _ in 0..hz {
                        step(
                            &mut horse,
                            InputFrame {
                                steer: direction,
                                ..InputFrame::default()
                            },
                            Environment::default(),
                            hz,
                        );
                    }

                    let telemetry = horse.telemetry();
                    assert_eq!(horse.state().gait, Gait::Idle);
                    assert!(horse.state().forward_speed_abs() < 1.0e-12);
                    assert!(
                        (telemetry.lateral_speed_mps - direction * stats.sidestep_mps).abs()
                            < 1.0e-9,
                        "{archetype:?} at {hz} Hz lateral {}",
                        telemetry.lateral_speed_mps
                    );
                    assert_eq!(horse.state().yaw_radians, 0.0);
                    assert_eq!(horse.state().yaw_rate_radians, 0.0);
                    assert_eq!(telemetry.speed_mps, 0.0);
                    assert_eq!(telemetry.sidestep_blocked_reason, SidestepBlockReason::None);

                    let expected_distance =
                        stats.sidestep_mps * (1.0 - 0.5 * stats.sidestep_ramp_s);
                    assert!(
                        (horse.state().position.x - direction * expected_distance).abs() < 0.02,
                        "{archetype:?} at {hz} Hz position {}",
                        horse.state().position.x
                    );
                }
            }
        }
    }

    #[test]
    fn sidestep_settles_before_forward_or_reverse_motion() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            for _ in 0..hz {
                step(
                    &mut horse,
                    InputFrame {
                        steer: 1.0,
                        brake: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert_eq!(horse.state().forward_speed_mps, 0.0, "{hz} Hz reverse leak");
            assert!(horse.state().lateral_speed_mps > 1.1);

            let settle_frames = (SIDESTEP_RAMP_OUT_S * f64::from(hz)).ceil() as u32;
            for frame in 0..settle_frames {
                let outcome = step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                if frame == 0 {
                    assert_eq!(
                        outcome.gait_transition.expect("walk transition").new,
                        Gait::Walk
                    );
                }
                assert_eq!(
                    horse.state().forward_speed_mps,
                    0.0,
                    "{hz} Hz diagonal launch"
                );
                assert_eq!(horse.state().yaw_rate_radians, 0.0, "{hz} Hz settle yaw");
            }
            assert!(horse.state().lateral_speed_mps.abs() < 1.0e-12);
            step(
                &mut horse,
                InputFrame {
                    throttle: 1.0,
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            assert!(
                horse.state().forward_speed_mps > 0.0,
                "{hz} Hz forward resume"
            );
            assert!(
                horse.state().yaw_radians < 0.0,
                "{hz} Hz rein steering resume"
            );

            let mut reverse = kernel();
            for _ in 0..hz {
                step(
                    &mut reverse,
                    InputFrame {
                        brake: 1.0,
                        steer: -1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert_eq!(reverse.state().forward_speed_mps, 0.0);
            for _ in 0..settle_frames {
                step(
                    &mut reverse,
                    InputFrame {
                        brake: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                assert_eq!(reverse.state().forward_speed_mps, 0.0);
            }
            for _ in 0..((0.35 * f64::from(hz)).ceil() as u32 + 1) {
                step(
                    &mut reverse,
                    InputFrame {
                        brake: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert!(
                reverse.state().forward_speed_mps < 0.0,
                "{hz} Hz reverse resume"
            );
        }
    }

    #[test]
    fn gallop_steering_never_creates_shooter_strafe() {
        for hz in HZ_VALUES {
            let mut horse = kernel();
            reach_gallop(&mut horse, hz);
            for _ in 0..(2 * hz) {
                step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                assert_eq!(horse.state().lateral_speed_mps, 0.0);
                assert_eq!(horse.telemetry().lateral_speed_mps, 0.0);
            }
            assert!(
                horse.state().yaw_radians.abs() > 0.1,
                "{hz} Hz did not steer"
            );

            // Collision-resolved lateral motion feeds back into canonical state, then is
            // removed within the anti-strafe window and hidden from high-speed telemetry.
            let position = horse.state().position;
            let yaw = horse.state().yaw_radians;
            let injected = heading_forward(yaw) * 13.0 + heading_right(yaw) * 0.6;
            horse.resolve_motion(position, injected, true);
            assert!((horse.state().lateral_speed_mps - 0.6).abs() < 1.0e-9);
            assert_eq!(horse.telemetry().lateral_speed_mps, 0.0);
            for _ in 0..((0.2 * f64::from(hz)).ceil() as u32) {
                step(
                    &mut horse,
                    InputFrame {
                        throttle: 1.0,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert_eq!(horse.state().lateral_speed_mps, 0.0);

            horse.resolve_motion(horse.state().position, Vec3::ZERO, true);
            assert_eq!(horse.state().forward_speed_mps, 0.0);
            step(
                &mut horse,
                InputFrame {
                    throttle: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            assert!(
                horse.state().forward_speed_mps < 0.2,
                "{hz} Hz stale collision speed"
            );
        }
    }

    #[test]
    fn archetype_handling_orders_and_jump_rows_apply_at_all_rates() {
        for hz in HZ_VALUES {
            let mut times = Vec::new();
            for archetype in HorseArchetype::ALL {
                let stats = archetype.stats();
                let mut horse = kernel_for(archetype);
                let mut reached_at = None;
                for frame in 0..(7 * hz) {
                    step(
                        &mut horse,
                        InputFrame {
                            throttle: 1.0,
                            ..InputFrame::default()
                        },
                        Environment::default(),
                        hz,
                    );
                    if reached_at.is_none()
                        && horse.state().forward_speed_abs() >= stats.gallop_mps * 0.99
                    {
                        reached_at = Some(f64::from(frame + 1) / f64::from(hz));
                    }
                }
                assert_eq!(horse.state().gait, Gait::Gallop);
                assert!((horse.state().forward_speed_abs() - stats.gallop_mps).abs() < 1.0e-9);
                times.push((archetype, reached_at.expect("archetype reaches top speed")));

                for _ in 0..(2 * hz) {
                    step(
                        &mut horse,
                        InputFrame {
                            throttle: 1.0,
                            steer: 1.0,
                            ..InputFrame::default()
                        },
                        Environment::default(),
                        hz,
                    );
                }
                assert!(
                    (horse.telemetry().yaw_rate_degrees + stats.turn_gallop_deg_s).abs() < 0.1,
                    "{archetype:?} at {hz} Hz turn rate"
                );

                let mut jumper = kernel_for(archetype);
                let jump = step(
                    &mut jumper,
                    InputFrame {
                        jump_pressed: true,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                assert!(jump.jumped);
                let mut apex = jumper.state().position.y;
                let mut landed_at = None;
                for frame in 1..=(2 * hz) {
                    step(
                        &mut jumper,
                        InputFrame::default(),
                        Environment {
                            grounded: false,
                            ..Environment::default()
                        },
                        hz,
                    );
                    apex = apex.max(jumper.state().position.y);
                    if jumper.state().position.y <= 0.0 {
                        landed_at = Some(f64::from(frame + 1) / f64::from(hz));
                        break;
                    }
                }
                assert!(
                    (apex - stats.jump_apex_m).abs() < 0.02,
                    "{archetype:?} at {hz} Hz apex {apex}"
                );
                assert!(
                    (landed_at.expect("ballistic return") - stats.jump_airtime_s).abs()
                        <= 1.0 / f64::from(hz) + 1.0e-9,
                    "{archetype:?} at {hz} Hz airtime"
                );
            }
            assert!(times[0].1 < times[2].1, "{hz} Hz Courser/Mustang accel");
            assert!(times[2].1 < times[1].1, "{hz} Hz Mustang/Warhorse accel");
        }
    }

    #[test]
    fn archetype_switch_clamps_speed_lateral_state_and_telemetry() {
        for hz in HZ_VALUES {
            let mut horse = kernel_for(HorseArchetype::Courser);
            reach_gallop(&mut horse, hz);
            assert_eq!(horse.state().forward_speed_mps, 14.5);
            assert!(horse.set_archetype(HorseArchetype::Warhorse));
            assert_eq!(horse.state().forward_speed_mps, 12.0);
            assert!((horse.state().velocity.horizontal_length() - 12.0).abs() < 1.0e-9);
            assert!(!horse.set_archetype(HorseArchetype::Warhorse));
            let telemetry = horse.telemetry();
            assert_eq!(telemetry.archetype, HorseArchetype::Warhorse);
            assert_eq!(telemetry.max_vitality, 320.0);

            let mut lateral = kernel_for(HorseArchetype::Mustang);
            for _ in 0..hz {
                step(
                    &mut lateral,
                    InputFrame {
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert_eq!(lateral.state().lateral_speed_mps, 1.2);
            assert!(lateral.set_archetype(HorseArchetype::Warhorse));
            assert_eq!(lateral.state().lateral_speed_mps, 0.8);
            assert_eq!(lateral.telemetry().lateral_speed_mps, 0.8);
        }
    }

    #[test]
    fn terrain_sidegrades_and_recovery_apply_from_immutable_rows() {
        for hz in HZ_VALUES {
            for archetype in HorseArchetype::ALL {
                let stats = archetype.stats();
                for (terrain, expected_factor) in [
                    (TerrainSurface::Scrub, stats.terrain_scrub),
                    (TerrainSurface::Mud, stats.terrain_mud),
                    (TerrainSurface::Riverbed, stats.terrain_riverbed),
                ] {
                    let mut horse = kernel_for(archetype);
                    reach_gallop(&mut horse, hz);
                    for _ in 0..(3 * hz) {
                        step(
                            &mut horse,
                            InputFrame {
                                throttle: 1.0,
                                ..InputFrame::default()
                            },
                            Environment {
                                terrain,
                                ..Environment::default()
                            },
                            hz,
                        );
                    }
                    let fraction = horse.state().forward_speed_abs() / stats.gallop_mps;
                    assert!(
                        (fraction - expected_factor).abs() < 0.01,
                        "{archetype:?} {terrain:?} at {hz} Hz fraction {fraction}"
                    );
                }

                let mut recovering = kernel_for(archetype);
                reach_gallop(&mut recovering, hz);
                for _ in 0..(3 * hz) {
                    step(
                        &mut recovering,
                        InputFrame {
                            throttle: 1.0,
                            ..InputFrame::default()
                        },
                        Environment {
                            terrain: TerrainSurface::Mud,
                            ..Environment::default()
                        },
                        hz,
                    );
                }
                let recovery_frames = (stats.terrain_recovery_s * f64::from(hz)).ceil() as u32;
                for _ in 0..recovery_frames {
                    step(
                        &mut recovering,
                        InputFrame {
                            throttle: 1.0,
                            ..InputFrame::default()
                        },
                        Environment::default(),
                        hz,
                    );
                }
                assert!(
                    recovering.state().forward_speed_abs() >= stats.gallop_mps * 0.95,
                    "{archetype:?} at {hz} Hz recovery"
                );
            }
        }
    }

    #[test]
    fn sidestep_block_reasons_cover_air_landing_slope_power_turn_and_stagger() {
        for hz in HZ_VALUES {
            let mut airborne = kernel();
            step(
                &mut airborne,
                InputFrame {
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                hz,
            );
            assert_eq!(
                airborne.telemetry().sidestep_blocked_reason,
                SidestepBlockReason::Airborne
            );
            assert_eq!(airborne.state().lateral_speed_mps, 0.0);

            let mut landing = kernel();
            landing.resolve_motion(Vec3::ZERO, Vec3::ZERO, false);
            landing.resolve_motion(Vec3::ZERO, Vec3::ZERO, true);
            step(
                &mut landing,
                InputFrame {
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            assert_eq!(
                landing.telemetry().sidestep_blocked_reason,
                SidestepBlockReason::LandingRecovery
            );

            let mut slope = kernel();
            for _ in 0..hz {
                step(
                    &mut slope,
                    InputFrame {
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment {
                        slope_angle_radians: 26.0_f64.to_radians(),
                        ..Environment::default()
                    },
                    hz,
                );
            }
            assert_eq!(
                slope.telemetry().sidestep_blocked_reason,
                SidestepBlockReason::OffCamber
            );
            assert_eq!(slope.state().lateral_speed_mps, 0.0);
            assert!(slope.state().yaw_rate_radians.to_degrees().abs() <= 40.0 + 1.0e-9);

            for (environment, reason) in [
                (
                    Environment {
                        power_turning: true,
                        ..Environment::default()
                    },
                    SidestepBlockReason::PowerTurn,
                ),
                (
                    Environment {
                        staggered: true,
                        ..Environment::default()
                    },
                    SidestepBlockReason::Stagger,
                ),
            ] {
                let mut blocked = kernel();
                step(
                    &mut blocked,
                    InputFrame {
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    environment,
                    hz,
                );
                assert_eq!(blocked.telemetry().sidestep_blocked_reason, reason);
                assert_eq!(blocked.state().lateral_speed_mps, 0.0);
            }
        }
    }

    #[test]
    fn majestic_charge_ignores_body_stagger_but_not_headshot_stagger() {
        for hz in HZ_VALUES {
            let mut body_hit = kernel();
            body_hit.set_majestic_charge_active(true);
            step(
                &mut body_hit,
                InputFrame {
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment {
                    staggered: true,
                    ..Environment::default()
                },
                hz,
            );
            assert_eq!(
                body_hit.telemetry().sidestep_blocked_reason,
                SidestepBlockReason::None
            );
            assert!(body_hit.state().lateral_speed_mps > 0.0);

            let mut headshot = kernel();
            headshot.set_majestic_charge_active(true);
            step(
                &mut headshot,
                InputFrame {
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment {
                    staggered: true,
                    headshot_staggered: true,
                    ..Environment::default()
                },
                hz,
            );
            assert_eq!(
                headshot.telemetry().sidestep_blocked_reason,
                SidestepBlockReason::Stagger
            );
            assert_eq!(headshot.state().lateral_speed_mps, 0.0);
        }
    }

    #[test]
    fn every_periodic_sample_has_archetype_lateral_and_vitality() {
        for hz in HZ_VALUES {
            let mut horse = kernel_for(HorseArchetype::Mustang);
            let mut samples = 0;
            for _ in 0..hz {
                let outcome = step(
                    &mut horse,
                    InputFrame {
                        steer: -1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
                if outcome.telemetry_due {
                    let telemetry = horse.telemetry();
                    assert_eq!(telemetry.archetype, HorseArchetype::Mustang);
                    assert_eq!(telemetry.max_vitality, 250.0);
                    assert!(telemetry.lateral_speed_mps < 0.0);
                    assert!(telemetry.lateral_speed_mps >= -1.2);
                    samples += 1;
                }
            }
            assert!(
                (9..=11).contains(&samples),
                "{hz} Hz sample count {samples}"
            );
        }
    }

    #[test]
    fn hard_soft_and_coast_braking_have_distinct_distances() {
        fn stopping_distance(mut horse: HorseKernel, input: InputFrame, hz: u32) -> f64 {
            let start = horse.state().position;
            for _ in 0..(30 * hz) {
                step(&mut horse, input, Environment::default(), hz);
                if horse.state().horizontal_speed() <= 1.0e-6 {
                    return (horse.state().position - start).horizontal_length();
                }
            }
            panic!("horse did not stop");
        }

        for hz in HZ_VALUES {
            let mut full_speed = kernel();
            reach_gallop(&mut full_speed, hz);
            let hard = stopping_distance(
                full_speed.clone(),
                InputFrame {
                    hard_brake: true,
                    ..InputFrame::default()
                },
                hz,
            );
            let soft = stopping_distance(
                full_speed.clone(),
                InputFrame {
                    brake: 1.0,
                    ..InputFrame::default()
                },
                hz,
            );
            let coast = stopping_distance(full_speed, InputFrame::default(), hz);
            assert!(hard <= 20.0, "{hz} Hz hard distance {hard}");
            assert!(soft <= 20.0, "{hz} Hz soft distance {soft}");
            assert!(coast >= 50.0, "{hz} Hz coast distance {coast}");
            assert!(hard < soft && soft < coast);
        }
    }

    #[test]
    fn reset_and_kill_plane_restore_every_state_component() {
        let spawn = Vec3::new(4.0, 2.0, -3.0);
        let spawn_yaw = 0.7;
        for hz in HZ_VALUES {
            let mut horse = HorseKernel::new(Tuning::default(), spawn, spawn_yaw);
            reach_gallop(&mut horse, hz);
            step(
                &mut horse,
                InputFrame {
                    jump_pressed: true,
                    steer: 1.0,
                    ..InputFrame::default()
                },
                Environment::default(),
                hz,
            );
            let outcome = step(
                &mut horse,
                InputFrame {
                    reset: true,
                    ..InputFrame::default()
                },
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                hz,
            );
            assert!(outcome.reset && outcome.telemetry_due);
            assert_eq!(
                outcome.gait_transition.expect("reset transition").new,
                Gait::Idle
            );
            assert_eq!(horse.state().position, spawn);
            assert_eq!(horse.state().velocity, Vec3::ZERO);
            assert_eq!(horse.state().yaw_radians, spawn_yaw);
            assert_eq!(horse.state().gait, Gait::Idle);
            assert_eq!(horse.state().air_time, 0.0);

            horse.resolve_motion(
                Vec3::new(9.0, -20.01, 2.0),
                Vec3::new(1.0, -5.0, 3.0),
                false,
            );
            let killed = step(
                &mut horse,
                InputFrame::default(),
                Environment {
                    grounded: false,
                    ..Environment::default()
                },
                hz,
            );
            assert!(killed.reset);
            assert_eq!(horse.state().position, spawn);
            assert_eq!(horse.state().velocity, Vec3::ZERO);
        }
    }

    #[test]
    fn invalid_delta_is_rejected_without_mutation() {
        for delta in [0.0, -1.0 / 60.0, f64::NAN, f64::INFINITY, 0.251] {
            let mut horse = kernel();
            let before = horse.clone();
            assert!(horse
                .step(InputFrame::default(), Environment::default(), delta)
                .is_err());
            assert_eq!(horse, before);
        }
    }

    #[test]
    fn idle_is_stable_and_telemetry_is_ten_hertz() {
        let mut horse = kernel();
        let mut ticks = Vec::new();
        let mut elapsed = 0.0;
        for _ in 0..(60 * 60) {
            let outcome = step(
                &mut horse,
                InputFrame::default(),
                Environment::default(),
                60,
            );
            elapsed += 1.0 / 60.0;
            if outcome.telemetry_due {
                ticks.push(elapsed);
            }
        }
        assert_eq!(horse.state().gait, Gait::Idle);
        assert!(horse.state().horizontal_speed() < 0.1);
        assert!((590..=610).contains(&ticks.len()));
        assert!(ticks.windows(2).all(|pair| pair[1] - pair[0] <= 0.101));
    }
}
