//! Thin Godot `CharacterBody3D` adapter for the deterministic M2 kernels.

use godot::classes::{CharacterBody3D, CollisionShape3D, ICharacterBody3D, Node, Node3D, Object};
use godot::prelude::*;
use spurfire_protocol::{
    AcceptedShotMetadata, CombatGait, DamageObservation, DamageObservationId, DiveCensorReason,
    DiveId, DiveInstrumentationRow, GameplayEventKind, GameplayEventRow, HitZone, LandingOutcome,
    LandingTerrain, PlayerId, QuantizedDirection, QuantizedOrigin, RiderMotionObservation,
    RiderStance, SaddleDiveCommand, SaddleDiveEffects, SaddleDiveKernel, SaddleDiveState,
    SaddleDiveTickInput, ShotResultAttribution, SimulationTick, WeaponId, BAD_LANDING_DAMAGE,
    DIRECTION_UNITS, MIN_DIVE_SPEED_MMPS, MOVEMENT_SCALE_FULL_MILLI,
    MOVEMENT_SCALE_PRONE_MILLI, MOVEMENT_SCALE_RECOVERY_MILLI, SADDLE_DIVE_GRAVITY_MMPS2,
    SADDLE_DIVE_TICK_RATE_HZ,
};

use crate::horse_controller::HorseController;
use crate::mounted_weapon_controller::MountedWeaponController;

const DEFAULT_ACTOR_ID: &str = "00000000-0000-4000-8000-000000000001";

/// One logical rider body. Presentation scripts may consume its signals, but
/// every gameplay gate and timer remains in `SaddleDiveKernel`.
#[derive(GodotClass)]
#[class(base = CharacterBody3D)]
pub struct SaddleDiveController {
    #[base]
    base: Base<CharacterBody3D>,

    #[export]
    horse_path: NodePath,
    #[export]
    saddle_path: NodePath,
    #[export]
    collision_shape_path: NodePath,
    #[export]
    weapon_controller_path: NodePath,
    #[export]
    actor_id: GString,
    #[export]
    authority_epoch: i64,
    #[export]
    on_foot_speed_mps: f64,

    #[var(no_set)]
    current_tick: i64,
    #[var(no_set)]
    stance_id: i64,
    #[var(no_set)]
    stance_known: bool,
    #[var(no_set)]
    dive_id: i64,
    #[var(no_set)]
    movement_scale: f64,
    #[var(no_set)]
    airtime_seconds: f64,
    #[var(no_set)]
    rider_health: i64,
    #[var(no_set)]
    can_fire: bool,
    #[var(no_set)]
    can_reload: bool,

    kernel: SaddleDiveKernel,
    chosen_forward: Vector3,
    move_input: Vector2,
    mounted_grounded: bool,
    motion_begun: bool,
    death_signal_emitted: bool,
}

#[godot_api]
impl SaddleDiveController {
    #[signal]
    fn stance_changed(previous_id: i64, current_id: i64, tick: i64, dive_id: i64);

    #[signal]
    fn dive_started(dive_id: i64, launch_velocity: Vector3, clamped_angle_degrees: f64);

    #[signal]
    fn dive_landed(dive_id: i64, bad: bool, slope_degrees: f64, terrain: GString);

    #[signal]
    fn recovery_changed(dive_id: i64, phase: GString, movement_scale: f64);

    #[signal]
    fn recovery_completed(dive_id: i64);

    #[signal]
    fn landing_damage_applied(dive_id: i64, amount: i64, health_after: i64);

    #[signal]
    fn gameplay_event(event_id: GString, kind: GString, payload: VarDictionary);

    #[signal]
    fn dive_telemetry_updated(row: VarDictionary);

    #[signal]
    fn dive_telemetry_finalized(row: VarDictionary);

    #[signal]
    fn rider_died(tick: i64);

    /// Camera/HUD presentation row for the logical rider currently followed.
    #[signal]
    fn telemetry_updated(telemetry: VarDictionary);

    /// Begin one shared absolute gameplay tick. Movement is resolved separately
    /// after same-tick combat through `resolve_motion`.
    #[func]
    pub fn advance_tick(
        &mut self,
        tick: i64,
        interact_pressed: bool,
        chosen_direction: Vector3,
        move_input: Vector2,
        weapon_id: i64,
    ) -> bool {
        let (Ok(tick_value), Ok(weapon_id)) = (u64::try_from(tick), WeaponId::try_from(weapon_id))
        else {
            return false;
        };
        let Some(horse) = self.horse() else {
            return false;
        };
        let Some(mut weapon) = self.weapon_controller() else {
            return false;
        };
        if self.kernel.state() == SaddleDiveState::Mounted {
            self.follow_saddle();
        }
        let horse_snapshot = horse.bind().m2_snapshot();
        self.mounted_grounded = horse_snapshot.grounded;
        self.chosen_forward = finite_planar_direction(chosen_direction)
            .unwrap_or_else(|| horse_forward(self.base().get_global_rotation().y));
        self.move_input = finite_move_input(move_input);

        let tick = SimulationTick::new(tick_value);
        let Some(horse_position) = quantized_origin(horse_snapshot.position) else {
            return false;
        };
        let Some(rider_position) = quantized_origin(self.base().get_global_position()) else {
            return false;
        };
        let actor = self.kernel.actor();
        let authority_epoch = self.kernel.authority_epoch();
        let horse_velocity_mmps = quantized_velocity(horse_snapshot.velocity);
        if self.kernel.state() == SaddleDiveState::Mounted {
            let mounted_stance = if horse_snapshot.grounded {
                RiderStance::Mounted
            } else {
                RiderStance::MountedAirborne
            };
            if !weapon
                .bind_mut()
                .synchronize_authoritative_stance(mounted_stance, None)
            {
                return false;
            }
            let launch_candidate = interact_pressed
                && horse_snapshot.grounded
                && planar_speed_squared_mmps(horse_velocity_mmps)
                    >= u128::from(MIN_DIVE_SPEED_MMPS).pow(2);
            if launch_candidate
                && (!weapon
                    .bind()
                    .can_begin_authoritative_dive(tick, weapon_id)
                    || !horse.bind().can_start_authoritative_runout())
            {
                return false;
            }
        }
        let input = SaddleDiveTickInput {
            tick,
            interact_pressed,
            chosen_direction: quantized_direction(chosen_direction),
            horse_grounded: horse_snapshot.grounded,
            horse_position,
            horse_velocity_mmps,
            horse_gait: combat_gait(horse_snapshot.gait),
            equipped_weapon: weapon_id,
            rider_position,
            horse_retrievable: horse_snapshot.retrievable,
            authority_epoch,
            actor,
        };
        let Ok(output) = self.kernel.begin_tick(input) else {
            return false;
        };
        self.current_tick = i64::try_from(tick_value).unwrap_or(i64::MAX);
        self.motion_begun = true;
        self.apply_tick_motion(&output);
        let consumed = output.interact_consumed;
        self.process_effects(output.effects);
        self.refresh_runtime_properties();
        consumed
    }

    /// Move the detached rider once and feed post-`move_and_slide` collision
    /// evidence back to the deterministic kernel.
    #[func]
    pub fn resolve_motion(&mut self, tick: i64) -> bool {
        let Ok(tick_value) = u64::try_from(tick) else {
            return false;
        };
        let tick = SimulationTick::new(tick_value);
        if !self.motion_begun || self.kernel.current_tick() != Some(tick) {
            return false;
        }
        self.motion_begun = false;

        let descending;
        let (landing_normal, terrain);
        if self.kernel.state() == SaddleDiveState::Mounted {
            self.follow_saddle();
            if let Some(horse) = self.horse() {
                self.base_mut()
                    .set_velocity(horse.bind().m2_snapshot().velocity);
            }
            descending = false;
            landing_normal = None;
            terrain = LandingTerrain::Unknown;
        } else {
            let mut velocity = self.base().get_velocity();
            if self.kernel.state() != SaddleDiveState::SaddleDiveAirborne
                && !self.base().is_on_floor()
            {
                velocity.y -= SADDLE_DIVE_GRAVITY_MMPS2 as f32
                    / 1_000.0
                    / SADDLE_DIVE_TICK_RATE_HZ as f32;
                self.base_mut().set_velocity(velocity);
            }
            descending = velocity.y <= 0.0;
            self.base_mut().move_and_slide();
            let contact = self.best_landing_contact();
            landing_normal = contact.map(|value| value.0);
            terrain = contact.map_or(LandingTerrain::Unknown, |value| value.1);
        }

        let Some(rider_position) = quantized_origin(self.base().get_global_position()) else {
            return false;
        };
        let observation = RiderMotionObservation {
            tick,
            rider_position,
            rider_velocity_mmps: quantized_velocity(self.base().get_velocity()),
            descending,
            landing_normal,
            landing_terrain: terrain,
        };
        let Ok(effects) = self.kernel.resolve_motion(observation) else {
            return false;
        };
        self.process_effects(effects);
        if matches!(
            self.kernel.state(),
            SaddleDiveState::LandingProne | SaddleDiveState::LandingRecovery
        ) && self.base().is_on_floor()
        {
            let mut velocity = self.base().get_velocity();
            velocity.y = 0.0;
            self.base_mut().set_velocity(velocity);
        }
        self.refresh_runtime_properties();
        true
    }

    /// Apply one authority-issued damage observation. The stable sequence is
    /// part of the replay key; re-delivery mutates neither health nor telemetry.
    #[func]
    pub fn apply_external_damage(
        &mut self,
        tick: i64,
        observation_sequence: i64,
        amount: i64,
    ) -> bool {
        let (Ok(tick_value), Ok(sequence), Ok(amount)) = (
            u64::try_from(tick),
            u64::try_from(observation_sequence),
            u16::try_from(amount),
        ) else {
            return false;
        };
        let health_before = self.kernel.rider_health();
        let effects = self.kernel.apply_external_damage(DamageObservation {
            id: DamageObservationId {
                authority_epoch: self.kernel.authority_epoch(),
                actor: self.kernel.actor(),
                tick: SimulationTick::new(tick_value),
                sequence,
            },
            amount,
        });
        let changed = self.kernel.rider_health() != health_before;
        self.process_effects(effects);
        if health_before > 0 && self.kernel.rider_health() == 0 {
            self.emit_rider_death_once(SimulationTick::new(tick_value));
        }
        self.refresh_runtime_properties();
        changed
    }

    /// Close observation windows only after the authority adapter has processed
    /// every original observation through this tick.
    #[func]
    pub fn settle_observations_through(&mut self, tick: i64) -> bool {
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return false;
        };
        let effects = self.kernel.settle_observations_through(tick);
        self.process_effects(effects);
        self.refresh_runtime_properties();
        true
    }

    #[func]
    pub fn observe_death(&mut self, tick: i64) -> bool {
        let Ok(tick_value) = u64::try_from(tick) else {
            return false;
        };
        let was_alive = self.kernel.rider_health() > 0;
        let effects = self.kernel.observe_death(SimulationTick::new(tick_value));
        self.process_effects(effects);
        if was_alive {
            self.emit_rider_death_once(SimulationTick::new(tick_value));
        }
        self.refresh_runtime_properties();
        was_alive
    }

    #[func]
    pub fn end_match(&mut self, tick: i64) {
        let tick = u64::try_from(tick).map_or(SimulationTick::new(0), SimulationTick::new);
        let effects = self.kernel.end_match(tick);
        self.process_effects(effects);
    }

    /// Explicit course reset; this censors an open row and may reposition both
    /// entities. It is not a normal retrieval shortcut.
    #[func]
    pub fn reset_rider(&mut self, tick: i64) -> bool {
        let Ok(tick_value) = u64::try_from(tick) else {
            return false;
        };
        let effects = self.kernel.reset(SimulationTick::new(tick_value));
        self.current_tick = i64::try_from(tick_value).unwrap_or(i64::MAX);
        self.motion_begun = false;
        if let Some(mut horse) = self.horse() {
            horse.bind_mut().reset_horse();
        }
        if let Some(mut weapon) = self.weapon_controller() {
            if !weapon.bind_mut().complete_remount(tick) {
                godot_error!("course reset could not restore mounted combat state");
                return false;
            }
        }
        self.follow_saddle();
        self.set_collision_enabled(false);
        self.base_mut().set_velocity(Vector3::ZERO);
        self.death_signal_emitted = false;
        self.process_effects(effects);
        self.refresh_runtime_properties();
        true
    }

    #[func]
    pub fn get_snapshot_state(&self) -> VarDictionary {
        let mut result = VarDictionary::new();
        result.set("tick", self.current_tick);
        result.set("position", self.base().get_global_position());
        result.set("velocity", self.base().get_velocity());
        result.set(
            "yaw_degrees",
            f64::from(self.base().get_global_rotation().y.to_degrees()),
        );
        result.set("stance_id", self.stance_id);
        result.set("stance_known", self.stance_known);
        result
    }
}

#[godot_api]
impl ICharacterBody3D for SaddleDiveController {
    fn init(base: Base<CharacterBody3D>) -> Self {
        let actor = default_actor();
        Self {
            base,
            horse_path: NodePath::from("../Horse"),
            saddle_path: NodePath::from("../Horse/SaddleProxy"),
            collision_shape_path: NodePath::from("CollisionShape3D"),
            weapon_controller_path: NodePath::from("WeaponController"),
            actor_id: GString::from(DEFAULT_ACTOR_ID),
            authority_epoch: 0,
            on_foot_speed_mps: 6.0,
            current_tick: 0,
            stance_id: i64::from(RiderStance::MOUNTED_ID),
            stance_known: true,
            dive_id: -1,
            movement_scale: 1.0,
            airtime_seconds: 0.0,
            rider_health: 100,
            can_fire: true,
            can_reload: true,
            kernel: SaddleDiveKernel::new(SADDLE_DIVE_TICK_RATE_HZ, actor, 0)
                .expect("M2 tick rate is nonzero"),
            chosen_forward: Vector3::FORWARD,
            move_input: Vector2::ZERO,
            mounted_grounded: true,
            motion_begun: false,
            death_signal_emitted: false,
        }
    }

    fn ready(&mut self) {
        let actor_text = self.actor_id.to_string();
        let actor = PlayerId::parse(&actor_text).unwrap_or_else(|error| {
            godot_error!(
                "SaddleDiveController rejected actor_id '{}': {error}; using prototype ID",
                actor_text
            );
            self.actor_id = GString::from(DEFAULT_ACTOR_ID);
            default_actor()
        });
        let authority_epoch = u64::try_from(self.authority_epoch).unwrap_or(0);
        self.authority_epoch = i64::try_from(authority_epoch).unwrap_or(i64::MAX);
        self.kernel = SaddleDiveKernel::new(SADDLE_DIVE_TICK_RATE_HZ, actor, authority_epoch)
            .expect("M2 tick rate is nonzero");
        if let Some(mut weapon) = self.weapon_controller() {
            if !weapon
                .bind_mut()
                .bind_session_identity(actor, authority_epoch)
            {
                godot_error!("MountedWeaponController rejected initial rider identity binding");
            }
        }
        self.death_signal_emitted = false;
        self.base_mut().set_up_direction(Vector3::UP);
        self.base_mut().set_floor_max_angle(80.0_f32.to_radians());
        self.base_mut().set_floor_snap_length(0.12);
        self.base_mut().set_floor_block_on_wall_enabled(true);
        self.follow_saddle();
        self.set_collision_enabled(false);
        self.refresh_runtime_properties();
    }
}

impl SaddleDiveController {
    fn horse(&self) -> Option<Gd<HorseController>> {
        self.base()
            .try_get_node_as::<HorseController>(&self.horse_path)
    }

    fn weapon_controller(&self) -> Option<Gd<MountedWeaponController>> {
        self.base()
            .try_get_node_as::<MountedWeaponController>(&self.weapon_controller_path)
    }

    pub(crate) fn can_accept_authority_shot(
        &self,
        tick: SimulationTick,
        stance: RiderStance,
        dive_id: Option<DiveId>,
        weapon_id: WeaponId,
    ) -> bool {
        if self.kernel.current_tick() != Some(tick)
            || self.kernel.stance() != stance
            || self.kernel.current_dive_id() != dive_id
        {
            return false;
        }
        match (stance, dive_id) {
            (RiderStance::Mounted, None) => true,
            (RiderStance::SaddleDiveAirborne, Some(id)) => self
                .kernel
                .instrumentation_row(id)
                .is_some_and(|row| row.launch_weapon == weapon_id && row.landing_tick.is_none()),
            _ => false,
        }
    }

    pub(crate) fn record_authority_shot_attempt(&mut self, tick: SimulationTick) {
        let effects = self.kernel.record_shot_attempt(tick);
        self.process_effects(effects);
        self.refresh_runtime_properties();
    }

    pub(crate) fn record_authority_accepted_shot(
        &mut self,
        shot: AcceptedShotMetadata,
    ) -> bool {
        if shot.shooter != self.kernel.actor()
            || !self.can_accept_authority_shot(
                shot.tick,
                shot.stance,
                shot.dive_id,
                shot.weapon_id,
            )
        {
            return false;
        }
        if shot.dive_id.is_none() {
            return shot.stance == RiderStance::Mounted;
        }
        let effects = self.kernel.record_accepted_shot(shot);
        let accepted = !effects.telemetry_updates.is_empty();
        self.process_effects(effects);
        self.refresh_runtime_properties();
        accepted
    }

    pub(crate) fn record_attributed_authority_result(
        &mut self,
        attribution: &ShotResultAttribution,
    ) -> bool {
        let Some(shot) = attribution.accepted_shot else {
            return false;
        };
        if attribution.duplicate || shot.shooter != self.kernel.actor() {
            return false;
        }
        let effects = self.kernel.record_authority_result(attribution);
        let accepted = shot.dive_id.is_none()
            || !effects.telemetry_updates.is_empty()
            || !effects.events.is_empty();
        self.process_effects(effects);
        self.refresh_runtime_properties();
        accepted
    }

    pub(crate) fn bind_session_identity(
        &mut self,
        actor: PlayerId,
        authority_epoch: u64,
    ) -> bool {
        if self.kernel.actor() == actor && self.kernel.authority_epoch() == authority_epoch {
            return true;
        }
        if self.kernel.state() != SaddleDiveState::Mounted
            || self.kernel.instrumentation_rows().next().is_some()
        {
            return false;
        }
        let Some(mut weapon) = self.weapon_controller() else {
            return false;
        };
        if !weapon
            .bind_mut()
            .bind_session_identity(actor, authority_epoch)
        {
            return false;
        }
        let Ok(kernel) = SaddleDiveKernel::new(SADDLE_DIVE_TICK_RATE_HZ, actor, authority_epoch)
        else {
            return false;
        };
        self.kernel = kernel;
        self.actor_id = GString::from(&actor.to_canonical_string());
        self.authority_epoch = i64::try_from(authority_epoch).unwrap_or(i64::MAX);
        self.motion_begun = false;
        self.death_signal_emitted = false;
        self.refresh_runtime_properties();
        true
    }

    fn emit_rider_death_once(&mut self, tick: SimulationTick) {
        if self.death_signal_emitted {
            return;
        }
        self.death_signal_emitted = true;
        self.signals().rider_died().emit(tick_i64(tick));
    }

    fn follow_saddle(&mut self) {
        let transform = self
            .base()
            .try_get_node_as::<Node3D>(&self.saddle_path)
            .map(|saddle| saddle.get_global_transform());
        if let Some(transform) = transform {
            self.base_mut().set_global_transform(transform);
        }
    }

    fn set_collision_enabled(&mut self, enabled: bool) {
        if let Some(mut shape) = self
            .base()
            .try_get_node_as::<CollisionShape3D>(&self.collision_shape_path)
        {
            shape.set_disabled(!enabled);
        }
    }

    fn apply_tick_motion(&mut self, output: &spurfire_protocol::SaddleDiveTickOutput) {
        if let Some(velocity) = output.requested_rider_velocity_mmps {
            self.base_mut().set_velocity(mmps_to_vector(velocity));
            return;
        }
        match output.state {
            SaddleDiveState::Mounted => {
                self.follow_saddle();
                if let Some(horse) = self.horse() {
                    self.base_mut()
                        .set_velocity(horse.bind().m2_snapshot().velocity);
                }
            }
            SaddleDiveState::LandingProne
            | SaddleDiveState::LandingRecovery
            | SaddleDiveState::OnFootReady => {
                let scale = f32::from(output.movement_input_scale_milli) / 1_000.0;
                let forward = self.chosen_forward;
                let right = Vector3::new(-forward.z, 0.0, forward.x);
                let desired = (right * self.move_input.x + forward * self.move_input.y)
                    .limit_length(Some(1.0))
                    * finite_positive_or(self.on_foot_speed_mps, 6.0) as f32
                    * scale;
                let mut velocity = self.base().get_velocity();
                velocity.x = desired.x;
                velocity.z = desired.z;
                self.base_mut().set_velocity(velocity);
            }
            SaddleDiveState::SaddleDiveAirborne => {}
        }
    }

    fn process_effects(&mut self, effects: SaddleDiveEffects) {
        if !self.prepare_authoritative_combat(&effects) {
            godot_error!("Saddle Dive authority adapters diverged; refusing presentation effects");
            return;
        }

        for command in &effects.commands {
            match *command {
                SaddleDiveCommand::StartHorseRunout { tick, .. } => {
                    let started = self
                        .horse()
                        .is_some_and(|mut horse| horse.bind_mut().start_dive_runout(tick_i64(tick)));
                    if !started {
                        godot_error!("preflighted horse runout failed at tick {}", tick.as_u64());
                        return;
                    }
                }
                SaddleDiveCommand::DetachRider {
                    dive_id,
                    launch_velocity_mmps,
                    tick,
                } => {
                    self.set_collision_enabled(true);
                    self.base_mut()
                        .set_velocity(mmps_to_vector(launch_velocity_mmps));
                    if let Some(id) = dive_id {
                        if let Some(row) = self.kernel.instrumentation_row(id).cloned() {
                            self.signals().dive_started().emit(
                                dive_id_i64(id),
                                mmps_to_vector(launch_velocity_mmps),
                                f64::from(row.clamped_angle_millidegrees) / 1_000.0,
                            );
                        }
                    } else if let Some(mut horse) = self.horse() {
                        if !horse.bind_mut().stop_for_dismount(tick_i64(tick)) {
                            godot_error!("ordinary dismount could not stop the existing horse");
                            return;
                        }
                    }
                }
                SaddleDiveCommand::ApplyRiderDamage(command) => {
                    let health_after = i64::from(self.kernel.rider_health());
                    self.signals().landing_damage_applied().emit(
                        dive_id_i64(command.id.dive_id),
                        i64::from(command.amount),
                        health_after,
                    );
                    if command.amount != BAD_LANDING_DAMAGE {
                        godot_error!("unexpected Saddle Dive landing damage command");
                    }
                    if health_after == 0 {
                        self.emit_rider_death_once(command.tick);
                    }
                }
                SaddleDiveCommand::AttachRider { tick, .. } => {
                    let remounted = self
                        .horse()
                        .is_some_and(|mut horse| horse.bind_mut().complete_remount(tick_i64(tick)));
                    if !remounted {
                        godot_error!("range-checked remount could not restore horse control");
                        return;
                    }
                    self.follow_saddle();
                    self.set_collision_enabled(false);
                    self.base_mut().set_velocity(Vector3::ZERO);
                }
            }
        }

        for transition in &effects.transitions {
            let previous = stance_for_state(transition.from, self.mounted_grounded);
            let current = stance_for_state(transition.to, self.mounted_grounded);
            let dive_id = transition.dive_id.map_or(-1, dive_id_i64);
            self.signals().stance_changed().emit(
                i64::from(previous.as_u8()),
                i64::from(current.as_u8()),
                tick_i64(transition.tick),
                dive_id,
            );
            match (transition.from, transition.to) {
                (SaddleDiveState::SaddleDiveAirborne, SaddleDiveState::LandingProne) => {
                    if let Some(id) = transition.dive_id {
                        if let Some(row) = self.kernel.instrumentation_row(id).cloned() {
                            let bad = row.landing_outcome == Some(LandingOutcome::Bad);
                            let terrain = GString::from(
                                row.landing_terrain.map_or("unknown", landing_terrain_name),
                            );
                            self.signals().dive_landed().emit(
                                dive_id_i64(id),
                                bad,
                                f64::from(row.landing_slope_millidegrees.unwrap_or(0)) / 1_000.0,
                                &terrain,
                            );
                            let phase = GString::from("prone");
                            self.signals()
                                .recovery_changed()
                                .emit(dive_id_i64(id), &phase, 0.0);
                        }
                    }
                }
                (SaddleDiveState::LandingProne, SaddleDiveState::LandingRecovery) => {
                    if let Some(id) = transition.dive_id {
                        let phase = GString::from("half_speed");
                        self.signals()
                            .recovery_changed()
                            .emit(dive_id_i64(id), &phase, 0.5);
                    }
                }
                (SaddleDiveState::LandingRecovery, SaddleDiveState::OnFootReady) => {
                    if let Some(id) = transition.dive_id {
                        self.signals().recovery_completed().emit(dive_id_i64(id));
                    }
                }
                _ => {}
            }
        }

        for event in &effects.events {
            self.emit_gameplay_event(event);
        }
        for row in &effects.telemetry_updates {
            let payload = instrumentation_dictionary(row);
            self.signals().dive_telemetry_updated().emit(&payload);
        }
        for row in &effects.telemetry_finalized {
            let payload = instrumentation_dictionary(row);
            self.signals().dive_telemetry_finalized().emit(&payload);
        }
    }

    fn prepare_authoritative_combat(&mut self, effects: &SaddleDiveEffects) -> bool {
        for command in &effects.commands {
            match *command {
                SaddleDiveCommand::StartHorseRunout {
                    dive_id,
                    tick,
                    horse_velocity_mmps,
                    ..
                } => {
                    let Some(row) = self.kernel.instrumentation_row(dive_id) else {
                        return false;
                    };
                    let Some(mut weapon) = self.weapon_controller() else {
                        return false;
                    };
                    if !weapon.bind_mut().begin_saddle_dive(
                        dive_id_i64(dive_id),
                        tick_i64(tick),
                        Vector2::new(
                            horse_velocity_mmps[0] as f32 / 1_000.0,
                            horse_velocity_mmps[2] as f32 / 1_000.0,
                        ),
                        i64::try_from(row.nominal_airtime_ticks).unwrap_or(i64::MAX),
                    ) {
                        return false;
                    }
                }
                SaddleDiveCommand::AttachRider { tick, .. } => {
                    let Some(mut weapon) = self.weapon_controller() else {
                        return false;
                    };
                    if !weapon.bind_mut().complete_remount(tick_i64(tick)) {
                        return false;
                    }
                }
                SaddleDiveCommand::DetachRider { .. }
                | SaddleDiveCommand::ApplyRiderDamage(_) => {}
            }
        }

        for transition in &effects.transitions {
            let current = stance_for_state(transition.to, self.mounted_grounded);
            let current_dive = if current == RiderStance::SaddleDiveAirborne {
                transition.dive_id
            } else {
                None
            };
            let already_applied = matches!(
                (transition.from, transition.to),
                (SaddleDiveState::Mounted, SaddleDiveState::SaddleDiveAirborne)
                    | (SaddleDiveState::OnFootReady, SaddleDiveState::Mounted)
            );
            if already_applied {
                continue;
            }
            let Some(mut weapon) = self.weapon_controller() else {
                return false;
            };
            if matches!(
                (transition.from, transition.to),
                (SaddleDiveState::SaddleDiveAirborne, SaddleDiveState::LandingProne)
            ) {
                let Some(id) = transition.dive_id else {
                    return false;
                };
                if !weapon
                    .bind_mut()
                    .finish_saddle_dive(dive_id_i64(id), tick_i64(transition.tick))
                {
                    return false;
                }
            } else if !weapon
                .bind_mut()
                .synchronize_authoritative_stance(current, current_dive)
            {
                return false;
            }
        }
        true
    }

    fn emit_gameplay_event(&mut self, event: &GameplayEventRow) {
        let event_id_text = format!(
            "{}:{}:{}:{}:{}",
            event.id.authority_epoch,
            event.id.actor.to_canonical_string(),
            event.id.source_tick.as_u64(),
            gameplay_event_kind_name(event.kind),
            event.id.sequence
        );
        let event_id = GString::from(&event_id_text);
        let kind = GString::from(gameplay_event_kind_name(event.kind));
        let payload = gameplay_event_dictionary(event);
        godot_print!(
            "SPURFIRE_GAMEPLAY_EVENT id={} kind={} text={} tick={} actor={}",
            event_id,
            kind,
            event.text,
            event.tick.as_u64(),
            event.actor.to_canonical_string()
        );
        self.signals()
            .gameplay_event()
            .emit(&event_id, &kind, &payload);
    }

    fn best_landing_contact(&self) -> Option<(QuantizedDirection, LandingTerrain)> {
        let mut best: Option<(QuantizedDirection, LandingTerrain)> = None;
        for index in 0..self.base().get_slide_collision_count() {
            let Some(collision) = self.base().get_slide_collision(index) else {
                continue;
            };
            let normal = collision.get_normal();
            if !normal.is_finite() || normal.y <= 0.0 {
                continue;
            }
            let Some(quantized) = quantized_direction(normal.normalized()) else {
                continue;
            };
            let terrain = collision
                .get_collider()
                .map_or(LandingTerrain::Unknown, classify_landing_terrain);
            let replace = best.is_none_or(|(current, _)| {
                (quantized.y, quantized.x, quantized.z) > (current.y, current.x, current.z)
            });
            if replace {
                best = Some((quantized, terrain));
            }
        }
        if best.is_none() && self.base().is_on_floor() {
            quantized_direction(self.base().get_floor_normal().normalized())
                .map(|normal| (normal, LandingTerrain::Unknown))
        } else {
            best
        }
    }

    fn refresh_runtime_properties(&mut self) {
        let stance = self.kernel.stance();
        self.stance_id = i64::from(stance.as_u8());
        self.stance_known = stance.is_known();
        self.dive_id = self.kernel.current_dive_id().map_or(-1, dive_id_i64);
        let movement_milli = match self.kernel.state() {
            SaddleDiveState::LandingProne => MOVEMENT_SCALE_PRONE_MILLI,
            SaddleDiveState::LandingRecovery => MOVEMENT_SCALE_RECOVERY_MILLI,
            _ => MOVEMENT_SCALE_FULL_MILLI,
        };
        self.movement_scale = f64::from(movement_milli) / 1_000.0;
        self.can_fire = matches!(
            stance,
            RiderStance::Mounted | RiderStance::SaddleDiveAirborne
        );
        self.can_reload = stance == RiderStance::Mounted;
        self.rider_health = i64::from(self.kernel.rider_health());
        self.airtime_seconds = self.current_airtime_seconds();
        self.emit_camera_telemetry(stance);
    }

    fn emit_camera_telemetry(&mut self, stance: RiderStance) {
        let Some(horse) = self.horse() else {
            return;
        };
        let snapshot = horse.bind().m2_snapshot();
        let planar_speed = f64::from(
            (self.base().get_velocity().x * self.base().get_velocity().x
                + self.base().get_velocity().z * self.base().get_velocity().z)
                .sqrt(),
        );
        let (speed_mps, speed_fraction, yaw_rate_degrees) = if stance.is_mounted() {
            (
                f64::from(snapshot.velocity.length()),
                snapshot.speed_fraction,
                snapshot.yaw_rate_degrees,
            )
        } else {
            let top_speed = finite_positive_or(snapshot.gallop_speed_mps, 13.0);
            (planar_speed, (planar_speed / top_speed).clamp(0.0, 1.0), 0.0)
        };
        let mut telemetry = VarDictionary::new();
        telemetry.set("speed_mps", speed_mps);
        telemetry.set("speed_fraction", speed_fraction.clamp(0.0, 1.0));
        telemetry.set("yaw_rate_degs", yaw_rate_degrees);
        telemetry.set("stance_id", i64::from(stance.as_u8()));
        self.signals().telemetry_updated().emit(&telemetry);
    }

    fn current_airtime_seconds(&self) -> f64 {
        let Some(id) = self.kernel.current_dive_id() else {
            return 0.0;
        };
        let Some(row) = self.kernel.instrumentation_row(id) else {
            return 0.0;
        };
        let ticks = row.airtime_ticks.or_else(|| {
            self.kernel
                .current_tick()
                .and_then(|tick| tick.checked_duration_since(row.launch_tick))
        });
        ticks.map_or(0.0, |value| {
            value as f64 / f64::from(SADDLE_DIVE_TICK_RATE_HZ)
        })
    }
}

fn default_actor() -> PlayerId {
    PlayerId::parse(DEFAULT_ACTOR_ID).expect("prototype actor UUID is valid")
}

fn combat_gait(value: i64) -> CombatGait {
    match value {
        1 => CombatGait::Walk,
        2 => CombatGait::Trot,
        3 => CombatGait::Gallop,
        4 => CombatGait::Canter,
        _ => CombatGait::Idle,
    }
}

fn stance_for_state(state: SaddleDiveState, mounted_grounded: bool) -> RiderStance {
    match state {
        SaddleDiveState::Mounted if mounted_grounded => RiderStance::Mounted,
        SaddleDiveState::Mounted => RiderStance::MountedAirborne,
        SaddleDiveState::SaddleDiveAirborne => RiderStance::SaddleDiveAirborne,
        SaddleDiveState::LandingProne => RiderStance::LandingProne,
        SaddleDiveState::LandingRecovery => RiderStance::LandingRecovery,
        SaddleDiveState::OnFootReady => RiderStance::OnFootStanding,
    }
}

fn finite_move_input(value: Vector2) -> Vector2 {
    if value.is_finite() {
        value.limit_length(Some(1.0))
    } else {
        Vector2::ZERO
    }
}

fn finite_planar_direction(value: Vector3) -> Option<Vector3> {
    if !value.is_finite() {
        return None;
    }
    let planar = Vector3::new(value.x, 0.0, value.z);
    (planar.length_squared() > 1.0e-8).then(|| planar.normalized())
}

fn horse_forward(yaw: f32) -> Vector3 {
    Vector3::new(-yaw.sin(), 0.0, -yaw.cos())
}

fn quantized_direction(value: Vector3) -> Option<QuantizedDirection> {
    let normalized = finite_planar_or_full(value)?;
    QuantizedDirection::from_components(
        f64::from(normalized.x),
        f64::from(normalized.y),
        f64::from(normalized.z),
    )
    .ok()
}

fn finite_planar_or_full(value: Vector3) -> Option<Vector3> {
    if !value.is_finite() || value.length_squared() <= 1.0e-8 {
        None
    } else {
        Some(value.normalized())
    }
}

fn quantized_origin(value: Vector3) -> Option<QuantizedOrigin> {
    QuantizedOrigin::from_meters(f64::from(value.x), f64::from(value.y), f64::from(value.z)).ok()
}

fn planar_speed_squared_mmps(value: [i32; 3]) -> u128 {
    let x = i128::from(value[0]);
    let z = i128::from(value[2]);
    (x * x + z * z) as u128
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

fn mmps_to_vector(value: [i32; 3]) -> Vector3 {
    Vector3::new(
        value[0] as f32 / 1_000.0,
        value[1] as f32 / 1_000.0,
        value[2] as f32 / 1_000.0,
    )
}

fn finite_positive_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn classify_landing_terrain(collider: Gd<Object>) -> LandingTerrain {
    for (key, terrain) in [
        ("mud", LandingTerrain::Mud),
        ("riverbed", LandingTerrain::Riverbed),
        ("scrub", LandingTerrain::Scrub),
    ] {
        if collider.has_meta(key) && collider.get_meta(key).try_to::<bool>().unwrap_or(true) {
            return terrain;
        }
    }
    for key in ["rough", "rough_terrain"] {
        if collider.has_meta(key) && collider.get_meta(key).try_to::<bool>().unwrap_or(true) {
            return LandingTerrain::Scrub;
        }
    }
    let Ok(node) = collider.try_cast::<Node>() else {
        return LandingTerrain::Flat;
    };
    if node.is_in_group("mud") {
        LandingTerrain::Mud
    } else if node.is_in_group("riverbed") {
        LandingTerrain::Riverbed
    } else if node.is_in_group("scrub")
        || node.is_in_group("rough")
        || node.is_in_group("rough_terrain")
    {
        LandingTerrain::Scrub
    } else {
        LandingTerrain::Flat
    }
}

fn instrumentation_dictionary(row: &DiveInstrumentationRow) -> VarDictionary {
    let mut result = VarDictionary::new();
    result.set("schema_version", i64::from(row.schema_version));
    result.set(
        "authority_epoch",
        i64::try_from(row.authority_epoch).unwrap_or(i64::MAX),
    );
    result.set("actor", row.actor.to_canonical_string());
    result.set("dive_id", dive_id_i64(row.dive_id));
    result.set("launch_tick", tick_i64(row.launch_tick));
    result.set("launch_weapon", i64::from(row.launch_weapon.as_u8()));
    result.set("launch_gait", row.launch_gait.as_str());
    result.set(
        "prelaunch_velocity_mmps",
        Vector2i::new(
            row.prelaunch_velocity_mmps[0],
            row.prelaunch_velocity_mmps[1],
        ),
    );
    result.set(
        "prelaunch_velocity_mps",
        Vector2::new(
            row.prelaunch_velocity_mmps[0] as f32 / 1_000.0,
            row.prelaunch_velocity_mmps[1] as f32 / 1_000.0,
        ),
    );
    result.set("prelaunch_speed_mmps", i64::from(row.prelaunch_speed_mmps));
    result.set(
        "prelaunch_speed_mps",
        f64::from(row.prelaunch_speed_mmps) / 1_000.0,
    );
    result.set(
        "requested_direction",
        direction_to_vector(row.requested_direction),
    );
    result.set(
        "requested_direction_quantized",
        Vector3i::new(
            row.requested_direction.x,
            row.requested_direction.y,
            row.requested_direction.z,
        ),
    );
    result.set(
        "requested_angle_millidegrees",
        i64::from(row.requested_angle_millidegrees),
    );
    result.set(
        "requested_angle_degrees",
        f64::from(row.requested_angle_millidegrees) / 1_000.0,
    );
    result.set(
        "clamped_direction",
        direction_to_vector(row.clamped_direction),
    );
    result.set(
        "clamped_direction_quantized",
        Vector3i::new(
            row.clamped_direction.x,
            row.clamped_direction.y,
            row.clamped_direction.z,
        ),
    );
    result.set(
        "clamped_angle_millidegrees",
        i64::from(row.clamped_angle_millidegrees),
    );
    result.set(
        "clamped_angle_degrees",
        f64::from(row.clamped_angle_millidegrees) / 1_000.0,
    );
    result.set("direction_was_clamped", row.direction_was_clamped);
    result.set(
        "horizontal_impulse_mmps",
        i64::from(row.horizontal_impulse_mmps),
    );
    result.set(
        "horizontal_impulse_mps",
        f64::from(row.horizontal_impulse_mmps) / 1_000.0,
    );
    result.set(
        "resulting_planar_speed_mmps",
        i64::from(row.resulting_planar_speed_mmps),
    );
    result.set(
        "resulting_planar_speed_mps",
        f64::from(row.resulting_planar_speed_mmps) / 1_000.0,
    );
    result.set(
        "resulting_total_speed_mmps",
        i64::from(row.resulting_total_speed_mmps),
    );
    result.set(
        "resulting_total_speed_mps",
        f64::from(row.resulting_total_speed_mmps) / 1_000.0,
    );
    result.set("vertical_pop_mmps", i64::from(row.vertical_pop_mmps));
    result.set(
        "vertical_pop_mps",
        f64::from(row.vertical_pop_mmps) / 1_000.0,
    );
    result.set("launch_height_mm", i64::from(row.launch_height_mm));
    result.set("launch_height_m", f64::from(row.launch_height_mm) / 1_000.0);
    result.set(
        "nominal_airtime_ticks",
        i64::try_from(row.nominal_airtime_ticks).unwrap_or(i64::MAX),
    );
    set_optional_tick(&mut result, "landing_tick", row.landing_tick);
    set_optional_u64(&mut result, "airtime_ticks", row.airtime_ticks);
    result.set("shot_attempts", i64::from(row.shot_attempts));
    result.set("shots_fired", i64::from(row.shots_fired));
    result.set("shots_hit", i64::from(row.shots_hit));
    result.set("headshots", i64::from(row.headshots));
    result.set("reversal_hits", i64::from(row.reversal_hits));
    result.set("damage_dealt", i64::from(row.damage_dealt));
    result.set(
        "landing_terrain",
        row.landing_terrain.map_or("", landing_terrain_name),
    );
    if let Some(value) = row.landing_slope_millidegrees {
        result.set("landing_slope_millidegrees", i64::from(value));
    } else {
        result.set("landing_slope_millidegrees", &Variant::nil());
    }
    result.set(
        "landing_slope_degrees",
        row.landing_slope_millidegrees
            .map_or(-1.0, |value| f64::from(value) / 1_000.0),
    );
    result.set(
        "landing_outcome",
        row.landing_outcome.map_or("", landing_outcome_name),
    );
    result.set("landing_damage", i64::from(row.landing_damage));
    result.set(
        "damage_taken_landing_through_3s",
        i64::from(row.damage_taken_landing_through_3s),
    );
    set_optional_tick(&mut result, "death_tick", row.death_tick);
    set_optional_bool(&mut result, "death_within_3s", row.death_within_3s);
    set_optional_tick(&mut result, "remount_tick", row.remount_tick);
    set_optional_u64(
        &mut result,
        "time_to_remount_ticks",
        row.time_to_remount_ticks,
    );
    result.set(
        "censor_reason",
        row.censor_reason.map_or("", censor_reason_name),
    );
    result
}

fn gameplay_event_dictionary(event: &GameplayEventRow) -> VarDictionary {
    let mut result = VarDictionary::new();
    result.set("text", event.text);
    result.set("kind", gameplay_event_kind_name(event.kind));
    result.set("tick", tick_i64(event.tick));
    result.set("actor", event.actor.to_canonical_string());
    result.set("dive_id", event.dive_id.map_or(-1, dive_id_i64));
    result.set(
        "weapon_id",
        event
            .weapon_id
            .map_or(-1, |weapon| i64::from(weapon.as_u8())),
    );
    result.set(
        "target_id",
        event
            .target_id
            .and_then(|target| i64::try_from(target.0).ok())
            .unwrap_or(-1),
    );
    result.set("hit_zone", event.hit_zone.map_or("", HitZone::as_str));
    result.set("damage", i64::from(event.damage));
    result
}

fn direction_to_vector(direction: QuantizedDirection) -> Vector3 {
    Vector3::new(
        direction.x as f32 / DIRECTION_UNITS as f32,
        direction.y as f32 / DIRECTION_UNITS as f32,
        direction.z as f32 / DIRECTION_UNITS as f32,
    )
}

fn gameplay_event_kind_name(kind: GameplayEventKind) -> &'static str {
    match kind {
        GameplayEventKind::FlyingDismount => "flying_dismount",
        GameplayEventKind::SaddleDiveHeadshot => "saddle_dive_headshot",
        GameplayEventKind::FullGallopHit => "full_gallop_hit",
        GameplayEventKind::AirborneReversal => "airborne_reversal",
    }
}

fn landing_terrain_name(terrain: LandingTerrain) -> &'static str {
    match terrain {
        LandingTerrain::Flat => "flat",
        LandingTerrain::Scrub => "scrub",
        LandingTerrain::Mud => "mud",
        LandingTerrain::Riverbed => "riverbed",
        LandingTerrain::Unknown => "unknown",
    }
}

fn landing_outcome_name(outcome: LandingOutcome) -> &'static str {
    match outcome {
        LandingOutcome::Good => "good",
        LandingOutcome::Bad => "bad",
    }
}

fn censor_reason_name(reason: DiveCensorReason) -> &'static str {
    match reason {
        DiveCensorReason::DiedAirborne => "died_airborne",
        DiveCensorReason::DiedBeforeRemount => "died_before_remount",
        DiveCensorReason::MatchEnded => "match_ended",
        DiveCensorReason::Reset => "reset",
        DiveCensorReason::NotObserved => "not_observed",
    }
}

fn set_optional_tick(result: &mut VarDictionary, key: &str, value: Option<SimulationTick>) {
    if let Some(value) = value {
        result.set(key, tick_i64(value));
    } else {
        result.set(key, &Variant::nil());
    }
}

fn set_optional_u64(result: &mut VarDictionary, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        result.set(key, i64::try_from(value).unwrap_or(i64::MAX));
    } else {
        result.set(key, &Variant::nil());
    }
}

fn set_optional_bool(result: &mut VarDictionary, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        result.set(key, value);
    } else {
        result.set(key, &Variant::nil());
    }
}

fn tick_i64(tick: SimulationTick) -> i64 {
    i64::try_from(tick.as_u64()).unwrap_or(i64::MAX)
}

fn dive_id_i64(dive_id: DiveId) -> i64 {
    i64::try_from(dive_id.get()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locomotion::TerrainSurface;

    #[test]
    fn gameplay_event_names_match_presentation_contract() {
        assert_eq!(GameplayEventKind::FlyingDismount.text(), "FLYING DISMOUNT");
        assert_eq!(
            GameplayEventKind::SaddleDiveHeadshot.text(),
            "SADDLE DIVE HEADSHOT"
        );
        assert_eq!(GameplayEventKind::FullGallopHit.text(), "FULL-GALLOP HIT");
        assert_eq!(
            GameplayEventKind::AirborneReversal.text(),
            "AIRBORNE REVERSAL"
        );
    }

    #[test]
    fn invalid_vectors_are_conservative() {
        assert!(quantized_direction(Vector3::ZERO).is_none());
        assert!(quantized_direction(Vector3::new(f32::NAN, 0.0, 0.0)).is_none());
        assert_eq!(
            finite_move_input(Vector2::new(f32::NAN, 0.0)),
            Vector2::ZERO
        );
    }

    #[test]
    fn stance_mapping_is_stable() {
        assert_eq!(
            stance_for_state(SaddleDiveState::Mounted, true).as_u8(),
            RiderStance::MOUNTED_ID
        );
        assert_eq!(
            stance_for_state(SaddleDiveState::Mounted, false).as_u8(),
            RiderStance::MOUNTED_AIRBORNE_ID
        );
        assert_eq!(
            stance_for_state(SaddleDiveState::SaddleDiveAirborne, false).as_u8(),
            RiderStance::SADDLE_DIVE_AIRBORNE_ID
        );
    }

    #[test]
    fn terrain_bridge_covers_authored_values() {
        let rows = [
            (TerrainSurface::Flat, LandingTerrain::Flat),
            (TerrainSurface::Scrub, LandingTerrain::Scrub),
            (TerrainSurface::Mud, LandingTerrain::Mud),
            (TerrainSurface::Riverbed, LandingTerrain::Riverbed),
        ];
        for (source, expected) in rows {
            let actual = match source {
                TerrainSurface::Flat => LandingTerrain::Flat,
                TerrainSurface::Scrub => LandingTerrain::Scrub,
                TerrainSurface::Mud => LandingTerrain::Mud,
                TerrainSurface::Riverbed => LandingTerrain::Riverbed,
            };
            assert_eq!(actual, expected);
        }
    }
}
