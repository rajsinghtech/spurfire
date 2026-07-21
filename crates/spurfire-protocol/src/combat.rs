//! Deterministic mounted-combat wire types and immutable weapon design rows.
//!
//! Wire vectors are integer-quantized so malformed IEEE-754 values cannot cross
//! the authority boundary. Gameplay code may convert to floating point only
//! after validating the quantized direction and origin bounds.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{PlayerId, RiderStance};

mod kernel;

pub use kernel::*;

/// Origin precision on the wire: one integer unit is one millimetre.
pub const ORIGIN_UNITS_PER_METER: f64 = 1_000.0;
/// Direction precision on the wire: one unit is one millionth of a unit vector.
pub const DIRECTION_UNITS: i32 = 1_000_000;
/// Accepted absolute unit-length error after direction quantization.
pub const DIRECTION_NORMALIZATION_EPSILON: f64 = 0.001;
/// Maximum distance between a submitted origin and the rewound authority muzzle.
pub const ORIGIN_LEASH_MM: u32 = 1_500;
/// Maximum interaction distance for a world weapon pickup.
pub const PICKUP_RANGE_MM: u32 = 3_000;
/// Authority rewind window from the design contract.
pub const ROLLBACK_WINDOW_MS: u32 = 250;
/// Maximum client-view rewind admitted for one authority shot. The longer
/// pose ring absorbs scheduling jitter but never grants more lag compensation.
pub const MAX_LAG_COMPENSATION_MS: u32 = 150;
/// Lifetime of a rifle dropped by a swap.
pub const DROPPED_WEAPON_LIFETIME_MS: u32 = 30_000;
/// Camera recoil recovery shared by every rifle.
pub const RECOIL_RECOVERY_MILLIDEGREES_PER_SECOND: u32 = 6_000;

/// A monotonically increasing fixed-step gameplay tick.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SimulationTick(u64);

impl SimulationTick {
    /// Creates a simulation tick.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer tick value.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Returns the number of ticks since `earlier`, if ordering is valid.
    #[must_use]
    pub const fn checked_duration_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }

    /// Adds ticks, saturating at the wire maximum.
    #[must_use]
    pub const fn saturating_add(self, ticks: u64) -> Self {
        Self(self.0.saturating_add(ticks))
    }
}

impl From<u64> for SimulationTick {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<SimulationTick> for u64 {
    fn from(value: SimulationTick) -> Self {
        value.as_u64()
    }
}

/// Stable entity identifier used for deterministic target ordering.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct EntityId(pub u64);

/// Stable team mask identifier. Targets on the shooter's team are excluded.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TeamId(pub u16);

/// The three fictional Spurfire rifle sidegrades.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum WeaponId {
    /// SF-C30 balanced carbine.
    #[default]
    Dustwalker = 0,
    /// SF-B18 heavy battle rifle.
    Longspur = 1,
    /// SF-R40 fast close-range rifle.
    Rattler = 2,
}

impl WeaponId {
    /// Every supported rifle in stable numeric order.
    pub const ALL: [Self; 3] = [Self::Dustwalker, Self::Longspur, Self::Rattler];

    /// Returns the stable compact numeric ID used by the Godot adapter.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Returns the short, wire-safe rifle name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dustwalker => "dustwalker",
            Self::Longspur => "longspur",
            Self::Rattler => "rattler",
        }
    }

    /// Returns the complete immutable weapon row.
    #[must_use]
    pub const fn stats(self) -> &'static WeaponStats {
        match self {
            Self::Dustwalker => &DUSTWALKER_STATS,
            Self::Longspur => &LONGSPUR_STATS,
            Self::Rattler => &RATTLER_STATS,
        }
    }
}

impl TryFrom<u8> for WeaponId {
    type Error = InvalidWeaponId;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dustwalker),
            1 => Ok(Self::Longspur),
            2 => Ok(Self::Rattler),
            other => Err(InvalidWeaponId(u64::from(other))),
        }
    }
}

impl TryFrom<i64> for WeaponId {
    type Error = InvalidWeaponId;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dustwalker),
            1 => Ok(Self::Longspur),
            2 => Ok(Self::Rattler),
            other => Err(InvalidWeaponId(other.cast_unsigned())),
        }
    }
}

impl fmt::Display for WeaponId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A numeric weapon ID absent from the fixed sidegrade table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("unknown weapon id {0}")]
pub struct InvalidWeaponId(pub u64);

/// Immutable fixed-point-friendly design values for one rifle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeaponStats {
    /// Stable weapon ID.
    pub id: WeaponId,
    /// Full fictional display name.
    pub display_name: &'static str,
    /// Magazine capacity in rounds.
    pub magazine_capacity: u16,
    /// Maximum reserve ammunition.
    pub reserve_capacity: u16,
    /// Rounds per second multiplied by 1,000.
    pub rounds_per_second_milli: u32,
    /// Reload duration in milliseconds.
    pub reload_ms: u32,
    /// Stationary spread in thousandths of a degree.
    pub base_spread_millidegrees: u32,
    /// Full-speed non-gallop spread in thousandths of a degree.
    pub moving_spread_millidegrees: u32,
    /// Full-speed gallop spread in thousandths of a degree.
    pub gallop_spread_millidegrees: u32,
    /// Vertical camera recoil per accepted shot.
    pub recoil_vertical_millidegrees: u32,
    /// Maximum deterministic random yaw recoil per accepted shot.
    pub recoil_yaw_millidegrees: u32,
    /// Body damage before falloff.
    pub body_damage: u16,
    /// Start of linear damage falloff.
    pub falloff_start_mm: u32,
    /// End of linear damage falloff.
    pub falloff_end_mm: u32,
    /// Body damage at and beyond the falloff end.
    pub minimum_body_damage: u16,
    /// Headshot multiplier multiplied by 1,000.
    pub headshot_multiplier_milli: u32,
    /// Intended effective range.
    pub effective_range_mm: u32,
    /// Hard hitscan clamp.
    pub hitscan_clamp_mm: u32,
}

impl WeaponStats {
    /// Minimum accepted interval for this weapon at `tick_rate`.
    ///
    /// The ceiling is intentional and prevents peers from accumulating
    /// floating-point cadence drift.
    #[must_use]
    pub fn cadence_ticks(self, tick_rate: u32) -> u64 {
        if tick_rate == 0 {
            return 0;
        }
        u64::from(tick_rate)
            .saturating_mul(1_000)
            .div_ceil(u64::from(self.rounds_per_second_milli))
    }

    /// Reload duration quantized upward to simulation ticks.
    #[must_use]
    pub fn reload_ticks(self, tick_rate: u32) -> u64 {
        if tick_rate == 0 {
            return 0;
        }
        u64::from(tick_rate)
            .saturating_mul(u64::from(self.reload_ms))
            .div_ceil(1_000)
    }

    /// Server-computed damage at a quantized distance and hit zone.
    #[must_use]
    pub fn damage_at(self, distance_mm: u32, hit_zone: HitZone) -> u16 {
        let body = if distance_mm <= self.falloff_start_mm {
            self.body_damage
        } else if distance_mm >= self.falloff_end_mm {
            self.minimum_body_damage
        } else {
            let distance_into_falloff = u64::from(distance_mm - self.falloff_start_mm);
            let falloff_span = u64::from(self.falloff_end_mm - self.falloff_start_mm);
            let distance_remaining = falloff_span - distance_into_falloff;
            let weighted_damage = u64::from(self.body_damage) * distance_remaining
                + u64::from(self.minimum_body_damage) * distance_into_falloff;
            u16::try_from((weighted_damage + falloff_span / 2) / falloff_span)
                .unwrap_or(self.minimum_body_damage)
        };

        match hit_zone {
            HitZone::Body => body,
            HitZone::Head => {
                let scaled = u64::from(body) * u64::from(self.headshot_multiplier_milli);
                u16::try_from((scaled + 500) / 1_000).unwrap_or(u16::MAX)
            }
        }
    }

    /// Rounds per second as a presentation-only float.
    #[must_use]
    pub fn rounds_per_second(self) -> f64 {
        f64::from(self.rounds_per_second_milli) / 1_000.0
    }

    /// Reload duration as a presentation-only float.
    #[must_use]
    pub fn reload_seconds(self) -> f64 {
        f64::from(self.reload_ms) / 1_000.0
    }
}

/// SF-C30 "Dustwalker" immutable design row.
pub const DUSTWALKER_STATS: WeaponStats = WeaponStats {
    id: WeaponId::Dustwalker,
    display_name: "SF-C30 'Dustwalker'",
    magazine_capacity: 30,
    reserve_capacity: 120,
    rounds_per_second_milli: 7_500,
    reload_ms: 2_100,
    base_spread_millidegrees: 800,
    moving_spread_millidegrees: 1_600,
    gallop_spread_millidegrees: 2_600,
    recoil_vertical_millidegrees: 550,
    recoil_yaw_millidegrees: 200,
    body_damage: 14,
    falloff_start_mm: 60_000,
    falloff_end_mm: 120_000,
    minimum_body_damage: 9,
    headshot_multiplier_milli: 2_000,
    effective_range_mm: 120_000,
    hitscan_clamp_mm: 300_000,
};

/// SF-B18 "Longspur" immutable design row.
pub const LONGSPUR_STATS: WeaponStats = WeaponStats {
    id: WeaponId::Longspur,
    display_name: "SF-B18 'Longspur'",
    magazine_capacity: 18,
    reserve_capacity: 72,
    rounds_per_second_milli: 4_000,
    reload_ms: 2_400,
    base_spread_millidegrees: 450,
    moving_spread_millidegrees: 1_000,
    gallop_spread_millidegrees: 1_800,
    recoil_vertical_millidegrees: 1_100,
    recoil_yaw_millidegrees: 350,
    body_damage: 26,
    falloff_start_mm: 120_000,
    falloff_end_mm: 200_000,
    minimum_body_damage: 17,
    headshot_multiplier_milli: 2_200,
    effective_range_mm: 200_000,
    hitscan_clamp_mm: 400_000,
};

/// SF-R40 "Rattler" immutable design row.
pub const RATTLER_STATS: WeaponStats = WeaponStats {
    id: WeaponId::Rattler,
    display_name: "SF-R40 'Rattler'",
    magazine_capacity: 40,
    reserve_capacity: 160,
    rounds_per_second_milli: 11_000,
    reload_ms: 2_600,
    base_spread_millidegrees: 1_400,
    moving_spread_millidegrees: 2_400,
    gallop_spread_millidegrees: 3_800,
    recoil_vertical_millidegrees: 350,
    recoil_yaw_millidegrees: 300,
    body_damage: 9,
    falloff_start_mm: 30_000,
    falloff_end_mm: 60_000,
    minimum_body_damage: 5,
    headshot_multiplier_milli: 1_800,
    effective_range_mm: 60_000,
    hitscan_clamp_mm: 150_000,
};

/// Integer-millimetre world-space origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuantizedOrigin {
    /// Right-positive coordinate in millimetres.
    pub x: i32,
    /// Up-positive coordinate in millimetres.
    pub y: i32,
    /// Back-positive coordinate in millimetres.
    pub z: i32,
}

impl QuantizedOrigin {
    /// Creates a raw integer origin.
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Quantizes finite metre coordinates to millimetres.
    pub fn from_meters(x: f64, y: f64, z: f64) -> Result<Self, VectorQuantizationError> {
        Ok(Self {
            x: quantize_component(x, ORIGIN_UNITS_PER_METER)?,
            y: quantize_component(y, ORIGIN_UNITS_PER_METER)?,
            z: quantize_component(z, ORIGIN_UNITS_PER_METER)?,
        })
    }

    /// Converts to presentation/simulation metres after wire validation.
    #[must_use]
    pub fn to_meters(self) -> [f64; 3] {
        [
            f64::from(self.x) / ORIGIN_UNITS_PER_METER,
            f64::from(self.y) / ORIGIN_UNITS_PER_METER,
            f64::from(self.z) / ORIGIN_UNITS_PER_METER,
        ]
    }

    /// Exact squared distance in square millimetres.
    #[must_use]
    pub fn squared_distance_mm(self, other: Self) -> u128 {
        let dx = i128::from(self.x) - i128::from(other.x);
        let dy = i128::from(self.y) - i128::from(other.y);
        let dz = i128::from(self.z) - i128::from(other.z);
        (dx * dx + dy * dy + dz * dz).unsigned_abs()
    }
}

/// Integer-quantized direction. Validation is deliberately separate from decoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuantizedDirection {
    /// Right-positive component in millionths.
    pub x: i32,
    /// Up-positive component in millionths.
    pub y: i32,
    /// Back-positive component in millionths.
    pub z: i32,
}

impl QuantizedDirection {
    /// Creates raw components, including malformed vectors for validation tests.
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Quantizes finite direction components without silently normalizing them.
    pub fn from_components(x: f64, y: f64, z: f64) -> Result<Self, VectorQuantizationError> {
        Ok(Self {
            x: quantize_component(x, f64::from(DIRECTION_UNITS))?,
            y: quantize_component(y, f64::from(DIRECTION_UNITS))?,
            z: quantize_component(z, f64::from(DIRECTION_UNITS))?,
        })
    }

    /// Returns whether the decoded vector is normalized within the fixed epsilon.
    #[must_use]
    pub fn is_normalized(self) -> bool {
        let [x, y, z] = self.to_components();
        let length = (x * x + y * y + z * z).sqrt();
        length.is_finite() && (length - 1.0).abs() <= DIRECTION_NORMALIZATION_EPSILON
    }

    /// Converts quantized components to floats after validation.
    #[must_use]
    pub fn to_components(self) -> [f64; 3] {
        let scale = f64::from(DIRECTION_UNITS);
        [
            f64::from(self.x) / scale,
            f64::from(self.y) / scale,
            f64::from(self.z) / scale,
        ]
    }
}

/// Failure converting an engine-space vector to its wire representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum VectorQuantizationError {
    /// At least one component was NaN or infinite.
    #[error("vector components must be finite")]
    NonFinite,
    /// A scaled component could not fit the signed 32-bit wire field.
    #[error("vector component exceeds quantized wire range")]
    OutOfRange,
}

fn quantize_component(value: f64, scale: f64) -> Result<i32, VectorQuantizationError> {
    if !value.is_finite() {
        return Err(VectorQuantizationError::NonFinite);
    }
    let scaled = (value * scale).round();
    if scaled < f64::from(i32::MIN) || scaled > f64::from(i32::MAX) {
        return Err(VectorQuantizationError::OutOfRange);
    }
    Ok(scaled as i32)
}

/// Client-provided target assertion. Every field is advisory and untrusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimedTarget {
    /// Claimed stable entity.
    pub target_id: EntityId,
    /// Claimed hit zone; authority geometry replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_zone: Option<HitZone>,
    /// Claimed damage; authority weapon rows replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage: Option<u16>,
    /// Claimed distance; authority geometry replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_mm: Option<u32>,
}

/// Fire request sent to the elected authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShotCommand {
    /// Shooter simulation tick.
    pub tick: SimulationTick,
    /// Stable shooter peer identity.
    pub shooter_peer_id: PlayerId,
    /// Rifle selected by the shooter.
    pub weapon_id: WeaponId,
    /// Quantized world-space muzzle origin.
    pub origin: QuantizedOrigin,
    /// Quantized, nominally normalized aim direction.
    pub direction: QuantizedDirection,
    /// Deterministic seed expected from lobby seed, shooter, and accepted-shot index.
    pub spread_seed: u64,
    /// Optional client prediction, never authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_target: Option<ClaimedTarget>,
}

/// Server-resolved target zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitZone {
    /// Torso/body capsule.
    Body,
    /// Head sphere.
    Head,
}

impl HitZone {
    /// Stable telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Head => "head",
        }
    }
}

/// Final authority disposition for one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotOutcome {
    /// A target was hit.
    Hit,
    /// A valid shot found no target inside the hitscan clamp.
    Miss,
    /// The command failed validation or weapon-state gates.
    Reject,
}

impl ShotOutcome {
    /// Stable telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Reject => "reject",
        }
    }
}

/// Stable reason for rejecting a shot command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum ShotRejectionReason {
    /// Shooter is not registered with this authority.
    #[error("unknown_shooter")]
    UnknownShooter,
    /// Rider snapshot identity or tick does not match the command.
    #[error("rider_snapshot")]
    RiderSnapshot,
    /// Command tick is not newer than this shooter's previous command.
    #[error("tick_replay")]
    TickReplay,
    /// Command predates the rollback window.
    #[error("tick_stale")]
    TickStale,
    /// Command is ahead of the authority simulation.
    #[error("tick_future")]
    TickFuture,
    /// Submitted weapon differs from authority equipment.
    #[error("weapon")]
    Weapon,
    /// Submitted spread seed differs from the authority-derived seed.
    #[error("spread_seed")]
    SpreadSeed,
    /// Fire cadence has not elapsed.
    #[error("rate")]
    Rate,
    /// Magazine has no rounds.
    #[error("empty")]
    Empty,
    /// Reload is active.
    #[error("reloading")]
    Reloading,
    /// Direction is not finite/normalized after quantization.
    #[error("invalid_direction")]
    InvalidDirection,
    /// Origin is farther than 1.5 metres from the rewound muzzle.
    #[error("origin_leash")]
    OriginLeash,
    /// Rifle is holstered.
    #[error("holstered")]
    Holstered,
    /// Rider is dismounted.
    #[error("dismounted")]
    Dismounted,
    /// Horse is airborne; M1 jumping fire is disabled.
    #[error("airborne")]
    Airborne,
    /// Rider or horse is in stumble recovery.
    #[error("stumble")]
    Stumble,
}

impl ShotRejectionReason {
    /// Stable lowercase value used by Godot telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownShooter => "unknown_shooter",
            Self::RiderSnapshot => "rider_snapshot",
            Self::TickReplay => "tick_replay",
            Self::TickStale => "tick_stale",
            Self::TickFuture => "tick_future",
            Self::Weapon => "weapon",
            Self::SpreadSeed => "spread_seed",
            Self::Rate => "rate",
            Self::Empty => "empty",
            Self::Reloading => "reloading",
            Self::InvalidDirection => "invalid_direction",
            Self::OriginLeash => "origin_leash",
            Self::Holstered => "holstered",
            Self::Dismounted => "dismounted",
            Self::Airborne => "airborne",
            Self::Stumble => "stumble",
        }
    }
}

/// Authority-computed result. No client-claimed hit or damage is copied here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShotResult {
    /// Command tick.
    pub tick: SimulationTick,
    /// Shooter identity.
    pub shooter_peer_id: PlayerId,
    /// Authority-equipped weapon.
    pub weapon_id: WeaponId,
    /// Hit, miss, or rejection.
    pub outcome: ShotOutcome,
    /// Present only for rejected commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<ShotRejectionReason>,
    /// Authority-derived spread direction for accepted shots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_direction: Option<QuantizedDirection>,
    /// Nearest valid authority target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<EntityId>,
    /// Authority-computed zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_zone: Option<HitZone>,
    /// Authority-computed damage.
    pub damage: u16,
    /// Quantized authority hit distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_mm: Option<u32>,
    /// Whether this hit reduced target health to zero.
    #[serde(default)]
    pub eliminated: bool,
}

/// Gait spelling carried by secret-free combat telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatGait {
    /// Stationary.
    #[default]
    Idle,
    /// Walking.
    Walk,
    /// Trotting.
    Trot,
    /// Cantering.
    Canter,
    /// Galloping.
    Gallop,
}

impl CombatGait {
    /// Stable lowercase telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Walk => "walk",
            Self::Trot => "trot",
            Self::Canter => "canter",
            Self::Gallop => "gallop",
        }
    }
}

/// Conservative stance used when loading pre-M2 persisted shot telemetry.
/// Unknown grants no fire, reload, target-geometry, or style capability.
#[must_use]
pub const fn legacy_shot_telemetry_stance() -> RiderStance {
    RiderStance::Unknown(RiderStance::UNKNOWN_ID)
}

/// Integer-only per-shot telemetry safe to persist or compare bit-for-bit.
///
/// Lobby seeds, OAuth material, join credentials, auth keys, and client claims
/// are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShotTelemetry {
    /// Shot tick.
    pub tick: SimulationTick,
    /// Shooter identity.
    pub shooter: PlayerId,
    /// Equipped rifle.
    pub weapon_id: WeaponId,
    /// Rounds remaining in the magazine.
    pub ammo_mag: u16,
    /// Rounds remaining in reserve.
    pub ammo_reserve: u16,
    /// Effective cone spread in thousandths of a degree.
    pub spread_millidegrees: u32,
    /// Absolute deterministic sway offset in thousandths of a degree.
    pub sway_millidegrees: u32,
    /// Rewound gait.
    pub gait: CombatGait,
    /// Rewound logical stance. Pre-M2 persisted rows default conservatively to
    /// unknown rather than inventing mounted capability.
    #[serde(default = "legacy_shot_telemetry_stance")]
    pub stance: RiderStance,
    /// Rewound planar speed in millimetres per second.
    pub speed_mmps: u32,
    /// Submitted origin.
    pub origin: QuantizedOrigin,
    /// Submitted or authority-resolved direction.
    pub direction: QuantizedDirection,
    /// Hit, miss, or rejection.
    pub result: ShotOutcome,
    /// Present only for a rejection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<ShotRejectionReason>,
    /// Authority target, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<EntityId>,
    /// Authority hit zone, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_zone: Option<HitZone>,
    /// Authority damage, zero for misses/rejections.
    pub damage: u16,
    /// Authority distance in millimetres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_mm: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shooter() -> PlayerId {
        PlayerId::parse("123e4567-e89b-42d3-a456-426614174000").unwrap()
    }

    #[test]
    fn weapon_rows_match_the_locked_design() {
        let dust = WeaponId::Dustwalker.stats();
        assert_eq!(dust.magazine_capacity, 30);
        assert_eq!(dust.reserve_capacity, 120);
        assert_eq!(dust.rounds_per_second_milli, 7_500);
        assert_eq!(dust.reload_ms, 2_100);
        assert_eq!(dust.base_spread_millidegrees, 800);
        assert_eq!(dust.moving_spread_millidegrees, 1_600);
        assert_eq!(dust.gallop_spread_millidegrees, 2_600);
        assert_eq!(dust.recoil_vertical_millidegrees, 550);
        assert_eq!(dust.recoil_yaw_millidegrees, 200);
        assert_eq!(dust.body_damage, 14);
        assert_eq!(dust.headshot_multiplier_milli, 2_000);
        assert_eq!(dust.effective_range_mm, 120_000);
        assert_eq!(dust.hitscan_clamp_mm, 300_000);

        let long = WeaponId::Longspur.stats();
        assert_eq!(long.magazine_capacity, 18);
        assert_eq!(long.reserve_capacity, 72);
        assert_eq!(long.rounds_per_second_milli, 4_000);
        assert_eq!(long.reload_ms, 2_400);
        assert_eq!(long.base_spread_millidegrees, 450);
        assert_eq!(long.moving_spread_millidegrees, 1_000);
        assert_eq!(long.gallop_spread_millidegrees, 1_800);
        assert_eq!(long.recoil_vertical_millidegrees, 1_100);
        assert_eq!(long.recoil_yaw_millidegrees, 350);
        assert_eq!(long.body_damage, 26);
        assert_eq!(long.headshot_multiplier_milli, 2_200);
        assert_eq!(long.effective_range_mm, 200_000);
        assert_eq!(long.hitscan_clamp_mm, 400_000);

        let rattler = WeaponId::Rattler.stats();
        assert_eq!(rattler.magazine_capacity, 40);
        assert_eq!(rattler.reserve_capacity, 160);
        assert_eq!(rattler.rounds_per_second_milli, 11_000);
        assert_eq!(rattler.reload_ms, 2_600);
        assert_eq!(rattler.base_spread_millidegrees, 1_400);
        assert_eq!(rattler.moving_spread_millidegrees, 2_400);
        assert_eq!(rattler.gallop_spread_millidegrees, 3_800);
        assert_eq!(rattler.recoil_vertical_millidegrees, 350);
        assert_eq!(rattler.recoil_yaw_millidegrees, 300);
        assert_eq!(rattler.body_damage, 9);
        assert_eq!(rattler.headshot_multiplier_milli, 1_800);
        assert_eq!(rattler.effective_range_mm, 60_000);
        assert_eq!(rattler.hitscan_clamp_mm, 150_000);
    }

    #[test]
    fn cadence_and_reload_quantize_up_at_supported_rates() {
        for (hz, expected) in [(30, [4, 8, 3]), (60, [8, 15, 6]), (120, [16, 30, 11])] {
            let actual = WeaponId::ALL.map(|id| id.stats().cadence_ticks(hz));
            assert_eq!(actual, expected, "{hz} Hz cadence");
        }
        for hz in [30, 60, 120] {
            for id in WeaponId::ALL {
                let stats = *id.stats();
                let ticks = stats.reload_ticks(hz);
                assert!(ticks * 1_000 >= u64::from(stats.reload_ms) * u64::from(hz));
                assert!((ticks - 1) * 1_000 < u64::from(stats.reload_ms) * u64::from(hz));
            }
        }
    }

    #[test]
    fn damage_falloff_and_headshots_are_server_rows() {
        assert_eq!(DUSTWALKER_STATS.damage_at(0, HitZone::Body), 14);
        assert_eq!(DUSTWALKER_STATS.damage_at(90_000, HitZone::Body), 12);
        assert_eq!(DUSTWALKER_STATS.damage_at(120_000, HitZone::Body), 9);
        assert_eq!(DUSTWALKER_STATS.damage_at(0, HitZone::Head), 28);
        assert_eq!(LONGSPUR_STATS.damage_at(0, HitZone::Head), 57);
        assert_eq!(RATTLER_STATS.damage_at(0, HitZone::Head), 16);
        assert!(WeaponId::ALL
            .iter()
            .all(|id| id.stats().damage_at(0, HitZone::Body) < 100));
    }

    #[test]
    fn quantized_vectors_reject_nonfinite_and_preserve_normalization() {
        let origin = QuantizedOrigin::from_meters(1.25, -2.5, 3.0).unwrap();
        assert_eq!(origin, QuantizedOrigin::new(1_250, -2_500, 3_000));
        assert_eq!(origin.to_meters(), [1.25, -2.5, 3.0]);
        assert_eq!(
            QuantizedOrigin::from_meters(f64::NAN, 0.0, 0.0),
            Err(VectorQuantizationError::NonFinite)
        );

        let direction = QuantizedDirection::from_components(0.0, 0.0, -1.0).unwrap();
        assert!(direction.is_normalized());
        assert!(!QuantizedDirection::new(0, 0, 0).is_normalized());
        assert!(!QuantizedDirection::new(0, 0, -500_000).is_normalized());
    }

    #[test]
    fn shot_command_result_and_telemetry_round_trip() {
        let command = ShotCommand {
            tick: SimulationTick::new(42),
            shooter_peer_id: shooter(),
            weapon_id: WeaponId::Longspur,
            origin: QuantizedOrigin::new(1_000, 2_000, 3_000),
            direction: QuantizedDirection::new(0, 0, -DIRECTION_UNITS),
            spread_seed: 0x1234_5678_9abc_def0,
            claimed_target: Some(ClaimedTarget {
                target_id: EntityId(99),
                hit_zone: Some(HitZone::Head),
                damage: Some(u16::MAX),
                distance_mm: Some(1),
            }),
        };
        let encoded = serde_json::to_string(&command).unwrap();
        assert_eq!(
            serde_json::from_str::<ShotCommand>(&encoded).unwrap(),
            command
        );

        let result = ShotResult {
            tick: command.tick,
            shooter_peer_id: command.shooter_peer_id,
            weapon_id: command.weapon_id,
            outcome: ShotOutcome::Hit,
            rejection_reason: None,
            resolved_direction: Some(command.direction),
            target_id: Some(EntityId(7)),
            hit_zone: Some(HitZone::Body),
            damage: 26,
            distance_mm: Some(12_345),
            eliminated: false,
        };
        let encoded = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<ShotResult>(&encoded).unwrap(),
            result
        );

        let telemetry = ShotTelemetry {
            tick: command.tick,
            shooter: command.shooter_peer_id,
            weapon_id: command.weapon_id,
            ammo_mag: 17,
            ammo_reserve: 72,
            spread_millidegrees: 450,
            sway_millidegrees: 0,
            gait: CombatGait::Idle,
            stance: RiderStance::Mounted,
            speed_mmps: 0,
            origin: command.origin,
            direction: command.direction,
            result: ShotOutcome::Hit,
            reject_reason: None,
            target_id: Some(EntityId(7)),
            hit_zone: Some(HitZone::Body),
            damage: 26,
            distance_mm: Some(12_345),
        };
        let encoded = serde_json::to_string(&telemetry).unwrap();
        assert!(!encoded.contains("spread_seed"));
        assert!(!encoded.contains("lobby_seed"));
        assert_eq!(
            serde_json::from_str::<ShotTelemetry>(&encoded).unwrap(),
            telemetry
        );

        let mut legacy = serde_json::to_value(&telemetry).unwrap();
        legacy
            .as_object_mut()
            .expect("telemetry is an object")
            .remove("stance");
        let decoded: ShotTelemetry = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            decoded.stance,
            RiderStance::Unknown(RiderStance::UNKNOWN_ID)
        );
    }

    #[test]
    fn origin_distance_uses_exact_integer_math() {
        let muzzle = QuantizedOrigin::new(0, 0, 0);
        assert_eq!(
            muzzle.squared_distance_mm(QuantizedOrigin::new(1_500, 0, 0)),
            u128::from(ORIGIN_LEASH_MM).pow(2)
        );
    }
}
