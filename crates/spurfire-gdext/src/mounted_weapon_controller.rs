//! Thin Godot `Node3D` adapter for the engine-independent combat kernel.

use godot::classes::{INode3D, Node3D};
use godot::prelude::*;
use spurfire_protocol::{
    effective_spread_millidegrees, CombatGait, CombatKernel, EntityId, HitZone, PlayerId,
    QuantizedDirection, QuantizedOrigin, ReloadStartError, RidingState, ShotOutcome,
    ShotRejectionReason, ShotResult, ShotTelemetry, SimulationTick, WeaponAmmo, WeaponId,
    WeaponStats, DIRECTION_UNITS, ORIGIN_LEASH_MM,
};

const DEFAULT_TICK_RATE: u32 = 60;
const DEFAULT_SHOOTER_ID: &str = "00000000-0000-4000-8000-000000000001";

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

    // Rewound handling inputs supplied by the horse/scene integration.
    #[export]
    mounted: bool,
    #[export]
    gait: i64,
    #[export]
    speed_mps: f64,
    #[export]
    gait_top_speed_mps: f64,
    #[export]
    yaw_rate_degrees: f64,
    #[export]
    airborne: bool,
    #[export]
    stumbling: bool,
    #[export]
    ads: bool,
    #[export]
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
    tick_accumulator: f64,
    pending_local_shot: Option<PendingLocalShot>,
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

    /// Emitted when an authority result confirms a target hit.
    #[signal]
    fn hit_confirmed(target_id: i64, hit_zone: GString, damage: i64);

    /// Equip 0=Dustwalker, 1=Longspur, or 2=Rattler from the prototype loadout.
    #[func]
    pub fn equip_weapon(&mut self, id: i64) -> bool {
        let Ok(weapon_id) = WeaponId::try_from(id) else {
            godot_error!(
                "MountedWeaponController rejected weapon id {id}; expected 0=Dustwalker, 1=Longspur, or 2=Rattler"
            );
            return false;
        };
        if self.kernel.ammo(weapon_id).is_none() {
            return false;
        }
        let changed = self.kernel.equip_weapon(weapon_id);
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
        let prepared = match self.kernel.request_fire(tick, direction_quantized, riding) {
            Ok(prepared) => prepared,
            Err(reason) => {
                self.current_tick = self
                    .current_tick
                    .max(i64::try_from(tick.as_u64()).unwrap_or(i64::MAX));
                self.record_local_rejection(tick, origin, direction, reason);
                self.update_runtime_properties();
                return false;
            }
        };

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
        self.emit_ammo_changed();
        true
    }

    /// Start the equipped rifle's deterministic reload at the current tick.
    #[func]
    pub fn request_reload(&mut self) -> bool {
        let tick = SimulationTick::new(self.current_tick.max(0).cast_unsigned());
        let riding = self.handling_state();
        match self.kernel.request_reload(tick, riding) {
            Ok(_) => {
                self.last_reject_reason = GString::new();
                self.update_runtime_properties();
                true
            }
            Err(reason) => {
                self.last_reject_reason = GString::from(reload_error_name(reason));
                self.update_runtime_properties();
                false
            }
        }
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
        self.apply_authority_result(&result);
        true
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
            mounted: true,
            gait: 0,
            speed_mps: 0.0,
            gait_top_speed_mps: 14.0,
            yaw_rate_degrees: 0.0,
            airborne: false,
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
            tick_accumulator: 0.0,
            pending_local_shot: None,
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

    fn physics_process(&mut self, delta: f64) {
        if !delta.is_finite() || delta <= 0.0 {
            return;
        }
        self.tick_accumulator += delta * f64::from(self.kernel.tick_rate());
        let steps = self.tick_accumulator.floor().clamp(0.0, 1_000.0) as u64;
        if steps == 0 {
            return;
        }
        self.tick_accumulator -= steps as f64;
        let previous_ammo = self.kernel.equipped_ammo();
        let riding = self.handling_state();
        for _ in 0..steps {
            self.current_tick = self.current_tick.saturating_add(1);
            let tick = SimulationTick::new(self.current_tick.cast_unsigned());
            if self.kernel.advance_to(tick, riding).is_err() {
                break;
            }
        }
        self.update_runtime_properties();
        if self.kernel.equipped_ammo() != previous_ammo {
            self.emit_ammo_changed();
        }
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
        self.tick_accumulator = 0.0;
        self.last_reject_reason = GString::new();
        self.last_telemetry = VarDictionary::new();
        self.pending_local_shot = None;
    }

    fn handling_state(&self) -> RidingState {
        RidingState {
            mounted: self.mounted,
            gait: gait_from_id(self.gait),
            planar_speed_mmps: meters_per_second_to_mmps(self.speed_mps),
            gait_top_speed_mmps: meters_per_second_to_mmps(self.gait_top_speed_mps).max(1),
            yaw_rate_millidegrees_per_second: degrees_to_millidegrees(self.yaw_rate_degrees),
            airborne: self.airborne,
            stumbling: self.stumbling,
            ads: self.ads,
            sprint_gallop: self.sprint_gallop,
        }
    }

    fn record_local_rejection(
        &mut self,
        tick: SimulationTick,
        origin: Vector3,
        direction: Vector3,
        reason: ShotRejectionReason,
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
            speed_mmps: riding.planar_speed_mmps,
            origin: origin_quantized,
            direction: direction_quantized,
            result: ShotOutcome::Reject,
            reject_reason: Some(reason),
            target_id: None,
            hit_zone: None,
            damage: 0,
            distance_mm: None,
        };
        self.last_shot_origin = finite_vector_or_zero(origin);
        self.last_shot_direction = finite_vector_or_zero(direction);
        self.last_reject_reason = GString::from(reason.as_str());
        self.last_telemetry = telemetry_dictionary(&telemetry);
        self.pending_local_shot = None;
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
    pub fn apply_authority_result(&mut self, result: &ShotResult) {
        if result.shooter_peer_id != self.kernel.shooter_peer_id() {
            return;
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
    }
}

fn default_shooter() -> PlayerId {
    PlayerId::parse(DEFAULT_SHOOTER_ID).expect("prototype shooter UUID is valid")
}

fn gait_from_id(value: i64) -> CombatGait {
    match value {
        1 => CombatGait::Walk,
        2 => CombatGait::Trot,
        // Existing M0 HorseController uses 3 for Gallop.
        3 => CombatGait::Gallop,
        // Optional explicit canter input for future animation integration.
        4 => CombatGait::Canter,
        _ => CombatGait::Idle,
    }
}

fn meters_per_second_to_mmps(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value * 1_000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32
    }
}

fn degrees_to_millidegrees(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else {
        (value * 1_000.0)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }
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

fn reload_error_name(error: ReloadStartError) -> &'static str {
    match error {
        ReloadStartError::TickReplay => "tick_replay",
        ReloadStartError::Holstered => "holstered",
        ReloadStartError::Dismounted => "dismounted",
        ReloadStartError::AlreadyReloading => "reloading",
        ReloadStartError::MagazineFull => "magazine_full",
        ReloadStartError::NoReserve => "no_reserve",
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
        assert_eq!(gait_from_id(0), CombatGait::Idle);
        assert_eq!(gait_from_id(1), CombatGait::Walk);
        assert_eq!(gait_from_id(2), CombatGait::Trot);
        assert_eq!(gait_from_id(3), CombatGait::Gallop);
        assert_eq!(gait_from_id(4), CombatGait::Canter);
        assert_eq!(gait_from_id(i64::MAX), CombatGait::Idle);
    }

    #[test]
    fn handling_quantization_sanitizes_nonfinite_values() {
        assert_eq!(meters_per_second_to_mmps(14.5), 14_500);
        assert_eq!(meters_per_second_to_mmps(f64::NAN), 0);
        assert_eq!(meters_per_second_to_mmps(-1.0), 0);
        assert_eq!(degrees_to_millidegrees(-60.5), -60_500);
        assert_eq!(degrees_to_millidegrees(f64::INFINITY), 0);
    }

    #[test]
    fn fixed_weapon_ids_have_distinct_visual_identity() {
        assert_eq!(weapon_color(WeaponId::Dustwalker), "tan_brown");
        assert_eq!(weapon_color(WeaponId::Longspur), "gunmetal_wood");
        assert_eq!(weapon_color(WeaponId::Rattler), "olive");
    }
}
