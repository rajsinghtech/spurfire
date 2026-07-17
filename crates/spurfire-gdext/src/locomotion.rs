//! Deterministic horse locomotion with no Godot types.
//!
//! The kernel owns command/state transitions and computes a requested velocity.
//! An engine adapter may call [`HorseKernel::resolve_motion`] after collision
//! resolution to feed the actual transform and velocity back into the kernel.

use std::f64::consts::PI;
use std::ops::{Add, AddAssign, Mul, Sub};

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

/// Collision and terrain observations for one physics tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    pub grounded: bool,
    pub rough: bool,
    /// Absolute floor angle for telemetry.
    pub slope_angle_radians: f64,
    /// Grade along the horse's heading. Positive is uphill.
    pub signed_slope_radians: f64,
    /// Whether a non-floor slope should force a controlled slide.
    pub steep_slope: bool,
    /// Horizontal downhill direction when [`Self::steep_slope`] is true.
    pub downhill_direction: Vec3,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            grounded: true,
            rough: false,
            slope_angle_radians: 0.0,
            signed_slope_radians: 0.0,
            steep_slope: false,
            downhill_direction: Vec3::ZERO,
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
            walk_speed: 2.0,
            trot_speed: 5.0,
            gallop_speed: 11.0,
            reverse_speed: 1.5,
            walk_acceleration: 3.0,
            trot_acceleration: 4.0,
            gallop_acceleration: 5.0,
            coast_deceleration: 0.8,
            soft_brake_deceleration: 3.5,
            hard_brake_deceleration: 8.0,
            idle_yaw_rate_degrees: 90.0,
            walk_yaw_rate_degrees: 120.0,
            trot_yaw_rate_degrees: 80.0,
            gallop_yaw_rate_degrees: 35.0,
            steering_response: 6.0,
            walk_trot_lateral_damping: 8.0,
            gallop_lateral_damping: 3.0,
            auto_downshift_delay: 0.4,
            idle_speed_floor: 0.3,
            walk_speed_floor: 1.2,
            trot_speed_floor: 3.0,
            gallop_speed_floor: 7.0,
            gravity: 22.0,
            terminal_fall_speed: 30.0,
            jump_impulse: 7.5,
            coyote_time: 0.15,
            jump_buffer_time: 0.12,
            air_control: 0.2,
            rough_speed_multiplier: 0.7,
            rough_acceleration_multiplier: 0.6,
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
    pub steering_axis: f64,
    pub yaw_rate_radians: f64,
    pub acceleration_mps2: f64,
    pub slope_angle_radians: f64,
    pub rough: bool,
    pub grounded: bool,
    pub air_time: f64,
    coyote_remaining: f64,
    jump_buffer_remaining: f64,
    low_speed_time: f64,
    telemetry_elapsed: f64,
}

impl HorseState {
    #[must_use]
    pub fn horizontal_speed(&self) -> f64 {
        self.velocity.horizontal_length()
    }

    #[must_use]
    pub const fn coyote_remaining(&self) -> f64 {
        self.coyote_remaining
    }

    #[must_use]
    pub const fn jump_buffer_remaining(&self) -> f64 {
        self.jump_buffer_remaining
    }
}

/// Stable snapshot used by the Godot telemetry adapter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Telemetry {
    pub speed_mps: f64,
    pub speed_kmh: f64,
    pub gait: Gait,
    pub acceleration_mps2: f64,
    pub yaw_rate_degrees: f64,
    pub slope_angle_degrees: f64,
    pub rough: bool,
    pub position: Vec3,
    pub is_airborne: bool,
    pub air_time: f64,
    pub speed_fraction: f64,
    pub turn_radius_m: f64,
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
    state: HorseState,
    spawn_position: Vec3,
    spawn_yaw_radians: f64,
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
            state: HorseState {
                gait: Gait::Idle,
                position: spawn_position,
                velocity: Vec3::ZERO,
                yaw_radians: spawn_yaw_radians,
                steering_axis: 0.0,
                yaw_rate_radians: 0.0,
                acceleration_mps2: 0.0,
                slope_angle_radians: 0.0,
                rough: false,
                grounded: true,
                air_time: 0.0,
                coyote_remaining: tuning.coyote_time,
                jump_buffer_remaining: 0.0,
                low_speed_time: 0.0,
                telemetry_elapsed: 0.0,
            },
            spawn_position,
            spawn_yaw_radians,
        }
    }

    #[must_use]
    pub const fn tuning(&self) -> &Tuning {
        &self.tuning
    }

    pub fn set_tuning(&mut self, tuning: Tuning) {
        self.tuning = tuning.sanitized();
    }

    #[must_use]
    pub const fn state(&self) -> &HorseState {
        &self.state
    }

    pub fn set_spawn(&mut self, position: Vec3, yaw_radians: f64) {
        if position.is_finite() {
            self.spawn_position = position;
        }
        if yaw_radians.is_finite() {
            self.spawn_yaw_radians = yaw_radians;
        }
    }

    /// Feed collision-resolved engine motion into the next kernel tick.
    pub fn resolve_motion(&mut self, position: Vec3, velocity: Vec3, grounded: bool) {
        if position.is_finite() {
            self.state.position = position;
        }
        if velocity.is_finite() {
            self.state.velocity = velocity;
        }
        self.state.grounded = grounded;
    }

    /// Reset synchronously and return the transition that must be signalled.
    pub fn reset(&mut self) -> GaitTransition {
        let old = self.state.gait;
        self.state = HorseState {
            gait: Gait::Idle,
            position: self.spawn_position,
            velocity: Vec3::ZERO,
            yaw_radians: self.spawn_yaw_radians,
            steering_axis: 0.0,
            yaw_rate_radians: 0.0,
            acceleration_mps2: 0.0,
            slope_angle_radians: 0.0,
            rough: false,
            grounded: true,
            air_time: 0.0,
            coyote_remaining: self.tuning.coyote_time,
            jump_buffer_remaining: 0.0,
            low_speed_time: 0.0,
            telemetry_elapsed: 0.0,
        };
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

        self.state.rough = environment.rough;
        self.state.slope_angle_radians = finite_or_zero(environment.slope_angle_radians)
            .abs()
            .clamp(0.0, PI / 2.0);

        let mut gait_transition = self.apply_player_gait_command(input);
        let jumped = self.integrate_vertical(input, environment.grounded, delta);
        self.integrate_steering(input.steer, environment.grounded, delta);

        let previous_speed = self.state.horizontal_speed();
        self.integrate_horizontal(input, environment, delta);
        let current_speed = self.state.horizontal_speed();
        self.state.acceleration_mps2 = (current_speed - previous_speed) / delta;
        self.state.position += self.state.velocity * delta;

        if gait_transition.is_some() {
            self.state.low_speed_time = 0.0;
        } else {
            gait_transition = self.apply_auto_downshift(input, current_speed, delta);
        }

        self.state.telemetry_elapsed += delta;
        let periodic_telemetry =
            self.state.telemetry_elapsed + f64::EPSILON >= self.tuning.telemetry_interval;
        if periodic_telemetry {
            self.state.telemetry_elapsed %= self.tuning.telemetry_interval;
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
        let speed_mps = self.state.horizontal_speed();
        let yaw_rate = self.state.yaw_rate_radians.abs();
        Telemetry {
            speed_mps,
            speed_kmh: speed_mps * 3.6,
            gait: self.state.gait,
            acceleration_mps2: self.state.acceleration_mps2,
            yaw_rate_degrees: self.state.yaw_rate_radians.to_degrees(),
            slope_angle_degrees: self.state.slope_angle_radians.to_degrees(),
            rough: self.state.rough,
            position: self.state.position,
            is_airborne: !self.state.grounded,
            air_time: self.state.air_time,
            speed_fraction: (speed_mps / self.tuning.gallop_speed).clamp(0.0, 1.0),
            turn_radius_m: if yaw_rate > 1.0e-5 {
                speed_mps / yaw_rate
            } else {
                0.0
            },
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

    fn apply_player_gait_command(&mut self, input: InputFrame) -> Option<GaitTransition> {
        let (new, reason) = match (input.gait_up, input.gait_down) {
            (true, false) => (self.state.gait.up(), TransitionReason::PlayerUp),
            (false, true) => (self.state.gait.down(), TransitionReason::PlayerDown),
            _ => return None,
        };
        if new == self.state.gait {
            return None;
        }
        let old = self.state.gait;
        self.state.gait = new;
        Some(GaitTransition { old, new, reason })
    }

    fn integrate_vertical(&mut self, input: InputFrame, grounded: bool, delta: f64) -> bool {
        self.state.grounded = grounded;
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
            self.state.velocity.y = self.tuning.jump_impulse;
            self.state.grounded = false;
            self.state.coyote_remaining = 0.0;
            self.state.jump_buffer_remaining = 0.0;
            return true;
        }

        self.state.jump_buffer_remaining = (self.state.jump_buffer_remaining - delta).max(0.0);
        if grounded {
            if self.state.velocity.y < 0.0 {
                self.state.velocity.y = 0.0;
            }
        } else {
            self.state.velocity.y = (self.state.velocity.y - self.tuning.gravity * delta)
                .max(-self.tuning.terminal_fall_speed);
        }
        false
    }

    fn integrate_steering(&mut self, raw_steer: f64, grounded: bool, delta: f64) {
        self.state.steering_axis = move_towards(
            self.state.steering_axis,
            raw_steer,
            self.tuning.steering_response * delta,
        );
        let control = if grounded {
            1.0
        } else {
            self.tuning.air_control
        };
        let target_yaw_rate = self.state.steering_axis
            * self.tuning.yaw_rate_degrees(self.state.gait).to_radians()
            * control;
        let blend = 1.0 - (-self.tuning.steering_response * delta).exp();
        self.state.yaw_rate_radians += (target_yaw_rate - self.state.yaw_rate_radians) * blend;
        self.state.yaw_radians =
            wrap_angle(self.state.yaw_radians + self.state.yaw_rate_radians * delta);
    }

    fn integrate_horizontal(&mut self, input: InputFrame, environment: Environment, delta: f64) {
        if environment.steep_slope {
            self.integrate_steep_slide(environment.downhill_direction, delta);
            return;
        }

        let forward = heading_forward(self.state.yaw_radians);
        let right = heading_right(self.state.yaw_radians);
        let old_horizontal = Vec3::new(self.state.velocity.x, 0.0, self.state.velocity.z);
        let old_speed = old_horizontal.horizontal_length();

        if input.hard_brake && old_speed > 0.0 {
            let horizontal = move_vector_towards(
                old_horizontal,
                Vec3::ZERO,
                self.tuning.hard_brake_deceleration * delta,
            );
            self.state.velocity.x = horizontal.x;
            self.state.velocity.z = horizontal.z;
            return;
        }

        let mut longitudinal = old_horizontal.dot(forward);
        let mut lateral = old_horizontal.dot(right);
        let (terrain_speed, terrain_acceleration) = self.terrain_multipliers(environment);
        let gait_cap = self.tuning.target_speed(self.state.gait) * terrain_speed;

        let (target_longitudinal, rate) = if input.brake > 0.0 {
            if longitudinal > 0.0 {
                (0.0, self.tuning.soft_brake_deceleration)
            } else {
                (
                    -self.tuning.reverse_speed * input.brake,
                    self.tuning.walk_acceleration,
                )
            }
        } else if input.throttle > 0.0 {
            (
                gait_cap * input.throttle,
                self.tuning.acceleration(self.state.gait) * terrain_acceleration,
            )
        } else {
            (0.0, self.tuning.coast_deceleration)
        };

        longitudinal = move_towards(longitudinal, target_longitudinal, rate * delta);
        let lateral_damping = if self.state.gait == Gait::Gallop {
            self.tuning.gallop_lateral_damping
        } else {
            self.tuning.walk_trot_lateral_damping
        };
        lateral *= (-lateral_damping * delta).exp();

        let mut horizontal = forward * longitudinal + right * lateral;
        let no_turning_energy_limit = old_speed.max(gait_cap.max(self.tuning.reverse_speed));
        if horizontal.horizontal_length() > no_turning_energy_limit {
            horizontal = horizontal.normalized_horizontal() * no_turning_energy_limit;
        }
        self.state.velocity.x = horizontal.x;
        self.state.velocity.z = horizontal.z;
        if self.state.gait == Gait::Idle
            && input.throttle == 0.0
            && input.brake == 0.0
            && self.state.horizontal_speed() <= self.tuning.idle_speed_floor
        {
            self.state.velocity.x = 0.0;
            self.state.velocity.z = 0.0;
        }
    }

    fn integrate_steep_slide(&mut self, downhill_direction: Vec3, delta: f64) {
        let downhill = downhill_direction.normalized_horizontal();
        let target = downhill * self.tuning.steep_slide_speed;
        let horizontal = Vec3::new(self.state.velocity.x, 0.0, self.state.velocity.z);
        let horizontal = move_vector_towards(horizontal, target, self.tuning.gravity * delta);
        self.state.velocity.x = horizontal.x;
        self.state.velocity.z = horizontal.z;
        self.state.velocity.y = self.state.velocity.y.min(0.0);
    }

    fn terrain_multipliers(&self, environment: Environment) -> (f64, f64) {
        let mut speed = 1.0;
        let mut acceleration = 1.0;
        if environment.rough {
            speed *= self.tuning.rough_speed_multiplier;
            acceleration *= self.tuning.rough_acceleration_multiplier;
        }

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
    Vec3::new(yaw_radians.sin(), 0.0, -yaw_radians.cos())
}

fn heading_right(yaw_radians: f64) -> Vec3 {
    Vec3::new(yaw_radians.cos(), 0.0, yaw_radians.sin())
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
        HorseKernel::new(Tuning::default(), Vec3::ZERO, 0.0)
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
        for _ in 0..(4 * hz) {
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
        assert!((kernel.state().horizontal_speed() - 11.0).abs() < 1.0e-9);
    }

    #[test]
    fn frame_rates_are_bounded_and_acceleration_meets_course_target() {
        let mut final_states = Vec::new();
        for hz in HZ_VALUES {
            let mut horse = kernel();
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
            assert!(horse.state().horizontal_speed() <= 11.0 + 1.0e-9);
            final_states.push(horse.state().clone());
        }

        for pair in final_states.windows(2) {
            assert!((pair[0].horizontal_speed() - pair[1].horizontal_speed()).abs() < 0.03);
            assert!((pair[0].yaw_radians - pair[1].yaw_radians).abs() < 0.04);
            assert!((pair[0].position - pair[1].position).length() < 0.5);
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
            assert!((0.25..=0.35).contains(&drop), "{hz} Hz rough drop {drop}");

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
            assert!(
                (18.0..=30.0).contains(&telemetry.turn_radius_m),
                "{hz} Hz radius {}",
                telemetry.turn_radius_m
            );
            assert!((telemetry.yaw_rate_degrees - 35.0).abs() < 0.1);

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
                        throttle: 1.0,
                        steer: 1.0,
                        ..InputFrame::default()
                    },
                    Environment::default(),
                    hz,
                );
            }
            assert!(walk.telemetry().turn_radius_m < 3.0);

            let ground_rate = walk.state().yaw_rate_radians.abs();
            for _ in 0..hz {
                step(
                    &mut walk,
                    InputFrame {
                        throttle: 1.0,
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
            assert!((horse.state().velocity.y - 7.5).abs() < 1.0e-9);

            let start_y = horse.state().position.y;
            let mut apex = start_y;
            for _ in 0..hz {
                step(&mut horse, InputFrame::default(), airborne, hz);
                apex = apex.max(horse.state().position.y);
            }
            let height = apex - start_y;
            assert!(height > 1.0, "{hz} Hz apex was {height}");
            assert!(height < 1.5);
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
