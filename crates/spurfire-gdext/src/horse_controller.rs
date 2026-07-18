use std::f64::consts::PI;

use godot::classes::{CharacterBody3D, ICharacterBody3D, Input, Marker3D, Node};
use godot::prelude::*;
use spurfire_protocol::{
    HorseRunoutKernel, HorseRunoutState, QuantizedOrigin, SimulationTick, SADDLE_DIVE_TICK_RATE_HZ,
};

use crate::archetype::{HorseArchetype, HorseStats};
use crate::locomotion::{
    Environment, GaitTransition, HorseKernel, InputFrame, SidestepBlockReason, StepOutcome,
    Telemetry, TerrainSurface, Tuning, Vec3 as KernelVec3,
};

const MOVE_FORWARD: &str = "move_forward";
const MOVE_BACK: &str = "move_back";
const STEER_LEFT: &str = "steer_left";
const STEER_RIGHT: &str = "steer_right";
const GAIT_UP: &str = "gait_up";
const GAIT_DOWN: &str = "gait_down";
const HARD_BRAKE: &str = "hard_brake";
const JUMP: &str = "jump";
const RESET_HORSE: &str = "reset_horse";

#[derive(Clone, Copy)]
pub(crate) struct HorseM2Snapshot {
    pub position: Vector3,
    pub velocity: Vector3,
    pub grounded: bool,
    pub gait: i64,
    pub retrievable: bool,
    pub speed_fraction: f64,
    pub yaw_rate_degrees: f64,
    pub gallop_speed_mps: f64,
}

/// Godot adapter for the pure locomotion kernel.
#[derive(GodotClass)]
#[class(base = CharacterBody3D)]
pub struct HorseController {
    #[base]
    base: Base<CharacterBody3D>,

    // Speed and acceleration tuning.
    #[export]
    walk_speed: f64,
    #[export]
    trot_speed: f64,
    #[export]
    gallop_speed: f64,
    #[export]
    reverse_speed: f64,
    #[export]
    walk_acceleration: f64,
    #[export]
    trot_acceleration: f64,
    #[export]
    gallop_acceleration: f64,
    #[export]
    coast_deceleration: f64,
    #[export]
    soft_brake_deceleration: f64,
    #[export]
    hard_brake_deceleration: f64,

    // Steering and gait tuning.
    #[export]
    idle_yaw_rate_degrees: f64,
    #[export]
    walk_yaw_rate_degrees: f64,
    #[export]
    trot_yaw_rate_degrees: f64,
    #[export]
    gallop_yaw_rate_degrees: f64,
    #[export]
    steering_response: f64,
    #[export]
    walk_trot_lateral_damping: f64,
    #[export]
    gallop_lateral_damping: f64,
    #[export]
    auto_downshift_delay: f64,
    #[export]
    idle_speed_floor: f64,
    #[export]
    walk_speed_floor: f64,
    #[export]
    trot_speed_floor: f64,
    #[export]
    gallop_speed_floor: f64,

    // Air and terrain tuning.
    #[export]
    gravity: f64,
    #[export]
    terminal_fall_speed: f64,
    #[export]
    jump_impulse: f64,
    #[export]
    coyote_time: f64,
    #[export]
    jump_buffer_time: f64,
    #[export]
    air_control: f64,
    #[export]
    rough_speed_multiplier: f64,
    #[export]
    rough_acceleration_multiplier: f64,
    #[export]
    uphill_reference_degrees: f64,
    #[export]
    uphill_speed_multiplier: f64,
    #[export]
    downhill_speed_bonus: f64,
    #[export]
    max_climb_degrees: f64,
    #[export]
    steep_slide_speed: f64,

    // Integration and input tuning.
    #[export]
    floor_snap_length: f64,
    #[export]
    kill_plane_y: f64,
    #[export]
    telemetry_interval: f64,
    #[export]
    maximum_dt: f64,
    #[export]
    input_deadzone: f64,
    #[export]
    steer_curve_exponent: f64,
    #[export]
    back_double_tap_window: f64,
    #[export]
    spawn_marker_path: NodePath,

    // Read-only runtime properties for scripts, HUD, and camera code.
    #[var(no_set)]
    archetype: i64,
    #[var(no_set)]
    max_vitality: f64,
    #[var(no_set)]
    vitality: f64,
    #[var(no_set)]
    mass: GString,
    #[var(no_set)]
    mass_class: i64,
    #[var(no_set)]
    stagger_threshold: f64,
    #[var(no_set)]
    gait: i64,
    #[var(no_set)]
    current_gait: i64,
    #[var(no_set)]
    speed_mps: f64,
    #[var(no_set)]
    speed_kmh: f64,
    #[var(no_set)]
    lateral_speed_mps: f64,
    #[var(no_set)]
    acceleration_mps2: f64,
    #[var(no_set)]
    yaw_rate_degrees: f64,
    #[var(no_set)]
    slope_angle_degrees: f64,
    #[var(no_set)]
    surface: GString,
    #[var(no_set)]
    is_airborne: bool,
    #[var(no_set)]
    air_time_s: f64,
    #[var(no_set)]
    speed_fraction: f64,
    #[var(no_set)]
    turn_radius_m: f64,
    #[var(no_set)]
    sidestep_blocked_reason: GString,
    #[var(no_set)]
    last_telemetry: VarDictionary,
    #[var(no_set)]
    control_mode: i64,
    #[var(no_set)]
    is_retrievable: bool,
    #[var(no_set)]
    runout_distance_m: f64,

    kernel: HorseKernel,
    runout_kernel: HorseRunoutKernel,
    external_simulation_enabled: bool,
    last_external_tick: Option<SimulationTick>,
    back_tap_elapsed: f64,
    back_double_tap_active: bool,
}

#[godot_api]
impl HorseController {
    #[signal]
    fn gait_changed(old_gait: i64, new_gait: i64);

    #[signal]
    fn archetype_changed(old: i64, new: i64);

    #[signal]
    fn telemetry_updated(telemetry: VarDictionary);

    #[signal]
    fn runout_started(tick: i64);

    #[signal]
    fn horse_retrievable(tick: i64, distance_m: f64);

    /// Select 0=Courser, 1=Warhorse, or 2=Mustang.
    #[func]
    pub fn set_archetype(&mut self, id: i64) {
        let Ok(archetype) = HorseArchetype::try_from(id) else {
            godot_error!(
                "HorseController rejected archetype id {id}; expected 0=Courser, 1=Warhorse, or 2=Mustang"
            );
            return;
        };
        let old = self.kernel.archetype();
        if !self.kernel.set_archetype(archetype) {
            return;
        }
        self.update_archetype_properties();
        let clamped_velocity = to_godot_vector(self.kernel.state().velocity);
        self.base_mut().set_velocity(clamped_velocity);
        self.signals()
            .archetype_changed()
            .emit(old.id(), archetype.id());
        self.emit_telemetry();
    }

    /// Return the complete locked design row for the active archetype.
    #[func]
    pub fn get_archetype_stats(&self) -> VarDictionary {
        archetype_stats_dictionary(self.kernel.archetype_stats())
    }

    /// Start fixed-heading, collision-resolved M2 horse continuation.
    #[func]
    pub fn start_dive_runout(&mut self, tick: i64) -> bool {
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return false;
        };
        let Some(position) = quantized_origin(self.base().get_global_position()) else {
            return false;
        };
        let velocity = quantized_velocity(self.base().get_velocity());
        if self
            .runout_kernel
            .start_runout(tick, position, velocity)
            .is_err()
        {
            return false;
        }
        self.assign_runout_properties();
        self.signals()
            .runout_started()
            .emit(i64::try_from(tick.as_u64()).unwrap_or(i64::MAX));
        true
    }

    /// Stop in place for an ordinary below-threshold dismount.
    #[func]
    pub fn stop_for_dismount(&mut self, tick: i64) -> bool {
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return false;
        };
        let Some(position) = quantized_origin(self.base().get_global_position()) else {
            return false;
        };
        self.runout_kernel.stop_for_dismount(tick, position);
        self.base_mut().set_velocity(Vector3::ZERO);
        self.assign_runout_properties();
        self.emit_retrievable(tick);
        true
    }

    /// Advance the same horse object on the injected absolute 60 Hz clock.
    #[func]
    pub fn set_external_simulation_tick(&mut self, tick: i64) -> bool {
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return false;
        };
        if self
            .last_external_tick
            .is_some_and(|current| tick <= current)
        {
            return false;
        }
        self.external_simulation_enabled = true;
        self.last_external_tick = Some(tick);
        self.simulate_external_tick(tick)
    }

    /// Return player control after the rider kernel has range-checked remount.
    #[func]
    pub fn complete_remount(&mut self, tick: i64) -> bool {
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return false;
        };
        if self.runout_kernel.complete_remount(tick).is_err() {
            return false;
        }
        self.assign_runout_properties();
        true
    }

    /// Restore the configured spawn marker synchronously. Course reset is an
    /// explicit censor/reset path, never normal horse retrieval.
    #[func]
    pub fn reset_horse(&mut self) {
        self.refresh_spawn();
        let transition = self.kernel.reset();
        self.runout_kernel =
            HorseRunoutKernel::new(SADDLE_DIVE_TICK_RATE_HZ).expect("M2 tick rate is nonzero");
        self.last_external_tick = None;
        self.assign_runout_properties();
        self.apply_kernel_transform();
        self.emit_transition(transition);
        self.emit_telemetry();
    }
}

#[godot_api]
impl ICharacterBody3D for HorseController {
    fn init(base: Base<CharacterBody3D>) -> Self {
        let tuning = Tuning::default();
        let default_archetype = HorseArchetype::default();
        let default_stats = default_archetype.stats();
        Self {
            base,
            walk_speed: tuning.walk_speed,
            trot_speed: tuning.trot_speed,
            gallop_speed: tuning.gallop_speed,
            reverse_speed: tuning.reverse_speed,
            walk_acceleration: tuning.walk_acceleration,
            trot_acceleration: tuning.trot_acceleration,
            gallop_acceleration: tuning.gallop_acceleration,
            coast_deceleration: tuning.coast_deceleration,
            soft_brake_deceleration: tuning.soft_brake_deceleration,
            hard_brake_deceleration: tuning.hard_brake_deceleration,
            idle_yaw_rate_degrees: tuning.idle_yaw_rate_degrees,
            walk_yaw_rate_degrees: tuning.walk_yaw_rate_degrees,
            trot_yaw_rate_degrees: tuning.trot_yaw_rate_degrees,
            gallop_yaw_rate_degrees: tuning.gallop_yaw_rate_degrees,
            steering_response: tuning.steering_response,
            walk_trot_lateral_damping: tuning.walk_trot_lateral_damping,
            gallop_lateral_damping: tuning.gallop_lateral_damping,
            auto_downshift_delay: tuning.auto_downshift_delay,
            idle_speed_floor: tuning.idle_speed_floor,
            walk_speed_floor: tuning.walk_speed_floor,
            trot_speed_floor: tuning.trot_speed_floor,
            gallop_speed_floor: tuning.gallop_speed_floor,
            gravity: tuning.gravity,
            terminal_fall_speed: tuning.terminal_fall_speed,
            jump_impulse: tuning.jump_impulse,
            coyote_time: tuning.coyote_time,
            jump_buffer_time: tuning.jump_buffer_time,
            air_control: tuning.air_control,
            rough_speed_multiplier: tuning.rough_speed_multiplier,
            rough_acceleration_multiplier: tuning.rough_acceleration_multiplier,
            uphill_reference_degrees: tuning.uphill_reference_degrees,
            uphill_speed_multiplier: tuning.uphill_speed_multiplier,
            downhill_speed_bonus: tuning.downhill_speed_bonus,
            max_climb_degrees: tuning.max_climb_degrees,
            steep_slide_speed: tuning.steep_slide_speed,
            floor_snap_length: tuning.floor_snap_length,
            kill_plane_y: tuning.kill_plane_y,
            telemetry_interval: tuning.telemetry_interval,
            maximum_dt: tuning.maximum_dt,
            input_deadzone: 0.15,
            steer_curve_exponent: 1.5,
            back_double_tap_window: 0.25,
            spawn_marker_path: NodePath::from("../HorseSpawn"),
            archetype: default_archetype.id(),
            max_vitality: default_stats.max_vitality,
            vitality: default_stats.max_vitality,
            mass: GString::from(default_stats.mass.name()),
            mass_class: default_stats.mass as i64,
            stagger_threshold: default_stats.stagger_threshold,
            gait: 0,
            current_gait: 0,
            speed_mps: 0.0,
            speed_kmh: 0.0,
            lateral_speed_mps: 0.0,
            acceleration_mps2: 0.0,
            yaw_rate_degrees: 0.0,
            slope_angle_degrees: 0.0,
            surface: GString::from("flat"),
            is_airborne: false,
            air_time_s: 0.0,
            speed_fraction: 0.0,
            turn_radius_m: 0.0,
            sidestep_blocked_reason: GString::from(SidestepBlockReason::None.name()),
            last_telemetry: VarDictionary::new(),
            control_mode: 0,
            is_retrievable: false,
            runout_distance_m: 0.0,
            kernel: HorseKernel::new(tuning, KernelVec3::ZERO, 0.0),
            runout_kernel: HorseRunoutKernel::new(SADDLE_DIVE_TICK_RATE_HZ)
                .expect("M2 tick rate is nonzero"),
            external_simulation_enabled: false,
            last_external_tick: None,
            back_tap_elapsed: f64::INFINITY,
            back_double_tap_active: false,
        }
    }

    fn ready(&mut self) {
        self.sync_tuning();
        self.configure_character_body();
        self.refresh_spawn();
        let position = from_godot_vector(self.base().get_global_position());
        let velocity = from_godot_vector(self.base().get_velocity());
        let grounded = self.base().is_on_floor();
        self.kernel.resolve_motion(position, velocity, grounded);
        self.update_archetype_properties();
        self.assign_runout_properties();
        let archetype = self.kernel.archetype().id();
        self.signals()
            .archetype_changed()
            .emit(archetype, archetype);
        self.emit_telemetry();
    }

    fn physics_process(&mut self, delta: f64) {
        if self.external_simulation_enabled {
            return;
        }
        self.simulate_player_control(delta);
    }
}

impl HorseController {
    pub(crate) fn m2_snapshot(&self) -> HorseM2Snapshot {
        HorseM2Snapshot {
            position: self.base().get_global_position(),
            velocity: self.base().get_velocity(),
            grounded: self.base().is_on_floor(),
            gait: self.current_gait,
            retrievable: self.runout_kernel.is_retrievable(),
            speed_fraction: self.speed_fraction,
            yaw_rate_degrees: self.yaw_rate_degrees,
            gallop_speed_mps: self.gallop_speed,
        }
    }

    pub(crate) fn can_start_authoritative_runout(&self) -> bool {
        self.runout_kernel.state() == HorseRunoutState::PlayerControlled
    }

    fn simulate_external_tick(&mut self, tick: SimulationTick) -> bool {
        self.sync_tuning();
        self.configure_character_body();
        let Ok(runout) = self.runout_kernel.begin_tick(tick) else {
            return false;
        };
        self.assign_runout_properties();
        if let Some(transition) = runout.transition {
            if transition.to == HorseRunoutState::IdleRetrievable {
                self.emit_retrievable(transition.tick);
            }
        }

        match runout.state {
            HorseRunoutState::PlayerControlled => {
                self.simulate_player_control(1.0 / f64::from(SADDLE_DIVE_TICK_RATE_HZ));
            }
            HorseRunoutState::Runout | HorseRunoutState::IdleRetrievable => {
                let velocity = Vector3::new(
                    runout.requested_velocity_mmps[0] as f32 / 1_000.0,
                    runout.requested_velocity_mmps[1] as f32 / 1_000.0,
                    runout.requested_velocity_mmps[2] as f32 / 1_000.0,
                );
                self.base_mut().set_velocity(velocity);
                if runout.state == HorseRunoutState::Runout {
                    self.base_mut().move_and_slide();
                }
                let mut position = self.base().get_global_position();
                let resolved_velocity = self.base().get_velocity();
                let Some(position_mm) = quantized_origin(position) else {
                    return false;
                };
                let clamped_position_mm = self.runout_kernel.clamp_motion_position(position_mm);
                if clamped_position_mm != position_mm {
                    position.x = clamped_position_mm.x as f32 / 1_000.0;
                    position.z = clamped_position_mm.z as f32 / 1_000.0;
                    self.base_mut().set_global_position(position);
                }
                let transition = match self.runout_kernel.resolve_motion(
                    tick,
                    clamped_position_mm,
                    quantized_velocity(resolved_velocity),
                ) {
                    Ok(transition) => transition,
                    Err(error) => {
                        godot_error!("horse runout collision feedback rejected: {error}");
                        return false;
                    }
                };
                self.kernel.resolve_motion(
                    from_godot_vector(position),
                    from_godot_vector(resolved_velocity),
                    self.base().is_on_floor(),
                );
                self.assign_runout_properties();
                self.update_telemetry_properties();
                if let Some(transition) = transition {
                    if transition.to == HorseRunoutState::IdleRetrievable {
                        self.base_mut().set_velocity(Vector3::ZERO);
                        self.emit_retrievable(transition.tick);
                    }
                }
            }
        }
        true
    }

    fn simulate_player_control(&mut self, delta: f64) {
        self.sync_tuning();
        self.configure_character_body();

        let position = from_godot_vector(self.base().get_global_position());
        let velocity = from_godot_vector(self.base().get_velocity());
        let grounded = self.base().is_on_floor();
        self.kernel.resolve_motion(position, velocity, grounded);

        let input = self.sample_input(delta);
        let environment = self.sample_environment();
        let Ok(outcome) = self.kernel.step(input, environment, delta) else {
            return;
        };

        if outcome.reset {
            self.runout_kernel =
                HorseRunoutKernel::new(SADDLE_DIVE_TICK_RATE_HZ).expect("M2 tick rate is nonzero");
            self.assign_runout_properties();
            self.apply_kernel_transform();
        } else {
            self.apply_requested_motion(outcome);
            self.resolve_godot_motion();
        }

        if let Some(transition) = outcome.gait_transition {
            self.emit_transition(transition);
        }
        if outcome.telemetry_due {
            self.emit_telemetry();
        } else {
            self.update_telemetry_properties();
        }
    }

    fn assign_runout_properties(&mut self) {
        self.control_mode = match self.runout_kernel.state() {
            HorseRunoutState::PlayerControlled => 0,
            HorseRunoutState::Runout => 1,
            HorseRunoutState::IdleRetrievable => 2,
        };
        self.is_retrievable = self.runout_kernel.is_retrievable();
        self.runout_distance_m = f64::from(self.runout_kernel.cumulative_travel_mm()) / 1_000.0;
    }

    fn emit_retrievable(&mut self, tick: SimulationTick) {
        self.assign_runout_properties();
        let distance = self.runout_distance_m;
        self.signals()
            .horse_retrievable()
            .emit(i64::try_from(tick.as_u64()).unwrap_or(i64::MAX), distance);
    }

    fn exported_tuning(&self) -> Tuning {
        Tuning {
            walk_speed: self.walk_speed,
            trot_speed: self.trot_speed,
            gallop_speed: self.gallop_speed,
            reverse_speed: self.reverse_speed,
            walk_acceleration: self.walk_acceleration,
            trot_acceleration: self.trot_acceleration,
            gallop_acceleration: self.gallop_acceleration,
            coast_deceleration: self.coast_deceleration,
            soft_brake_deceleration: self.soft_brake_deceleration,
            hard_brake_deceleration: self.hard_brake_deceleration,
            idle_yaw_rate_degrees: self.idle_yaw_rate_degrees,
            walk_yaw_rate_degrees: self.walk_yaw_rate_degrees,
            trot_yaw_rate_degrees: self.trot_yaw_rate_degrees,
            gallop_yaw_rate_degrees: self.gallop_yaw_rate_degrees,
            steering_response: self.steering_response,
            walk_trot_lateral_damping: self.walk_trot_lateral_damping,
            gallop_lateral_damping: self.gallop_lateral_damping,
            auto_downshift_delay: self.auto_downshift_delay,
            idle_speed_floor: self.idle_speed_floor,
            walk_speed_floor: self.walk_speed_floor,
            trot_speed_floor: self.trot_speed_floor,
            gallop_speed_floor: self.gallop_speed_floor,
            gravity: self.gravity,
            terminal_fall_speed: self.terminal_fall_speed,
            jump_impulse: self.jump_impulse,
            coyote_time: self.coyote_time,
            jump_buffer_time: self.jump_buffer_time,
            air_control: self.air_control,
            rough_speed_multiplier: self.rough_speed_multiplier,
            rough_acceleration_multiplier: self.rough_acceleration_multiplier,
            uphill_reference_degrees: self.uphill_reference_degrees,
            uphill_speed_multiplier: self.uphill_speed_multiplier,
            downhill_speed_bonus: self.downhill_speed_bonus,
            max_climb_degrees: self.max_climb_degrees,
            steep_slide_speed: self.steep_slide_speed,
            floor_snap_length: self.floor_snap_length,
            kill_plane_y: self.kill_plane_y,
            telemetry_interval: self.telemetry_interval,
            maximum_dt: self.maximum_dt,
        }
    }

    fn sync_tuning(&mut self) {
        let tuning = self.exported_tuning();
        self.kernel.set_tuning(tuning);
    }

    fn configure_character_body(&mut self) {
        let tuning = *self.kernel.tuning();
        let mut base = self.base_mut();
        base.set_up_direction(Vector3::UP);
        base.set_floor_max_angle(tuning.max_climb_degrees.to_radians() as f32);
        base.set_floor_snap_length(tuning.floor_snap_length as f32);
        base.set_floor_block_on_wall_enabled(true);
        base.set_slide_on_ceiling_enabled(false);
    }

    fn refresh_spawn(&mut self) {
        let fallback = self.base().get_global_transform();
        let transform = if self.spawn_marker_path.is_empty() {
            fallback
        } else {
            self.base()
                .get_node_or_null(&self.spawn_marker_path)
                .and_then(|node| node.try_cast::<Marker3D>().ok())
                .map_or(fallback, |marker| marker.get_global_transform())
        };
        self.kernel.set_spawn(
            from_godot_vector(transform.origin),
            f64::from(transform.basis.get_euler().y),
        );
    }

    fn sample_input(&mut self, delta: f64) -> InputFrame {
        let input = Input::singleton();
        let forward = remap_deadzone(
            f64::from(input.get_action_strength(MOVE_FORWARD)),
            self.input_deadzone,
        );
        let back = remap_deadzone(
            f64::from(input.get_action_strength(MOVE_BACK)),
            self.input_deadzone,
        );
        let raw_steer = f64::from(
            input.get_action_strength(STEER_RIGHT) - input.get_action_strength(STEER_LEFT),
        );
        let steer = curved_axis(raw_steer, self.input_deadzone, self.steer_curve_exponent);

        self.back_tap_elapsed = (self.back_tap_elapsed + delta).min(60.0);
        let back_just_pressed = input.is_action_just_pressed(MOVE_BACK);
        let double_tapped = back_just_pressed
            && self.back_tap_elapsed <= finite_positive_or(self.back_double_tap_window, 0.25);
        if back_just_pressed {
            self.back_tap_elapsed = 0.0;
            if double_tapped {
                self.back_double_tap_active = true;
            }
        }
        if !input.is_action_pressed(MOVE_BACK) {
            self.back_double_tap_active = false;
        }

        InputFrame {
            throttle: forward,
            brake: back,
            steer,
            hard_brake: input.is_action_pressed(HARD_BRAKE) || self.back_double_tap_active,
            gait_up: input.is_action_just_pressed(GAIT_UP),
            gait_down: input.is_action_just_pressed(GAIT_DOWN) || double_tapped,
            jump_pressed: input.is_action_just_pressed(JUMP),
            reset: input.is_action_just_pressed(RESET_HORSE),
        }
    }

    fn sample_environment(&self) -> Environment {
        let grounded = self.base().is_on_floor();
        let forward = heading_forward(self.kernel.state().yaw_radians);
        let (terrain, steep_downhill) = self.inspect_slide_collisions();

        let (slope_angle_radians, signed_slope_radians) = if grounded {
            let normal = from_godot_vector(self.base().get_floor_normal());
            let normal_y = normal.y.clamp(-1.0, 1.0);
            let angle = normal_y.acos();
            let grade =
                (-(normal.x * forward.x + normal.z * forward.z)).atan2(normal_y.max(1.0e-6));
            (angle, grade)
        } else {
            (0.0, 0.0)
        };

        Environment {
            grounded,
            rough: terrain != TerrainSurface::Flat,
            terrain,
            slope_angle_radians,
            signed_slope_radians,
            steep_slope: steep_downhill.is_some(),
            downhill_direction: steep_downhill.unwrap_or(KernelVec3::ZERO),
            ..Environment::default()
        }
    }

    fn inspect_slide_collisions(&self) -> (TerrainSurface, Option<KernelVec3>) {
        let count = self.base().get_slide_collision_count();
        let max_floor_angle = self.kernel.tuning().max_climb_degrees.to_radians();
        let mut terrain = TerrainSurface::Flat;
        let mut steep_downhill = None;

        for index in 0..count {
            let Some(collision) = self.base().get_slide_collision(index) else {
                continue;
            };
            let normal = from_godot_vector(collision.get_normal());
            let angle = normal.y.clamp(-1.0, 1.0).acos();
            if normal.y > 0.5 {
                if let Some(collider) = collision.get_collider() {
                    terrain = more_specific_terrain(terrain, Self::collider_terrain(collider));
                }
            }
            if normal.y > 0.05 && angle > max_floor_angle && angle < PI / 2.0 {
                let downhill = KernelVec3::new(normal.x, 0.0, normal.z).normalized_horizontal();
                if downhill != KernelVec3::ZERO {
                    steep_downhill = Some(downhill);
                }
            }
        }
        (terrain, steep_downhill)
    }

    fn collider_terrain(collider: Gd<Object>) -> TerrainSurface {
        for (key, terrain) in [
            ("mud", TerrainSurface::Mud),
            ("riverbed", TerrainSurface::Riverbed),
            ("scrub", TerrainSurface::Scrub),
        ] {
            if collider.has_meta(key) && collider.get_meta(key).try_to::<bool>().unwrap_or(true) {
                return terrain;
            }
        }
        for key in ["rough", "rough_terrain"] {
            if collider.has_meta(key) {
                return if collider.get_meta(key).try_to::<bool>().unwrap_or(true) {
                    TerrainSurface::Scrub
                } else {
                    TerrainSurface::Flat
                };
            }
        }
        let Ok(node) = collider.try_cast::<Node>() else {
            return TerrainSurface::Flat;
        };
        if node.is_in_group("mud") {
            TerrainSurface::Mud
        } else if node.is_in_group("riverbed") {
            TerrainSurface::Riverbed
        } else if node.is_in_group("scrub")
            || node.is_in_group("rough")
            || node.is_in_group("rough_terrain")
        {
            TerrainSurface::Scrub
        } else {
            TerrainSurface::Flat
        }
    }

    fn apply_requested_motion(&mut self, outcome: StepOutcome) {
        let mut base = self.base_mut();
        base.set_global_rotation(Vector3::new(0.0, outcome.yaw_radians as f32, 0.0));
        base.set_velocity(to_godot_vector(outcome.velocity));
    }

    fn resolve_godot_motion(&mut self) {
        self.base_mut().move_and_slide();
        let position = from_godot_vector(self.base().get_global_position());
        let velocity = from_godot_vector(self.base().get_velocity());
        let grounded = self.base().is_on_floor();
        self.kernel.resolve_motion(position, velocity, grounded);
    }

    fn apply_kernel_transform(&mut self) {
        let state = self.kernel.state();
        let position = to_godot_vector(state.position);
        let yaw = state.yaw_radians as f32;
        self.back_tap_elapsed = f64::INFINITY;
        self.back_double_tap_active = false;
        let mut base = self.base_mut();
        base.set_global_position(position);
        base.set_global_rotation(Vector3::new(0.0, yaw, 0.0));
        base.set_velocity(Vector3::ZERO);
    }

    fn emit_transition(&mut self, transition: GaitTransition) {
        self.signals()
            .gait_changed()
            .emit(transition.old as i64, transition.new as i64);
    }

    fn emit_telemetry(&mut self) {
        let payload = self.update_telemetry_properties();
        self.signals().telemetry_updated().emit(&payload);
    }

    fn update_archetype_properties(&mut self) {
        let archetype = self.kernel.archetype();
        let stats = archetype.stats();
        self.archetype = archetype.id();
        self.max_vitality = stats.max_vitality;
        self.vitality = stats.max_vitality;
        self.mass = GString::from(stats.mass.name());
        self.mass_class = stats.mass as i64;
        self.stagger_threshold = stats.stagger_threshold;
    }

    fn update_telemetry_properties(&mut self) -> VarDictionary {
        let telemetry = self.kernel.telemetry();
        self.assign_telemetry_properties(telemetry);

        let mut payload = VarDictionary::new();
        payload.set("archetype", telemetry.archetype.id());
        payload.set("speed_mps", telemetry.speed_mps);
        payload.set("speed_kmh", telemetry.speed_kmh);
        payload.set("lateral_speed_mps", telemetry.lateral_speed_mps);
        payload.set("max_vitality", telemetry.max_vitality);
        payload.set("gait", telemetry.gait as i64);
        payload.set("acceleration_mps2", telemetry.acceleration_mps2);
        payload.set("yaw_rate_degs", telemetry.yaw_rate_degrees);
        payload.set("slope_angle_deg", telemetry.slope_angle_degrees);
        payload.set("surface", terrain_name(telemetry.terrain));
        payload.set("position", to_godot_vector(telemetry.position));
        payload.set("is_airborne", telemetry.is_airborne);
        payload.set("air_time_s", telemetry.air_time);
        payload.set("speed_fraction", telemetry.speed_fraction);
        payload.set("turn_radius_m", telemetry.turn_radius_m);
        payload.set(
            "sidestep_blocked_reason",
            telemetry.sidestep_blocked_reason.name(),
        );
        self.last_telemetry = payload.clone();
        payload
    }

    fn assign_telemetry_properties(&mut self, telemetry: Telemetry) {
        self.archetype = telemetry.archetype.id();
        self.max_vitality = telemetry.max_vitality;
        self.gait = telemetry.gait as i64;
        self.current_gait = telemetry.gait as i64;
        self.speed_mps = telemetry.speed_mps;
        self.speed_kmh = telemetry.speed_kmh;
        self.lateral_speed_mps = telemetry.lateral_speed_mps;
        self.acceleration_mps2 = telemetry.acceleration_mps2;
        self.yaw_rate_degrees = telemetry.yaw_rate_degrees;
        self.slope_angle_degrees = telemetry.slope_angle_degrees;
        self.surface = GString::from(terrain_name(telemetry.terrain));
        self.is_airborne = telemetry.is_airborne;
        self.air_time_s = telemetry.air_time;
        self.speed_fraction = telemetry.speed_fraction;
        self.turn_radius_m = telemetry.turn_radius_m;
        self.sidestep_blocked_reason = GString::from(telemetry.sidestep_blocked_reason.name());
    }
}

fn archetype_stats_dictionary(stats: &HorseStats) -> VarDictionary {
    let mut row = VarDictionary::new();
    row.set("walk_mps", stats.walk_mps);
    row.set("trot_mps", stats.trot_mps);
    row.set("gallop_mps", stats.gallop_mps);
    row.set("sprint_mps", stats.sprint_mps);
    row.set("accel_0_to_gallop_s", stats.accel_0_to_gallop_s);
    row.set("turn_walk_deg_s", stats.turn_walk_deg_s);
    row.set("turn_gallop_deg_s", stats.turn_gallop_deg_s);
    row.set("drift_deg_s", stats.drift_deg_s);
    row.set("jump_apex_m", stats.jump_apex_m);
    row.set("jump_airtime_s", stats.jump_airtime_s);
    row.set("terrain_scrub", stats.terrain_scrub);
    row.set("terrain_mud", stats.terrain_mud);
    row.set("terrain_riverbed", stats.terrain_riverbed);
    row.set("terrain_recovery_s", stats.terrain_recovery_s);
    row.set("max_vitality", stats.max_vitality);
    row.set("stagger_threshold", stats.stagger_threshold);
    row.set("sidestep_mps", stats.sidestep_mps);
    row.set("sidestep_ramp_s", stats.sidestep_ramp_s);
    row
}

fn terrain_name(terrain: TerrainSurface) -> &'static str {
    match terrain {
        TerrainSurface::Flat => "flat",
        TerrainSurface::Scrub => "scrub",
        TerrainSurface::Mud => "mud",
        TerrainSurface::Riverbed => "riverbed",
    }
}

fn more_specific_terrain(current: TerrainSurface, candidate: TerrainSurface) -> TerrainSurface {
    fn priority(terrain: TerrainSurface) -> u8 {
        match terrain {
            TerrainSurface::Flat => 0,
            TerrainSurface::Riverbed => 1,
            TerrainSurface::Scrub => 2,
            TerrainSurface::Mud => 3,
        }
    }
    if priority(candidate) > priority(current) {
        candidate
    } else {
        current
    }
}

fn heading_forward(yaw_radians: f64) -> KernelVec3 {
    KernelVec3::new(-yaw_radians.sin(), 0.0, -yaw_radians.cos())
}

fn from_godot_vector(value: Vector3) -> KernelVec3 {
    KernelVec3::new(f64::from(value.x), f64::from(value.y), f64::from(value.z))
}

fn to_godot_vector(value: KernelVec3) -> Vector3 {
    Vector3::new(value.x as f32, value.y as f32, value.z as f32)
}

fn remap_deadzone(value: f64, deadzone: f64) -> f64 {
    let deadzone = if deadzone.is_finite() {
        deadzone.clamp(0.0, 0.95)
    } else {
        0.15
    };
    if value <= deadzone {
        0.0
    } else {
        ((value - deadzone) / (1.0 - deadzone)).clamp(0.0, 1.0)
    }
}

fn curved_axis(value: f64, deadzone: f64, exponent: f64) -> f64 {
    let sign = value.signum();
    let magnitude = remap_deadzone(value.abs(), deadzone);
    let exponent = finite_positive_or(exponent, 1.5);
    sign * magnitude.powf(exponent)
}

fn finite_positive_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn quantized_origin(value: Vector3) -> Option<QuantizedOrigin> {
    QuantizedOrigin::from_meters(f64::from(value.x), f64::from(value.y), f64::from(value.z)).ok()
}

fn quantized_velocity(value: Vector3) -> [i32; 3] {
    [value.x, value.y, value.z].map(|component| {
        if !component.is_finite() {
            0
        } else {
            (f64::from(component) * 1_000.0)
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_and_curve_preserve_range_and_sign() {
        assert_eq!(remap_deadzone(0.1, 0.15), 0.0);
        assert_eq!(curved_axis(0.1, 0.15, 1.5), 0.0);
        assert_eq!(curved_axis(1.0, 0.15, 1.5), 1.0);
        assert_eq!(curved_axis(-1.0, 0.15, 1.5), -1.0);
        let fine = curved_axis(0.5, 0.15, 1.5);
        assert!(fine > 0.0 && fine < remap_deadzone(0.5, 0.15));
    }
}
