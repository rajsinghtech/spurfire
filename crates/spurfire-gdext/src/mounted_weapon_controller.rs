//! Thin Godot `Node3D` adapter for the engine-independent combat kernel.

use godot::classes::{INode3D, Node3D};
use godot::prelude::*;
use spurfire_protocol::{
    clamp_launch_direction, effective_spread_millidegrees, AcceptedShotMetadata, CombatGait,
    CombatKernel, DiveContextError, DiveFireRejection, DiveId, EntityId, FireRejection, HitZone,
    PlayerId, QuantizedDirection, QuantizedOrigin, ReloadStartError, RiderStance, RidingState,
    ShotAttributionLedger, ShotCommand, ShotOutcome, ShotRejectionReason, ShotResult,
    ShotTelemetry, SimulationTick, WeaponAmmo, WeaponId, WeaponStats, DIRECTION_UNITS,
    ORIGIN_LEASH_MM,
};

use crate::saddle_dive_controller::SaddleDiveController;

const DEFAULT_TICK_RATE: u32 = 60;
const DEFAULT_SHOOTER_ID: &str = "00000000-0000-4000-8000-000000000001";
const MAX_HANDLING_SPEED_MPS: f64 = 200.0;
const MAX_HANDLING_YAW_RATE_DEGREES: f64 = 3_600.0;

#[derive(Clone, Copy)]
struct PendingLocalShot {
    tick: SimulationTick,
    weapon_id: WeaponId,
    direction: QuantizedDirection,
}

/// The single Godot-facing authority API for mounted rifles.
#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct MountedWeaponController {
    #[base]
    base: Base<Node3D>,

    // Match configuration is read when the node becomes ready.
    #[export]
    tick_rate: i64,
    #[export]
    lobby_seed: i64,
    #[export]
    shooter_peer_id: GString,
    #[export]
    rider_path: NodePath,

    // Rewound handling inputs supplied atomically by the shared scene tick.
    #[var(no_set)]
    stance_id: i64,
    #[var(no_set)]
    stance_known: bool,
    #[var(no_set)]
    dive_id: i64,
    gait: i64,
    speed_mps: f64,
    gait_top_speed_mps: f64,
    yaw_rate_degrees: f64,
    stumbling: bool,
    ads: bool,
    sprint_gallop: bool,

    // Read-only HUD/effect state; no seed or credential is exposed.
    #[var(no_set)]
    current_tick: i64,
    #[var(no_set)]
    weapon_id: i64,
    #[var(no_set)]
    ammo_mag: i64,
    #[var(no_set)]
    ammo_reserve: i64,
    #[var(no_set)]
    is_reloading: bool,
    #[var(no_set)]
    reload_progress: f64,
    #[var(no_set)]
    recoil_pitch_degrees: f64,
    #[var(no_set)]
    recoil_yaw_degrees: f64,
    #[var(no_set)]
    last_shot_origin: Vector3,
    #[var(no_set)]
    last_shot_direction: Vector3,
    #[var(no_set)]
    last_reject_reason: GString,
    #[var(no_set)]
    last_telemetry: VarDictionary,

    kernel: CombatKernel,
    riding: RidingState,
    context_tick: Option<SimulationTick>,
    authority_epoch: u64,
    shot_ledger: ShotAttributionLedger,
    pending_local_shot: Option<PendingLocalShot>,
    pending_wire_command: Option<ShotCommand>,
}

#[godot_api]
impl MountedWeaponController {
    /// Emitted after a successful equipment change.
    #[signal]
    fn weapon_changed(weapon_id: i64);

    /// Emitted whenever magazine or reserve state changes.
    #[signal]
    fn ammo_changed(mag: i64, reserve: i64);

    /// Cosmetic shot/effect event. Geometry and authority results remain separate.
    #[signal]
    fn shot_fired(tick: i64, weapon_id: i64);

    /// Authority acceptance metadata used by M2 instrumentation.
    #[signal]
    fn shot_accepted(tick: i64, weapon_id: i64, accepted_shot_index: i64, dive_id: i64);

    /// Local-authority result evidence used by deterministic attribution.
    #[signal]
    fn shot_resolved(
        tick: i64,
        outcome: GString,
        hit_zone: GString,
        damage: i64,
        resolved_direction: Vector3,
    );

    /// Stable rejection reason; M2 internal cap reasons remain precise here.
    #[signal]
    fn fire_rejected(tick: i64, reason: GString);

    /// Authoritative reload lifecycle. Progress is native active-tick truth;
    /// presentation must not run a second wall-clock timer.
    #[signal]
    fn reload_started(tick: i64, required_ticks: i64);

    #[signal]
    fn reload_progressed(tick: i64, progress: f64, active_ticks: i64, required_ticks: i64);

    #[signal]
    fn reload_completed(tick: i64, mag: i64, reserve: i64);

    #[signal]
    fn reload_rejected(tick: i64, reason: GString);

    /// Emitted when an authority result confirms a target hit.
    #[signal]
    fn hit_confirmed(target_id: i64, hit_zone: GString, damage: i64);

    /// Install launch-time handling for one shared tick. Stance and DiveId are
    /// assertions only: authoritative transitions are applied by the Rust rider
    /// controller and cannot be minted by a Godot caller.
    #[func]
    #[allow(clippy::too_many_arguments)]
    pub fn set_rider_context(
        &mut self,
        tick: i64,
        stance_id: i64,
        dive_id: i64,
        gait: i64,
        speed_mps: f64,
        gait_top_speed_mps: f64,
        yaw_rate_degrees: f64,
        stumbling: bool,
        ads: bool,
        sprint_gallop: bool,
    ) -> bool {
        let (Ok(tick_value), Ok(stance_value)) = (u64::try_from(tick), u8::try_from(stance_id))
        else {
            return false;
        };
        let tick = SimulationTick::new(tick_value);
        if self.context_tick.is_some_and(|current| tick <= current) {
            return false;
        }
        let stance = RiderStance::from_u8(stance_value);
        if !stance.is_known() || !stance.is_canonical() {
            return false;
        }
        let parsed_dive = match parse_dive_for_stance(stance, dive_id) {
            Some(value) => value,
            None => return false,
        };
        if stance != self.riding.stance || parsed_dive != self.riding.dive_id {
            return false;
        }
        let (Some(gait), Some(planar_speed_mmps), Some(gait_top_speed_mmps), Some(yaw_rate)) = (
            gait_from_id(gait),
            meters_per_second_to_mmps(speed_mps),
            positive_meters_per_second_to_mmps(gait_top_speed_mps),
            degrees_to_millidegrees(yaw_rate_degrees),
        ) else {
            return false;
        };
        let riding = RidingState {
            stance,
            dive_id: parsed_dive,
            gait,
            planar_speed_mmps,
            gait_top_speed_mmps,
            yaw_rate_millidegrees_per_second: yaw_rate,
            stumbling,
            ads,
            sprint_gallop,
            majestic_charge: false,
        };
        if !riding.is_consistent() {
            return false;
        }
        self.riding = riding;
        self.context_tick = Some(tick);
        self.gait = gait_id(gait);
        self.speed_mps = speed_mps;
        self.gait_top_speed_mps = gait_top_speed_mps;
        self.yaw_rate_degrees = yaw_rate_degrees;
        self.stumbling = stumbling;
        self.ads = ads;
        self.sprint_gallop = sprint_gallop;
        true
    }

    /// Advance reload/recoil using the injected absolute gameplay clock.
    #[func]
    pub fn advance_to_tick(&mut self, tick: i64) -> bool {
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return false;
        };
        self.advance_kernel_to(tick)
    }

    /// Open the authority-owned dive fire context before same-tick fire/reload.
    /// Rust-only: Godot scripts cannot mint an airborne authority context.
    pub(crate) fn begin_saddle_dive(
        &mut self,
        dive_id: i64,
        launch_tick: i64,
        prelaunch_velocity_mps: Vector2,
        nominal_airtime_ticks: i64,
    ) -> bool {
        let (Ok(dive_value), Ok(tick_value), Ok(nominal_ticks)) = (
            u64::try_from(dive_id),
            u64::try_from(launch_tick),
            u64::try_from(nominal_airtime_ticks),
        ) else {
            return false;
        };
        let Some(dive_id) = DiveId::new(dive_value) else {
            return false;
        };
        let (Some(prelaunch_x), Some(prelaunch_z)) = (
            meters_per_second_to_signed_mmps(f64::from(prelaunch_velocity_mps.x)),
            meters_per_second_to_signed_mmps(f64::from(prelaunch_velocity_mps.y)),
        ) else {
            return false;
        };
        let prelaunch = [prelaunch_x, prelaunch_z];
        let tick = SimulationTick::new(tick_value);
        if self.context_tick != Some(tick)
            || self.riding.stance != RiderStance::Mounted
            || self.riding.dive_id.is_some()
        {
            self.last_reject_reason = GString::from("dive_context_mismatch");
            return false;
        }
        let weapon = self.kernel.equipped_weapon();
        match self
            .kernel
            .begin_saddle_dive(dive_id, tick, weapon, prelaunch, nominal_ticks)
        {
            Ok(()) => {
                self.riding.stance = RiderStance::SaddleDiveAirborne;
                self.riding.dive_id = Some(dive_id);
                self.assign_stance(RiderStance::SaddleDiveAirborne, Some(dive_id));
                self.current_tick = i64::try_from(tick_value).unwrap_or(i64::MAX);
                self.update_runtime_properties();
                true
            }
            Err(error) => {
                self.last_reject_reason = GString::from(dive_context_error_name(error));
                false
            }
        }
    }

    /// Close new dive fire at first post-move landing contact. Rust-only so a
    /// scene caller cannot shorten or reopen an authority-owned gate.
    pub(crate) fn finish_saddle_dive(&mut self, dive_id: i64, landing_tick: i64) -> bool {
        let (Ok(dive_value), Ok(tick_value)) =
            (u64::try_from(dive_id), u64::try_from(landing_tick))
        else {
            return false;
        };
        let Some(dive_id) = DiveId::new(dive_value) else {
            return false;
        };
        match self
            .kernel
            .finish_saddle_dive(dive_id, SimulationTick::new(tick_value))
        {
            Ok(()) => {
                self.riding.stance = RiderStance::LandingProne;
                self.riding.dive_id = None;
                self.assign_stance(RiderStance::LandingProne, None);
                self.current_tick = i64::try_from(tick_value).unwrap_or(i64::MAX);
                self.update_runtime_properties();
                true
            }
            Err(error) => {
                self.last_reject_reason = GString::from(dive_context_error_name(error));
                false
            }
        }
    }

    /// Existing equipment path used after a range-checked remount. Rust-only;
    /// the rider kernel owns the range/recovery guard.
    pub(crate) fn complete_remount(&mut self, tick: i64) -> bool {
        let Ok(tick_value) = u64::try_from(tick) else {
            return false;
        };
        let tick = SimulationTick::new(tick_value);
        // Course reset may explicitly censor an airborne dive. Close its fire
        // context before restoring equipment; normal range-checked remounts
        // arrive with the same context already closed by landing.
        if let Some(context) = self.kernel.dive_fire_context() {
            if !context.closed_to_new_shots
                && self
                    .kernel
                    .finish_saddle_dive(context.dive_id, tick)
                    .is_err()
            {
                return false;
            }
        }
        self.riding.stance = RiderStance::Mounted;
        self.riding.dive_id = None;
        self.assign_stance(RiderStance::Mounted, None);
        if !self.advance_kernel_to(tick) {
            return false;
        }
        let weapon = self.kernel.equipped_weapon();
        let already_unholstered = !self.kernel.is_holstered();
        let equipped = self.kernel.equip_weapon(weapon);
        self.update_runtime_properties();
        already_unholstered || equipped
    }

    /// Equip 0=Dustwalker, 1=Longspur, or 2=Rattler from the prototype loadout.
    #[func]
    pub fn equip_weapon(&mut self, id: i64) -> bool {
        let Ok(weapon_id) = WeaponId::try_from(id) else {
            godot_error!(
                "MountedWeaponController rejected weapon id {id}; expected 0=Dustwalker, 1=Longspur, or 2=Rattler"
            );
            return false;
        };
        if self.kernel.ammo(weapon_id).is_none()
            || self.riding.stance == RiderStance::SaddleDiveAirborne
        {
            return false;
        }
        let already_equipped =
            self.kernel.equipped_weapon() == weapon_id && !self.kernel.is_holstered();
        let changed = self.kernel.equip_weapon(weapon_id);
        if !changed && !already_equipped {
            return false;
        }
        self.update_runtime_properties();
        if changed {
            self.signals()
                .weapon_changed()
                .emit(i64::from(weapon_id.as_u8()));
            self.emit_ammo_changed();
        }
        true
    }

    /// Validate and submit one fixed-tick fire request immediately.
    ///
    /// Returns `true` only when cadence/ammo/reload/handling/vector/origin gates
    /// accept and consume one round. `shot_fired` carries cosmetic timing; a
    /// later authority result may call the Rust-side result bridge and emit
    /// `hit_confirmed`.
    #[func]
    pub fn request_fire(&mut self, origin: Vector3, direction: Vector3, tick: i64) -> bool {
        let fallback_tick = SimulationTick::new(self.current_tick.max(0).cast_unsigned());
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            self.record_local_rejection(
                fallback_tick,
                Vector3::ZERO,
                Vector3::ZERO,
                ShotRejectionReason::TickReplay,
            );
            return false;
        };
        if let Some(mut rider) = self.rider() {
            rider.bind_mut().record_authority_shot_attempt(tick);
        }

        let Ok(origin_quantized) = QuantizedOrigin::from_meters(
            f64::from(origin.x),
            f64::from(origin.y),
            f64::from(origin.z),
        ) else {
            self.record_local_rejection(
                tick,
                Vector3::ZERO,
                finite_vector_or_zero(direction),
                ShotRejectionReason::OriginLeash,
            );
            return false;
        };
        let Ok(direction_quantized) = QuantizedDirection::from_components(
            f64::from(direction.x),
            f64::from(direction.y),
            f64::from(direction.z),
        ) else {
            self.record_local_rejection(
                tick,
                origin,
                Vector3::ZERO,
                ShotRejectionReason::InvalidDirection,
            );
            return false;
        };

        let muzzle = self.base().get_global_position();
        let Ok(muzzle_quantized) = QuantizedOrigin::from_meters(
            f64::from(muzzle.x),
            f64::from(muzzle.y),
            f64::from(muzzle.z),
        ) else {
            self.record_local_rejection(tick, origin, direction, ShotRejectionReason::OriginLeash);
            return false;
        };
        if origin_quantized.squared_distance_mm(muzzle_quantized)
            > u128::from(ORIGIN_LEASH_MM).pow(2)
        {
            self.record_local_rejection(tick, origin, direction, ShotRejectionReason::OriginLeash);
            return false;
        }

        let riding = self.handling_state();
        let Some(mut rider) = self.rider() else {
            self.record_local_rejection(
                tick,
                origin,
                direction,
                ShotRejectionReason::RiderSnapshot,
            );
            return false;
        };
        let authority_snapshot_matches = rider.bind().can_accept_authority_shot(
            tick,
            riding.stance,
            riding.dive_id,
            self.kernel.equipped_weapon(),
        );
        let duplicate_tick = self
            .shot_ledger
            .accepted(self.kernel.shooter_peer_id(), tick)
            .is_some();
        if !authority_snapshot_matches || duplicate_tick {
            let reason = if !authority_snapshot_matches {
                authority_snapshot_rejection(riding.stance)
            } else {
                ShotRejectionReason::RiderSnapshot
            };
            self.record_local_rejection(tick, origin, direction, reason);
            return false;
        }
        let seed = self.kernel.next_spread_seed();
        let prepared =
            match self
                .kernel
                .request_fire_detailed(tick, direction_quantized, riding, seed)
            {
                Ok(prepared) => prepared,
                Err(rejection) => {
                    self.current_tick = self
                        .current_tick
                        .max(i64::try_from(tick.as_u64()).unwrap_or(i64::MAX));
                    self.record_detailed_rejection(tick, origin, direction, rejection);
                    self.update_runtime_properties();
                    return false;
                }
            };

        let (shot_gait, prelaunch_horizontal_velocity_mmps) = prepared
            .dive_id
            .and_then(|dive_id| {
                self.kernel
                    .dive_fire_context()
                    .filter(|context| context.dive_id == dive_id)
                    .map(|context| {
                        (
                            context.launch_handling.gait,
                            context.prelaunch_horizontal_velocity_mmps,
                        )
                    })
            })
            .unwrap_or((riding.gait, [0; 2]));
        let accepted = AcceptedShotMetadata {
            shooter: self.kernel.shooter_peer_id(),
            tick,
            accepted_shot_index: prepared.accepted_shot_index,
            weapon_id: prepared.weapon_id,
            stance: riding.stance,
            gait: shot_gait,
            dive_id: prepared.dive_id,
            prelaunch_horizontal_velocity_mmps,
        };
        if !self.shot_ledger.record_accepted(accepted)
            || !rider.bind_mut().record_authority_accepted_shot(accepted)
        {
            godot_error!("accepted combat shot could not reach the authoritative rider sink");
            self.last_reject_reason = GString::from("rider_snapshot");
            self.update_runtime_properties();
            return false;
        }

        self.current_tick = self
            .current_tick
            .max(i64::try_from(tick.as_u64()).unwrap_or(i64::MAX));
        self.last_shot_origin = origin;
        self.last_shot_direction = to_godot_direction(prepared.resolved_direction);
        self.last_reject_reason = GString::new();
        let telemetry = ShotTelemetry {
            tick,
            shooter: self.kernel.shooter_peer_id(),
            weapon_id: prepared.weapon_id,
            ammo_mag: prepared.ammo.magazine,
            ammo_reserve: prepared.ammo.reserve,
            spread_millidegrees: prepared.spread_millidegrees,
            sway_millidegrees: prepared.sway.magnitude_millidegrees(),
            gait: riding.gait,
            stance: riding.stance,
            speed_mmps: riding.planar_speed_mmps,
            origin: origin_quantized,
            direction: prepared.resolved_direction,
            // Geometry may be resolved by the authority/scene layer later. A
            // valid unresolved local trace is represented as a miss, never a hit.
            result: ShotOutcome::Miss,
            reject_reason: None,
            target_id: None,
            hit_zone: None,
            damage: 0,
            distance_mm: None,
        };
        self.last_telemetry = telemetry_dictionary(&telemetry);
        self.pending_wire_command = Some(ShotCommand {
            tick,
            shooter_peer_id: self.kernel.shooter_peer_id(),
            weapon_id: prepared.weapon_id,
            origin: origin_quantized,
            direction: prepared.resolved_direction,
            spread_seed: seed,
            claimed_target: None,
        });
        self.pending_local_shot = Some(PendingLocalShot {
            tick,
            weapon_id: prepared.weapon_id,
            direction: prepared.resolved_direction,
        });
        self.update_runtime_properties();
        let emitted_tick = self.current_tick;
        self.signals()
            .shot_fired()
            .emit(emitted_tick, i64::from(prepared.weapon_id.as_u8()));
        self.signals().shot_accepted().emit(
            emitted_tick,
            i64::from(prepared.weapon_id.as_u8()),
            i64::try_from(prepared.accepted_shot_index).unwrap_or(i64::MAX),
            prepared
                .dive_id
                .and_then(|id| i64::try_from(id.get()).ok())
                .unwrap_or(-1),
        );
        self.emit_ammo_changed();
        true
    }

    /// Start the equipped rifle's deterministic reload at the current tick.
    #[func]
    pub fn request_reload(&mut self) -> bool {
        let tick = SimulationTick::new(self.current_tick.max(0).cast_unsigned());
        let riding = self.handling_state();
        match self.kernel.request_reload(tick, riding) {
            Ok(reload) => {
                self.last_reject_reason = GString::new();
                self.update_runtime_properties();
                let tick_value = i64::try_from(tick.as_u64()).unwrap_or(i64::MAX);
                let required_ticks = i64::try_from(reload.required_ticks).unwrap_or(i64::MAX);
                self.signals()
                    .reload_started()
                    .emit(tick_value, required_ticks);
                self.signals()
                    .reload_progressed()
                    .emit(tick_value, 0.0, 0, required_ticks);
                true
            }
            Err(reason) => {
                let exact_reason = reload_rejection_name(reason, riding.stance);
                self.last_reject_reason = GString::from(exact_reason);
                self.update_runtime_properties();
                let signal_reason = GString::from(exact_reason);
                self.signals().reload_rejected().emit(
                    i64::try_from(tick.as_u64()).unwrap_or(i64::MAX),
                    &signal_reason,
                );
                false
            }
        }
    }

    /// Exact kernel-equivalent camera-relative Saddle Dive launch preview.
    /// This is presentation-only and cannot mutate launch or combat state.
    #[func]
    pub fn preview_dive_direction(
        &self,
        horse_velocity: Vector3,
        chosen_direction: Vector3,
    ) -> VarDictionary {
        let velocity = quantized_velocity_mmps(horse_velocity);
        let chosen = QuantizedDirection::from_components(
            f64::from(chosen_direction.x),
            f64::from(chosen_direction.y),
            f64::from(chosen_direction.z),
        )
        .ok();
        launch_preview_dictionary(velocity, chosen)
    }

    /// Return the complete locked design row and live ammo state.
    #[func]
    pub fn get_weapon_stats(&self) -> VarDictionary {
        weapon_stats_dictionary(
            *self.kernel.equipped_weapon().stats(),
            self.kernel.equipped_ammo(),
            self.kernel.reload(),
            self.kernel.tick_rate(),
            self.kernel.is_holstered(),
        )
    }

    /// Resolve one local-authority geometry hit for the most recently accepted shot.
    ///
    /// Godot supplies only target/zone/distance evidence from its physics ray. Damage and range
    /// are derived from the locked native weapon row. Networked matches replace this bridge with
    /// `CombatAuthority::validate_shot` over authority snapshots.
    /// Consume the native command exactly once for signed network delivery.
    #[func]
    pub fn take_pending_shot_command_json(&mut self) -> GString {
        self.pending_wire_command
            .take()
            .map_or_else(GString::new, |command| {
                let encoded = serde_json::to_string(&command).unwrap_or_default();
                GString::from(&encoded)
            })
    }

    /// Apply one authority-signed result. Native attribution deduplicates it.
    #[func]
    pub fn apply_authority_result_json(&mut self, result_json: GString) -> bool {
        let Ok(result) = serde_json::from_str::<ShotResult>(&result_json.to_string()) else {
            return false;
        };
        self.apply_authority_result(&result)
    }

    #[func]
    pub fn resolve_local_hit(
        &mut self,
        target_id: i64,
        hit_zone: GString,
        distance_m: f64,
    ) -> bool {
        let Some(pending) = self.pending_local_shot.take() else {
            return false;
        };
        let Ok(target_id) = u64::try_from(target_id) else {
            return false;
        };
        if !distance_m.is_finite() || distance_m < 0.0 {
            return false;
        }
        let distance_mm_f64 = (distance_m * 1_000.0).round();
        if distance_mm_f64 > f64::from(u32::MAX) {
            return false;
        }
        let distance_mm = distance_mm_f64 as u32;
        let stats = *pending.weapon_id.stats();
        if distance_mm > stats.hitscan_clamp_mm {
            return false;
        }
        let zone = match hit_zone.to_string().to_ascii_lowercase().as_str() {
            "head" => HitZone::Head,
            "body" => HitZone::Body,
            _ => return false,
        };
        let result = ShotResult {
            tick: pending.tick,
            shooter_peer_id: self.kernel.shooter_peer_id(),
            weapon_id: pending.weapon_id,
            outcome: ShotOutcome::Hit,
            rejection_reason: None,
            resolved_direction: Some(pending.direction),
            target_id: Some(EntityId(target_id)),
            hit_zone: Some(zone),
            damage: stats.damage_at(distance_mm, zone),
            distance_mm: Some(distance_mm),
            eliminated: false,
        };
        self.apply_authority_result(&result)
    }

    /// Resolve the most recently accepted local shot as a miss.
    #[func]
    pub fn resolve_local_miss(&mut self) -> bool {
        let Some(pending) = self.pending_local_shot.take() else {
            return false;
        };
        let result = ShotResult {
            tick: pending.tick,
            shooter_peer_id: self.kernel.shooter_peer_id(),
            weapon_id: pending.weapon_id,
            outcome: ShotOutcome::Miss,
            rejection_reason: None,
            resolved_direction: Some(pending.direction),
            target_id: None,
            hit_zone: None,
            damage: 0,
            distance_mm: None,
            eliminated: false,
        };
        self.apply_authority_result(&result)
    }
}

#[godot_api]
impl INode3D for MountedWeaponController {
    fn init(base: Base<Node3D>) -> Self {
        let shooter = default_shooter();
        let kernel = CombatKernel::with_full_loadout(DEFAULT_TICK_RATE, 0, shooter)
            .expect("default combat tick rate is valid");
        let ammo = kernel.equipped_ammo();
        Self {
            base,
            tick_rate: i64::from(DEFAULT_TICK_RATE),
            lobby_seed: 0,
            shooter_peer_id: GString::from(DEFAULT_SHOOTER_ID),
            rider_path: NodePath::from(".."),
            stance_id: i64::from(RiderStance::MOUNTED_ID),
            stance_known: true,
            dive_id: -1,
            gait: 0,
            speed_mps: 0.0,
            gait_top_speed_mps: 14.0,
            yaw_rate_degrees: 0.0,
            stumbling: false,
            ads: false,
            sprint_gallop: false,
            current_tick: 0,
            weapon_id: i64::from(WeaponId::Dustwalker.as_u8()),
            ammo_mag: i64::from(ammo.magazine),
            ammo_reserve: i64::from(ammo.reserve),
            is_reloading: false,
            reload_progress: 0.0,
            recoil_pitch_degrees: 0.0,
            recoil_yaw_degrees: 0.0,
            last_shot_origin: Vector3::ZERO,
            last_shot_direction: Vector3::new(0.0, 0.0, -1.0),
            last_reject_reason: GString::new(),
            last_telemetry: VarDictionary::new(),
            kernel,
            riding: RidingState::default(),
            context_tick: None,
            authority_epoch: 0,
            shot_ledger: ShotAttributionLedger::default(),
            pending_local_shot: None,
            pending_wire_command: None,
        }
    }

    fn ready(&mut self) {
        self.rebuild_kernel_from_exports();
        self.update_runtime_properties();
        let weapon_id = self.kernel.equipped_weapon();
        self.signals()
            .weapon_changed()
            .emit(i64::from(weapon_id.as_u8()));
        self.emit_ammo_changed();
    }

    fn physics_process(&mut self, _delta: f64) {
        // The scene gameplay coordinator injects the one absolute simulation
        // tick. A private delta accumulator would drift from movement/networking.
    }
}

impl MountedWeaponController {
    fn rebuild_kernel_from_exports(&mut self) {
        let tick_rate = u32::try_from(self.tick_rate)
            .ok()
            .filter(|rate| *rate > 0)
            .unwrap_or(DEFAULT_TICK_RATE);
        self.tick_rate = i64::from(tick_rate);
        let shooter_text = self.shooter_peer_id.to_string();
        let shooter = PlayerId::parse(&shooter_text).unwrap_or_else(|error| {
            godot_error!(
                "MountedWeaponController rejected shooter_peer_id '{}': {error}; using prototype ID",
                shooter_text
            );
            self.shooter_peer_id = GString::from(DEFAULT_SHOOTER_ID);
            default_shooter()
        });
        self.kernel =
            CombatKernel::with_full_loadout(tick_rate, self.lobby_seed.cast_unsigned(), shooter)
                .expect("validated tick rate");
        self.current_tick = 0;
        self.riding = RidingState::default();
        self.context_tick = None;
        self.authority_epoch = 0;
        self.shot_ledger = ShotAttributionLedger::default();
        self.assign_stance(RiderStance::Mounted, None);
        self.last_reject_reason = GString::new();
        self.last_telemetry = VarDictionary::new();
        self.pending_local_shot = None;
    }

    fn rider(&self) -> Option<Gd<SaddleDiveController>> {
        self.base()
            .try_get_node_as::<SaddleDiveController>(&self.rider_path)
    }

    pub(crate) fn can_begin_authoritative_dive(
        &self,
        tick: SimulationTick,
        launch_weapon: WeaponId,
    ) -> bool {
        self.context_tick == Some(tick)
            && self.riding.stance == RiderStance::Mounted
            && self.riding.dive_id.is_none()
            && self.riding.is_consistent()
            && self.kernel.equipped_weapon() == launch_weapon
            && self
                .kernel
                .dive_fire_context()
                .is_none_or(|context| context.closed_to_new_shots)
    }

    pub(crate) fn synchronize_authoritative_stance(
        &mut self,
        stance: RiderStance,
        dive_id: Option<DiveId>,
    ) -> bool {
        let candidate = RidingState {
            stance,
            dive_id,
            ..self.riding
        };
        if !stance.is_known() || !stance.is_canonical() || !candidate.is_consistent() {
            return false;
        }
        self.riding = candidate;
        let installed_tick = SimulationTick::new(self.current_tick.max(0).cast_unsigned());
        if self.kernel.advance_to(installed_tick, candidate).is_err() {
            return false;
        }
        self.assign_stance(stance, dive_id);
        self.update_runtime_properties();
        true
    }

    pub(crate) fn advance_authority_epoch(&mut self, authority_epoch: u64) -> bool {
        if authority_epoch < self.authority_epoch {
            return false;
        }
        self.authority_epoch = authority_epoch;
        true
    }

    pub(crate) fn bind_session_identity(
        &mut self,
        shooter: PlayerId,
        authority_epoch: u64,
    ) -> bool {
        if self.kernel.shooter_peer_id() == shooter && self.authority_epoch == authority_epoch {
            return true;
        }
        if self.kernel.shot_index() != 0
            || self.kernel.dive_fire_context().is_some()
            || self.kernel.reload().is_some()
            || self.pending_local_shot.is_some()
        {
            return false;
        }
        let equipped = self.kernel.equipped_weapon();
        let tick_rate = self.kernel.tick_rate();
        let Ok(mut kernel) =
            CombatKernel::with_full_loadout(tick_rate, self.lobby_seed.cast_unsigned(), shooter)
        else {
            return false;
        };
        if equipped != kernel.equipped_weapon() && !kernel.equip_weapon(equipped) {
            return false;
        }
        self.kernel = kernel;
        self.shooter_peer_id = GString::from(&shooter.to_canonical_string());
        self.authority_epoch = authority_epoch;
        self.riding = RidingState::default();
        self.context_tick = None;
        self.current_tick = 0;
        self.shot_ledger = ShotAttributionLedger::default();
        self.pending_local_shot = None;
        self.assign_stance(RiderStance::Mounted, None);
        self.update_runtime_properties();
        true
    }

    fn handling_state(&self) -> RidingState {
        self.riding
    }

    fn assign_stance(&mut self, stance: RiderStance, dive_id: Option<DiveId>) {
        self.stance_id = i64::from(stance.as_u8());
        self.stance_known = stance.is_known();
        self.dive_id = dive_id
            .and_then(|id| i64::try_from(id.get()).ok())
            .unwrap_or(-1);
    }

    fn advance_kernel_to(&mut self, tick: SimulationTick) -> bool {
        let previous_ammo = self.kernel.equipped_ammo();
        let previous_reload = self.kernel.reload();
        let Ok(outcome) = self.kernel.advance_to(tick, self.riding) else {
            return false;
        };
        self.current_tick = i64::try_from(tick.as_u64()).unwrap_or(i64::MAX);
        self.update_runtime_properties();
        let emitted_tick = self.current_tick;
        let current_reload = self.kernel.reload();
        if let Some(reload) = current_reload {
            if previous_reload != Some(reload) {
                self.signals().reload_progressed().emit(
                    emitted_tick,
                    f64::from(reload.progress_milli()) / 1_000.0,
                    i64::try_from(reload.active_ticks).unwrap_or(i64::MAX),
                    i64::try_from(reload.required_ticks).unwrap_or(i64::MAX),
                );
            }
        } else if outcome.reload_completed {
            let required_ticks = previous_reload
                .map(|reload| i64::try_from(reload.required_ticks).unwrap_or(i64::MAX))
                .unwrap_or(0);
            self.signals().reload_progressed().emit(
                emitted_tick,
                1.0,
                required_ticks,
                required_ticks,
            );
        }
        if self.kernel.equipped_ammo() != previous_ammo {
            self.emit_ammo_changed();
        }
        if outcome.reload_completed {
            let ammo = self.kernel.equipped_ammo();
            self.signals().reload_completed().emit(
                emitted_tick,
                i64::from(ammo.magazine),
                i64::from(ammo.reserve),
            );
        }
        true
    }

    fn record_local_rejection(
        &mut self,
        tick: SimulationTick,
        origin: Vector3,
        direction: Vector3,
        reason: ShotRejectionReason,
    ) {
        self.record_rejection(tick, origin, direction, reason, reason.as_str());
    }

    fn record_detailed_rejection(
        &mut self,
        tick: SimulationTick,
        origin: Vector3,
        direction: Vector3,
        rejection: FireRejection,
    ) {
        let exact = rejection
            .dive_reason
            .map_or(rejection.wire_reason.as_str(), dive_rejection_name);
        self.record_rejection(tick, origin, direction, rejection.wire_reason, exact);
    }

    fn record_rejection(
        &mut self,
        tick: SimulationTick,
        origin: Vector3,
        direction: Vector3,
        wire_reason: ShotRejectionReason,
        exact_reason: &'static str,
    ) {
        let riding = self.handling_state();
        let weapon_id = self.kernel.equipped_weapon();
        let ammo = self.kernel.equipped_ammo();
        let origin_quantized = QuantizedOrigin::from_meters(
            f64::from(origin.x),
            f64::from(origin.y),
            f64::from(origin.z),
        )
        .unwrap_or_default();
        let direction_quantized = QuantizedDirection::from_components(
            f64::from(direction.x),
            f64::from(direction.y),
            f64::from(direction.z),
        )
        .unwrap_or_default();
        let sway = spurfire_protocol::deterministic_sway(tick, self.kernel.tick_rate(), riding);
        let telemetry = ShotTelemetry {
            tick,
            shooter: self.kernel.shooter_peer_id(),
            weapon_id,
            ammo_mag: ammo.magazine,
            ammo_reserve: ammo.reserve,
            spread_millidegrees: effective_spread_millidegrees(*weapon_id.stats(), riding),
            sway_millidegrees: sway.magnitude_millidegrees(),
            gait: riding.gait,
            stance: riding.stance,
            speed_mmps: riding.planar_speed_mmps,
            origin: origin_quantized,
            direction: direction_quantized,
            result: ShotOutcome::Reject,
            reject_reason: Some(wire_reason),
            target_id: None,
            hit_zone: None,
            damage: 0,
            distance_mm: None,
        };
        self.last_shot_origin = finite_vector_or_zero(origin);
        self.last_shot_direction = finite_vector_or_zero(direction);
        self.last_reject_reason = GString::from(exact_reason);
        self.last_telemetry = telemetry_dictionary(&telemetry);
        self.last_telemetry
            .set("internal_reject_reason", exact_reason);
        self.pending_local_shot = None;
        let signal_reason = GString::from(exact_reason);
        self.signals().fire_rejected().emit(
            i64::try_from(tick.as_u64()).unwrap_or(i64::MAX),
            &signal_reason,
        );
    }

    fn update_runtime_properties(&mut self) {
        let weapon_id = self.kernel.equipped_weapon();
        let ammo = self.kernel.equipped_ammo();
        let recoil = self.kernel.recoil();
        self.weapon_id = i64::from(weapon_id.as_u8());
        self.ammo_mag = i64::from(ammo.magazine);
        self.ammo_reserve = i64::from(ammo.reserve);
        self.is_reloading = self.kernel.reload().is_some();
        self.reload_progress = self
            .kernel
            .reload()
            .map_or(0.0, |reload| f64::from(reload.progress_milli()) / 1_000.0);
        self.recoil_pitch_degrees = f64::from(recoil.pitch_millidegrees) / 1_000.0;
        self.recoil_yaw_degrees = f64::from(recoil.yaw_millidegrees) / 1_000.0;
    }

    fn emit_ammo_changed(&mut self) {
        let ammo = self.kernel.equipped_ammo();
        self.signals()
            .ammo_changed()
            .emit(i64::from(ammo.magazine), i64::from(ammo.reserve));
    }

    /// Rust/network bridge for a server result. This is deliberately not a
    /// Godot method, preserving the locked scene-facing API.
    pub fn apply_authority_result(&mut self, result: &ShotResult) -> bool {
        if result.shooter_peer_id != self.kernel.shooter_peer_id() {
            return false;
        }
        let attribution = self
            .shot_ledger
            .observe_result(self.authority_epoch, result);
        if attribution.duplicate || attribution.accepted_shot.is_none() {
            return false;
        }
        let Some(mut rider) = self.rider() else {
            return false;
        };
        if !rider
            .bind_mut()
            .record_attributed_authority_result(&attribution)
        {
            return false;
        }
        if let (ShotOutcome::Hit, Some(target_id), Some(hit_zone)) =
            (result.outcome, result.target_id, result.hit_zone)
        {
            let target_id = i64::try_from(target_id.0).unwrap_or(i64::MAX);
            let hit_zone = GString::from(hit_zone.as_str());
            self.signals()
                .hit_confirmed()
                .emit(target_id, &hit_zone, i64::from(result.damage));
        }
        self.last_telemetry.set("result", result.outcome.as_str());
        self.last_telemetry.set(
            "reject_reason",
            result
                .rejection_reason
                .map_or("", ShotRejectionReason::as_str),
        );
        self.last_telemetry.set(
            "target_id",
            result
                .target_id
                .and_then(|target| i64::try_from(target.0).ok())
                .unwrap_or(-1),
        );
        self.last_telemetry
            .set("hit_zone", result.hit_zone.map_or("", HitZone::as_str));
        self.last_telemetry.set("damage", i64::from(result.damage));
        self.last_telemetry.set(
            "distance_m",
            result
                .distance_mm
                .map_or(-1.0, |distance| f64::from(distance) / 1_000.0),
        );
        let outcome = GString::from(result.outcome.as_str());
        let hit_zone = GString::from(result.hit_zone.map_or("", HitZone::as_str));
        let resolved_direction = result
            .resolved_direction
            .map_or(Vector3::ZERO, to_godot_direction);
        self.signals().shot_resolved().emit(
            i64::try_from(result.tick.as_u64()).unwrap_or(i64::MAX),
            &outcome,
            &hit_zone,
            i64::from(result.damage),
            resolved_direction,
        );
        true
    }
}

fn default_shooter() -> PlayerId {
    PlayerId::parse(DEFAULT_SHOOTER_ID).expect("prototype shooter UUID is valid")
}

fn gait_from_id(value: i64) -> Option<CombatGait> {
    match value {
        0 => Some(CombatGait::Idle),
        1 => Some(CombatGait::Walk),
        2 => Some(CombatGait::Trot),
        // Existing M0 HorseController uses 3 for Gallop.
        3 => Some(CombatGait::Gallop),
        // Optional explicit canter input for future animation integration.
        4 => Some(CombatGait::Canter),
        _ => None,
    }
}

const fn gait_id(value: CombatGait) -> i64 {
    match value {
        CombatGait::Idle => 0,
        CombatGait::Walk => 1,
        CombatGait::Trot => 2,
        CombatGait::Gallop => 3,
        CombatGait::Canter => 4,
    }
}

fn parse_dive_for_stance(stance: RiderStance, value: i64) -> Option<Option<DiveId>> {
    if stance == RiderStance::SaddleDiveAirborne {
        let value = u64::try_from(value).ok().and_then(DiveId::new)?;
        Some(Some(value))
    } else if value == -1 {
        Some(None)
    } else {
        None
    }
}

fn meters_per_second_to_mmps(value: f64) -> Option<u32> {
    if !value.is_finite() || !(0.0..=MAX_HANDLING_SPEED_MPS).contains(&value) {
        None
    } else {
        Some((value * 1_000.0).round() as u32)
    }
}

fn positive_meters_per_second_to_mmps(value: f64) -> Option<u32> {
    meters_per_second_to_mmps(value).filter(|value| *value > 0)
}

fn meters_per_second_to_signed_mmps(value: f64) -> Option<i32> {
    if !value.is_finite() || !(-MAX_HANDLING_SPEED_MPS..=MAX_HANDLING_SPEED_MPS).contains(&value) {
        None
    } else {
        Some((value * 1_000.0).round() as i32)
    }
}

fn degrees_to_millidegrees(value: f64) -> Option<i32> {
    if !value.is_finite()
        || !(-MAX_HANDLING_YAW_RATE_DEGREES..=MAX_HANDLING_YAW_RATE_DEGREES).contains(&value)
    {
        None
    } else {
        Some((value * 1_000.0).round() as i32)
    }
}

fn quantized_velocity_mmps(value: Vector3) -> [i32; 3] {
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

fn launch_preview_dictionary(
    horse_velocity_mmps: [i32; 3],
    chosen_direction: Option<QuantizedDirection>,
) -> VarDictionary {
    let preview = clamp_launch_direction(horse_velocity_mmps, chosen_direction);
    let mut row = VarDictionary::new();
    row.set(
        "requested_angle_degrees",
        f64::from(preview.requested_angle_millidegrees) / 1_000.0,
    );
    row.set(
        "clamped_angle_degrees",
        f64::from(preview.clamped_angle_millidegrees) / 1_000.0,
    );
    row.set("direction_was_clamped", preview.direction_was_clamped);
    row.set(
        "clamped_direction",
        to_godot_direction(preview.clamped_direction),
    );
    row
}

fn finite_vector_or_zero(value: Vector3) -> Vector3 {
    if value.x.is_finite() && value.y.is_finite() && value.z.is_finite() {
        value
    } else {
        Vector3::ZERO
    }
}

fn to_godot_direction(value: QuantizedDirection) -> Vector3 {
    Vector3::new(
        value.x as f32 / DIRECTION_UNITS as f32,
        value.y as f32 / DIRECTION_UNITS as f32,
        value.z as f32 / DIRECTION_UNITS as f32,
    )
}

const fn authority_snapshot_rejection(stance: RiderStance) -> ShotRejectionReason {
    if matches!(stance, RiderStance::MountedAirborne) {
        ShotRejectionReason::Airborne
    } else {
        ShotRejectionReason::RiderSnapshot
    }
}

fn reload_rejection_name(error: ReloadStartError, stance: RiderStance) -> &'static str {
    match error {
        ReloadStartError::Dismounted
            if matches!(
                stance,
                RiderStance::LandingProne | RiderStance::LandingRecovery
            ) =>
        {
            "recovering"
        }
        ReloadStartError::Dismounted if stance == RiderStance::OnFootStanding => "holstered",
        ReloadStartError::TickReplay => "tick_replay",
        ReloadStartError::Holstered => "holstered",
        ReloadStartError::Airborne => "airborne",
        ReloadStartError::Dismounted => "dismounted",
        ReloadStartError::AlreadyReloading => "reloading",
        ReloadStartError::MagazineFull => "magazine_full",
        ReloadStartError::NoReserve => "no_reserve",
    }
}

fn dive_context_error_name(error: DiveContextError) -> &'static str {
    match error {
        DiveContextError::TickReplay => "tick_replay",
        DiveContextError::WeaponMismatch => "weapon_mismatch",
        DiveContextError::DiveAlreadyOpen => "dive_already_open",
        DiveContextError::ContextMismatch => "dive_context_mismatch",
    }
}

fn dive_rejection_name(reason: DiveFireRejection) -> &'static str {
    match reason {
        DiveFireRejection::ContextMismatch => "dive_context_mismatch",
        DiveFireRejection::WeaponMismatch => "dive_weapon_mismatch",
        DiveFireRejection::Closed => "dive_closed",
        DiveFireRejection::ShotCap => "dive_shot_cap",
    }
}

fn weapon_stats_dictionary(
    stats: WeaponStats,
    ammo: WeaponAmmo,
    reload: Option<spurfire_protocol::ReloadSnapshot>,
    tick_rate: u32,
    holstered: bool,
) -> VarDictionary {
    let mut row = VarDictionary::new();
    row.set("weapon_id", i64::from(stats.id.as_u8()));
    row.set("weapon_key", stats.id.as_str());
    row.set("name", stats.display_name);
    row.set("display_name", stats.display_name);
    row.set("magazine", i64::from(stats.magazine_capacity));
    row.set("reserve", i64::from(stats.reserve_capacity));
    row.set("magazine_capacity", i64::from(stats.magazine_capacity));
    row.set("reserve_capacity", i64::from(stats.reserve_capacity));
    row.set("rounds_per_second", stats.rounds_per_second());
    row.set("reload_seconds", stats.reload_seconds());
    row.set(
        "base_spread_deg",
        f64::from(stats.base_spread_millidegrees) / 1_000.0,
    );
    row.set(
        "moving_spread_deg",
        f64::from(stats.moving_spread_millidegrees) / 1_000.0,
    );
    row.set(
        "gallop_spread_deg",
        f64::from(stats.gallop_spread_millidegrees) / 1_000.0,
    );
    row.set(
        "recoil_vertical_deg",
        f64::from(stats.recoil_vertical_millidegrees) / 1_000.0,
    );
    row.set(
        "recoil_yaw_deg",
        f64::from(stats.recoil_yaw_millidegrees) / 1_000.0,
    );
    row.set("damage", i64::from(stats.body_damage));
    row.set("body_damage", i64::from(stats.body_damage));
    row.set(
        "falloff_start_m",
        f64::from(stats.falloff_start_mm) / 1_000.0,
    );
    row.set("falloff_end_m", f64::from(stats.falloff_end_mm) / 1_000.0);
    row.set("minimum_damage", i64::from(stats.minimum_body_damage));
    row.set(
        "headshot_multiplier",
        f64::from(stats.headshot_multiplier_milli) / 1_000.0,
    );
    row.set(
        "effective_range_m",
        f64::from(stats.effective_range_mm) / 1_000.0,
    );
    row.set(
        "hitscan_clamp_m",
        f64::from(stats.hitscan_clamp_mm) / 1_000.0,
    );
    row.set(
        "minimum_intershot_ticks",
        i64::try_from(stats.cadence_ticks(tick_rate)).unwrap_or(i64::MAX),
    );
    row.set(
        "reload_ticks",
        i64::try_from(stats.reload_ticks(tick_rate)).unwrap_or(i64::MAX),
    );
    row.set("ammo_mag", i64::from(ammo.magazine));
    row.set("ammo_reserve", i64::from(ammo.reserve));
    row.set("is_reloading", reload.is_some());
    row.set(
        "reload_progress",
        reload.map_or(0.0, |state| f64::from(state.progress_milli()) / 1_000.0),
    );
    row.set("holstered", holstered);
    row.set("color_identity", weapon_color(stats.id));
    row
}

fn telemetry_dictionary(telemetry: &ShotTelemetry) -> VarDictionary {
    let mut payload = VarDictionary::new();
    payload.set(
        "tick",
        i64::try_from(telemetry.tick.as_u64()).unwrap_or(i64::MAX),
    );
    payload.set("shooter", telemetry.shooter.to_canonical_string());
    payload.set("weapon_id", i64::from(telemetry.weapon_id.as_u8()));
    payload.set("weapon_key", telemetry.weapon_id.as_str());
    payload.set("ammo_mag", i64::from(telemetry.ammo_mag));
    payload.set("ammo_reserve", i64::from(telemetry.ammo_reserve));
    payload.set(
        "spread_deg",
        f64::from(telemetry.spread_millidegrees) / 1_000.0,
    );
    payload.set("sway_deg", f64::from(telemetry.sway_millidegrees) / 1_000.0);
    payload.set("gait", telemetry.gait.as_str());
    payload.set("stance_id", i64::from(telemetry.stance.as_u8()));
    payload.set("stance", telemetry.stance.as_str());
    payload.set("speed_mps", f64::from(telemetry.speed_mmps) / 1_000.0);
    payload.set(
        "origin",
        Vector3::new(
            telemetry.origin.x as f32 / 1_000.0,
            telemetry.origin.y as f32 / 1_000.0,
            telemetry.origin.z as f32 / 1_000.0,
        ),
    );
    payload.set("direction", to_godot_direction(telemetry.direction));
    payload.set("result", telemetry.result.as_str());
    payload.set(
        "reject_reason",
        telemetry
            .reject_reason
            .map_or("", ShotRejectionReason::as_str),
    );
    payload.set(
        "target_id",
        telemetry
            .target_id
            .and_then(|target| i64::try_from(target.0).ok())
            .unwrap_or(-1),
    );
    payload.set("hit_zone", telemetry.hit_zone.map_or("", HitZone::as_str));
    payload.set("damage", i64::from(telemetry.damage));
    payload.set(
        "distance_m",
        telemetry
            .distance_mm
            .map_or(-1.0, |distance| f64::from(distance) / 1_000.0),
    );
    payload
}

fn weapon_color(weapon_id: WeaponId) -> &'static str {
    match weapon_id {
        WeaponId::Dustwalker => "tan_brown",
        WeaponId::Longspur => "gunmetal_wood",
        WeaponId::Rattler => "olive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horse_gait_ids_and_future_canter_map_without_panics() {
        assert_eq!(gait_from_id(0), Some(CombatGait::Idle));
        assert_eq!(gait_from_id(1), Some(CombatGait::Walk));
        assert_eq!(gait_from_id(2), Some(CombatGait::Trot));
        assert_eq!(gait_from_id(3), Some(CombatGait::Gallop));
        assert_eq!(gait_from_id(4), Some(CombatGait::Canter));
        assert_eq!(gait_from_id(i64::MAX), None);
    }

    #[test]
    fn handling_quantization_rejects_nonfinite_and_out_of_range_values() {
        assert_eq!(meters_per_second_to_mmps(14.5), Some(14_500));
        assert_eq!(meters_per_second_to_mmps(f64::NAN), None);
        assert_eq!(meters_per_second_to_mmps(-1.0), None);
        assert_eq!(meters_per_second_to_mmps(201.0), None);
        assert_eq!(positive_meters_per_second_to_mmps(0.0), None);
        assert_eq!(degrees_to_millidegrees(-60.5), Some(-60_500));
        assert_eq!(degrees_to_millidegrees(f64::INFINITY), None);
        assert_eq!(degrees_to_millidegrees(3_601.0), None);
    }

    #[test]
    fn ordinary_jump_snapshot_rejection_stays_airborne() {
        assert_eq!(
            authority_snapshot_rejection(RiderStance::MountedAirborne),
            ShotRejectionReason::Airborne
        );
        assert_eq!(
            authority_snapshot_rejection(RiderStance::SaddleDiveAirborne),
            ShotRejectionReason::RiderSnapshot
        );
        assert_eq!(
            authority_snapshot_rejection(RiderStance::LandingProne),
            ShotRejectionReason::RiderSnapshot
        );
    }

    #[test]
    fn fixed_weapon_ids_have_distinct_visual_identity() {
        assert_eq!(weapon_color(WeaponId::Dustwalker), "tan_brown");
        assert_eq!(weapon_color(WeaponId::Longspur), "gunmetal_wood");
        assert_eq!(weapon_color(WeaponId::Rattler), "olive");
    }

    #[test]
    fn reload_rejections_are_stance_specific_for_presentation() {
        assert_eq!(
            reload_rejection_name(ReloadStartError::Airborne, RiderStance::SaddleDiveAirborne),
            "airborne"
        );
        assert_eq!(
            reload_rejection_name(ReloadStartError::Dismounted, RiderStance::LandingRecovery),
            "recovering"
        );
        assert_eq!(
            reload_rejection_name(ReloadStartError::Dismounted, RiderStance::OnFootStanding),
            "holstered"
        );
    }

    #[test]
    fn dive_preview_uses_the_locked_seventy_five_degree_kernel_cone() {
        let velocity = [0, 0, -9_000];
        for (angle, expected, clamped) in [
            (0_i32, 0_i32, false),
            (45_000, 45_000, false),
            (-45_000, -45_000, false),
            (75_000, 75_000, false),
            (-75_000, -75_000, false),
            (90_000, 75_000, true),
            (-90_000, -75_000, true),
            (180_000, 75_000, true),
        ] {
            let radians = f64::from(angle).to_radians() / 1_000.0;
            let chosen = QuantizedDirection::from_components(-radians.sin(), 0.0, -radians.cos())
                .expect("test direction is normalized");
            let preview = clamp_launch_direction(velocity, Some(chosen));
            assert!((preview.clamped_angle_millidegrees - expected).abs() <= 1);
            assert_eq!(preview.direction_was_clamped, clamped);
        }
    }
}
