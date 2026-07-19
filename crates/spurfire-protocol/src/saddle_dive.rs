//! Deterministic M2 Saddle Dive, horse runout, style-event, and instrumentation kernels.
//!
//! Every gameplay timer is expressed in absolute [`SimulationTick`] values.
//! Wall-clock time, random numbers, and engine frame deltas are deliberately
//! absent. Engine adapters submit quantized input before movement and feed
//! collision-resolved observations back after movement.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CombatGait, EntityId, HitZone, PlayerId, QuantizedDirection, QuantizedOrigin, RiderStance,
    ShotOutcome, ShotResult, SimulationTick, WeaponId, DIRECTION_UNITS,
};

/// Production gameplay frequency shared by movement, combat, events, and snapshots.
pub const SADDLE_DIVE_TICK_RATE_HZ: u32 = 60;
/// Inclusive planar-speed threshold for a flying dismount.
pub const MIN_DIVE_SPEED_MMPS: u32 = 8_000;
/// Additive horizontal launch impulse.
pub const HORIZONTAL_LAUNCH_IMPULSE_MMPS: u32 = 6_000;
/// Upward launch velocity.
pub const VERTICAL_POP_MMPS: u32 = 6_000;
/// Shipped gravity.
pub const SADDLE_DIVE_GRAVITY_MMPS2: u32 = 22_000;
/// Design saddle height used by the analytic airtime contract.
pub const SADDLE_LAUNCH_HEIGHT_MM: i32 = 1_600;
/// Player choice is clamped to this half-angle from pre-launch velocity.
pub const LAUNCH_CONE_HALF_ANGLE_MILLIDEGREES: i32 = 75_000;
/// Positive ballistic root for 1.6 m, +6 m/s, and 22 m/s².
pub const ANALYTIC_AIRTIME_MICROSECONDS: u64 = 741_593;
/// Nominal production airtime after ceiling quantization.
pub const NOMINAL_AIRTIME_TICKS_60_HZ: u64 = 45;
/// Initial dive sway multiplier.
pub const DIVE_SWAY_BONUS_MILLI: u16 = 600;
/// Fraction at which the sway bonus begins decaying.
pub const DIVE_SWAY_DECAY_START_FRACTION_MILLI: u16 = 800;
/// Production decay start.
pub const DIVE_SWAY_DECAY_START_TICK_60_HZ: u64 = 36;
/// Normal no-input landing phase at 60 Hz.
pub const LANDING_PRONE_TICKS_60_HZ: u64 = 24;
/// Half-speed landing phase at 60 Hz.
pub const LANDING_RECOVERY_TICKS_60_HZ: u64 = 24;
/// Extra no-input phase for a bad landing at 60 Hz.
pub const BAD_LANDING_EXTRA_TICKS_60_HZ: u64 = 24;
/// A slope strictly above this angle is bad.
pub const BAD_LANDING_THRESHOLD_MILLIDEGREES: u32 = 30_000;
/// Deterministic bad-landing damage.
pub const BAD_LANDING_DAMAGE: u16 = 15;
/// Inclusive post-landing observation window at 60 Hz.
pub const DEATH_OBSERVATION_TICKS_60_HZ: u64 = 180;
/// Horse runout duration at 60 Hz.
pub const HORSE_RUNOUT_TICKS_60_HZ: u64 = 120;
/// Maximum cumulative collision-resolved runout travel.
pub const HORSE_MAX_TRAVEL_MM: u32 = 25_000;
/// Stationary remount range.
pub const REMOUNT_RANGE_MM: u32 = 3_000;
/// Full movement input multiplier.
pub const MOVEMENT_SCALE_FULL_MILLI: u16 = 1_000;
/// Disabled movement input multiplier.
pub const MOVEMENT_SCALE_PRONE_MILLI: u16 = 0;
/// Recovery movement input multiplier.
pub const MOVEMENT_SCALE_RECOVERY_MILLI: u16 = 500;
/// Prototype rider spawn health.
pub const PROTOTYPE_RIDER_HEALTH: u16 = 100;

const NANODEGREES_PER_MILLIDEGREE: i64 = 1_000_000;
const NANODEGREES_180: i64 = 180_000_000_000;
const CORDIC_SCALE: i128 = 1_000_000_000_000;
const CORDIC_GAIN_INVERSE: i128 = 607_252_935_009;
const NORMALIZED_LOWER_UNITS: i128 = 999_000;
const NORMALIZED_UPPER_UNITS: i128 = 1_001_000;
const CORDIC_ANGLES_NANODEGREES: [i64; 37] = [
    45_000_000_000,
    26_565_051_177,
    14_036_243_468,
    7_125_016_349,
    3_576_334_375,
    1_789_910_608,
    895_173_710,
    447_614_171,
    223_810_500,
    111_905_677,
    55_952_892,
    27_976_453,
    13_988_227,
    6_994_114,
    3_497_057,
    1_748_528,
    874_264,
    437_132,
    218_566,
    109_283,
    54_642,
    27_321,
    13_660,
    6_830,
    3_415,
    1_708,
    854,
    427,
    213,
    107,
    53,
    27,
    13,
    7,
    3,
    2,
    1,
];

/// Stable nonzero identifier allocated once per accepted dive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DiveId(NonZeroU64);

impl DiveId {
    /// Builds a valid dive ID.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the numeric ID used by event sequences and adapters.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Converts an integer duration upward to fixed simulation ticks.
#[must_use]
pub const fn duration_ticks_ceil(tick_rate: u32, milliseconds: u32) -> u64 {
    if tick_rate == 0 {
        return 0;
    }
    (tick_rate as u64)
        .saturating_mul(milliseconds as u64)
        .div_ceil(1_000)
}

/// Nominal ballistic duration at a fixed simulation frequency.
#[must_use]
pub const fn nominal_airtime_ticks(tick_rate: u32) -> u64 {
    if tick_rate == 0 {
        return 0;
    }
    (tick_rate as u64)
        .saturating_mul(ANALYTIC_AIRTIME_MICROSECONDS)
        .div_ceil(1_000_000)
}

/// Tick at which the final-20-percent sway decay begins.
#[must_use]
pub const fn sway_decay_start_tick(nominal_ticks: u64) -> u64 {
    nominal_ticks.saturating_mul(4) / 5
}

/// Deterministic 600..1000 dive sway multiplier.
#[must_use]
pub const fn dive_sway_scale_milli(elapsed_ticks: u64, nominal_ticks: u64) -> u16 {
    let decay_start = sway_decay_start_tick(nominal_ticks);
    if elapsed_ticks <= decay_start {
        return DIVE_SWAY_BONUS_MILLI;
    }
    if elapsed_ticks >= nominal_ticks || nominal_ticks <= decay_start {
        return 1_000;
    }
    let numerator = 400_u64.saturating_mul(elapsed_ticks - decay_start);
    let denominator = nominal_ticks - decay_start;
    let scale = 600_u64.saturating_add(numerator / denominator);
    if scale > 1_000 {
        1_000
    } else {
        scale as u16
    }
}

/// Vertical velocity requested for an airborne tick offset.
#[must_use]
pub fn airborne_vertical_velocity_mmps(elapsed_ticks: u64, tick_rate: u32) -> i32 {
    if tick_rate == 0 {
        return VERTICAL_POP_MMPS as i32;
    }
    let gravity_numerator =
        i128::from(SADDLE_DIVE_GRAVITY_MMPS2).saturating_mul(i128::from(elapsed_ticks));
    let gravity_delta = div_round_half_away(gravity_numerator, i128::from(tick_rate));
    saturating_i128_to_i32(i128::from(VERTICAL_POP_MMPS) - gravity_delta)
}

/// Per-weapon accepted-shot cap for one dive.
#[must_use]
pub const fn dive_shot_cap(weapon_id: WeaponId) -> u16 {
    match weapon_id {
        WeaponId::Longspur => 1,
        WeaponId::Dustwalker => 3,
        WeaponId::Rattler => 5,
    }
}

/// M2 rider state machine state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaddleDiveState {
    /// Rider is logically attached to the horse.
    #[default]
    Mounted,
    /// Accepted flying dismount is airborne.
    SaddleDiveAirborne,
    /// Landing no-input phase.
    LandingProne,
    /// Landing half-speed phase.
    LandingRecovery,
    /// Dismounted and ready to retrieve an idle horse.
    OnFootReady,
}

/// Terrain label recorded at first valid landing contact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingTerrain {
    /// Authored flat/open ground.
    Flat,
    /// Scrub or rough vegetation.
    Scrub,
    /// Mud.
    Mud,
    /// Dry or wet riverbed.
    Riverbed,
    /// Adapter could not classify the material.
    #[default]
    Unknown,
}

/// Quantized landing classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingOutcome {
    /// Slope is at or below 30 degrees.
    Good,
    /// Slope is strictly above 30 degrees.
    Bad,
}

/// Why an instrumentation row ended without a normal remount/death-window completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiveCensorReason {
    /// Rider died before any landing was observed.
    DiedAirborne,
    /// Rider died after landing and before remounting.
    DiedBeforeRemount,
    /// Match ended while the row was open.
    MatchEnded,
    /// Course reset invalidated the normal retrieval path.
    Reset,
    /// Observation collection stopped without a terminal observation.
    NotObserved,
}

/// Complete secret-free per-dive instrumentation row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiveInstrumentationRow {
    /// Schema revision for persisted analysis.
    pub schema_version: u16,
    /// Authority epoch captured at launch; zero denotes the offline prototype.
    pub authority_epoch: u64,
    /// Rider identity.
    pub actor: PlayerId,
    /// Nonzero dive identity.
    pub dive_id: DiveId,
    /// Accepted launch tick.
    pub launch_tick: SimulationTick,
    /// Weapon locked at launch.
    pub launch_weapon: WeaponId,
    /// Gait captured at launch.
    pub launch_gait: CombatGait,
    /// Planar pre-launch velocity as `[x, z]`.
    pub prelaunch_velocity_mmps: [i32; 2],
    /// Rounded planar launch speed.
    pub prelaunch_speed_mmps: u32,
    /// Projected/fallback player request.
    pub requested_direction: QuantizedDirection,
    /// Signed request angle from pre-launch velocity.
    pub requested_angle_millidegrees: i32,
    /// Direction after cone clamping.
    pub clamped_direction: QuantizedDirection,
    /// Signed clamped angle.
    pub clamped_angle_millidegrees: i32,
    /// Whether the request crossed either cone boundary.
    pub direction_was_clamped: bool,
    /// Additive horizontal impulse magnitude.
    pub horizontal_impulse_mmps: u32,
    /// Resulting rounded planar velocity magnitude.
    pub resulting_planar_speed_mmps: u32,
    /// Resulting rounded total velocity magnitude.
    pub resulting_total_speed_mmps: u32,
    /// Vertical pop magnitude.
    pub vertical_pop_mmps: u32,
    /// Observed rider-minus-horse launch height.
    pub launch_height_mm: i32,
    /// Nominal ballistic ticks used by the sway schedule.
    pub nominal_airtime_ticks: u64,
    /// First valid landing tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_tick: Option<SimulationTick>,
    /// Actual landing tick minus launch tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub airtime_ticks: Option<u64>,
    /// Valid airborne trigger pulls.
    pub shot_attempts: u16,
    /// Ammo-consuming shots accepted by combat.
    pub shots_fired: u16,
    /// Authority-confirmed hits.
    pub shots_hit: u16,
    /// Authority-confirmed headshots.
    pub headshots: u16,
    /// Authority-confirmed strict behind-direction hits.
    pub reversal_hits: u16,
    /// Authority-confirmed damage dealt.
    pub damage_dealt: u32,
    /// Landing material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_terrain: Option<LandingTerrain>,
    /// Quantized nearest-millidegree slope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_slope_millidegrees: Option<u32>,
    /// Good/bad threshold result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_outcome: Option<LandingOutcome>,
    /// Deterministic landing damage (zero or 15).
    pub landing_damage: u16,
    /// Damage taken in the inclusive landing-through-three-seconds window.
    pub damage_taken_landing_through_3s: u32,
    /// First observed death after launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub death_tick: Option<SimulationTick>,
    /// `None` until the inclusive death window is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub death_within_3s: Option<bool>,
    /// Successful stationary remount tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remount_tick: Option<SimulationTick>,
    /// Landing-to-remount duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_remount_ticks: Option<u64>,
    /// Why normal observation could not complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub censor_reason: Option<DiveCensorReason>,
}

/// Stable gameplay notification kind. M5 may assign points later; M2 does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameplayEventKind {
    /// Accepted transition into a flying dismount.
    FlyingDismount,
    /// Dive shot later resolved as a headshot.
    SaddleDiveHeadshot,
    /// Grounded mounted Gallop shot later resolved as a hit.
    FullGallopHit,
    /// Dive hit aimed strictly behind pre-launch planar velocity.
    AirborneReversal,
}

impl GameplayEventKind {
    /// Exact presentation text.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::FlyingDismount => "FLYING DISMOUNT",
            Self::SaddleDiveHeadshot => "SADDLE DIVE HEADSHOT",
            Self::FullGallopHit => "FULL-GALLOP HIT",
            Self::AirborneReversal => "AIRBORNE REVERSAL",
        }
    }
}

/// Deterministic, replay-safe event identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GameplayEventId {
    /// Authority epoch; zero for offline play.
    pub authority_epoch: u64,
    /// Actor receiving style credit.
    pub actor: PlayerId,
    /// Original launch/shot tick.
    pub source_tick: SimulationTick,
    /// Notification kind.
    pub kind: GameplayEventKind,
    /// Dive ID for launch or authority accepted-shot index for shot events.
    pub sequence: u64,
}

/// Secret-free notification row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GameplayEventRow {
    /// Stable deduplication identity.
    pub id: GameplayEventId,
    /// Event kind.
    pub kind: GameplayEventKind,
    /// Source tick repeated for convenient logs.
    pub tick: SimulationTick,
    /// Actor.
    pub actor: PlayerId,
    /// Dive attribution when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dive_id: Option<DiveId>,
    /// Weapon attribution when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon_id: Option<WeaponId>,
    /// Authority target when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<EntityId>,
    /// Authority zone when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_zone: Option<HitZone>,
    /// Authority damage; zero for launch.
    pub damage: u16,
    /// Exact uppercase presentation string.
    pub text: &'static str,
}

/// Authority-owned metadata retained for late result attribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedShotMetadata {
    /// Shooter identity.
    pub shooter: PlayerId,
    /// Original command tick.
    pub tick: SimulationTick,
    /// Match-lifetime accepted-shot index.
    pub accepted_shot_index: u64,
    /// Authority-equipped weapon.
    pub weapon_id: WeaponId,
    /// Shot-time stance.
    pub stance: RiderStance,
    /// Shot-time gait.
    pub gait: CombatGait,
    /// Authority-owned dive attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dive_id: Option<DiveId>,
    /// Planar velocity captured when the attributed dive launched.
    pub prelaunch_horizontal_velocity_mmps: [i32; 2],
}

impl AcceptedShotMetadata {
    /// Whether stance and dive attribution obey the authority invariant.
    #[must_use]
    pub const fn is_consistent(self) -> bool {
        matches!(self.stance, RiderStance::SaddleDiveAirborne) == self.dive_id.is_some()
            && matches!(
                self.stance,
                RiderStance::Mounted | RiderStance::SaddleDiveAirborne
            )
    }
}

/// Result attribution returned by [`ShotAttributionLedger`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotResultAttribution {
    /// Authority result supplied to the ledger.
    pub result: ShotResult,
    /// Stored accepted-shot row, if the result was linkable.
    pub accepted_shot: Option<AcceptedShotMetadata>,
    /// True when this `(shooter, tick)` result had already been processed.
    pub duplicate: bool,
    /// Newly emitted style rows, in stable kind order.
    pub events: Vec<GameplayEventRow>,
}

/// Match-lifetime accepted-shot/result ledger used for late attribution and deduplication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShotAttributionLedger {
    accepted: BTreeMap<(PlayerId, SimulationTick), AcceptedShotMetadata>,
    resolved: BTreeSet<(PlayerId, SimulationTick)>,
}

impl ShotAttributionLedger {
    /// Stores one authority-accepted shot. Duplicate shooter/tick rows and
    /// inconsistent stance claims are rejected without mutation.
    pub fn record_accepted(&mut self, shot: AcceptedShotMetadata) -> bool {
        if !shot.is_consistent() {
            return false;
        }
        let key = (shot.shooter, shot.tick);
        if self.accepted.contains_key(&key) {
            return false;
        }
        self.accepted.insert(key, shot);
        true
    }

    /// Returns retained metadata for an accepted shooter/tick.
    #[must_use]
    pub fn accepted(
        &self,
        shooter: PlayerId,
        tick: SimulationTick,
    ) -> Option<AcceptedShotMetadata> {
        self.accepted.get(&(shooter, tick)).copied()
    }

    /// Applies one authority result exactly once and derives M2 hit notifications.
    pub fn observe_result(
        &mut self,
        authority_epoch: u64,
        result: &ShotResult,
    ) -> ShotResultAttribution {
        let key = (result.shooter_peer_id, result.tick);
        let accepted = self.accepted.get(&key).copied().filter(|shot| {
            shot.weapon_id == result.weapon_id && result.outcome != ShotOutcome::Reject
        });
        let duplicate = accepted.is_some() && !self.resolved.insert(key);
        let mut events = Vec::new();
        if !duplicate && result.outcome == ShotOutcome::Hit {
            if let Some(shot) = accepted {
                if shot.dive_id.is_some() && result.hit_zone == Some(HitZone::Head) {
                    events.push(shot_event(
                        authority_epoch,
                        shot,
                        result,
                        GameplayEventKind::SaddleDiveHeadshot,
                    ));
                }
                if shot.stance == RiderStance::Mounted && shot.gait == CombatGait::Gallop {
                    events.push(shot_event(
                        authority_epoch,
                        shot,
                        result,
                        GameplayEventKind::FullGallopHit,
                    ));
                }
                if shot.dive_id.is_some()
                    && result.resolved_direction.is_some_and(|direction| {
                        horizontal_dot(
                            shot.prelaunch_horizontal_velocity_mmps,
                            [direction.x, direction.z],
                        ) < 0
                    })
                {
                    events.push(shot_event(
                        authority_epoch,
                        shot,
                        result,
                        GameplayEventKind::AirborneReversal,
                    ));
                }
            }
        }
        ShotResultAttribution {
            result: result.clone(),
            accepted_shot: accepted,
            duplicate,
            events,
        }
    }
}

fn shot_event(
    authority_epoch: u64,
    shot: AcceptedShotMetadata,
    result: &ShotResult,
    kind: GameplayEventKind,
) -> GameplayEventRow {
    GameplayEventRow {
        id: GameplayEventId {
            authority_epoch,
            actor: shot.shooter,
            source_tick: shot.tick,
            kind,
            sequence: shot.accepted_shot_index,
        },
        kind,
        tick: shot.tick,
        actor: shot.shooter,
        dive_id: shot.dive_id,
        weapon_id: Some(shot.weapon_id),
        target_id: result.target_id,
        hit_zone: result.hit_zone,
        damage: result.damage,
        text: kind.text(),
    }
}

/// One accepted state transition, including calculated boundary tick after a jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaddleDiveTransition {
    /// Previous state.
    pub from: SaddleDiveState,
    /// New state.
    pub to: SaddleDiveState,
    /// Exact source or timer-boundary tick.
    pub tick: SimulationTick,
    /// Dive associated with the transition.
    pub dive_id: Option<DiveId>,
}

/// Replay-safe bad-landing damage command identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RiderDamageCommandId {
    /// Authority epoch at launch.
    pub authority_epoch: u64,
    /// Damaged rider.
    pub actor: PlayerId,
    /// Dive producing the landing.
    pub dive_id: DiveId,
    /// First landing tick.
    pub landing_tick: SimulationTick,
}

/// Deterministic rider damage command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RiderDamageCommand {
    /// Idempotency key.
    pub id: RiderDamageCommandId,
    /// Always 15 for M2 bad landing.
    pub amount: u16,
    /// Source tick.
    pub tick: SimulationTick,
    /// Stable secret-free source spelling.
    pub source: &'static str,
}

/// Deterministic side effects for adapters to execute without re-deriving eligibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaddleDiveCommand {
    /// Keep the same horse object and start deterministic runout.
    StartHorseRunout {
        /// Dive causing runout.
        dive_id: DiveId,
        /// Launch tick.
        tick: SimulationTick,
        /// Collision-resolved horse position at launch.
        horse_position: QuantizedOrigin,
        /// Captured planar horse velocity (`y` is zero).
        horse_velocity_mmps: [i32; 3],
    },
    /// Detach the logical rider while preserving world position.
    DetachRider {
        /// `None` for an ordinary low-speed dismount.
        dive_id: Option<DiveId>,
        /// Transition tick.
        tick: SimulationTick,
        /// Launch velocity for a dive, or zero for ordinary dismount.
        launch_velocity_mmps: [i32; 3],
    },
    /// Apply first-contact bad-landing damage exactly once.
    ApplyRiderDamage(RiderDamageCommand),
    /// Attach to the existing horse without moving it to the rider.
    AttachRider {
        /// Remount tick.
        tick: SimulationTick,
        /// Existing horse position.
        horse_position: QuantizedOrigin,
    },
}

/// Shared effects emitted by tick, motion, and observation methods.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaddleDiveEffects {
    /// Ordered state transitions.
    pub transitions: Vec<SaddleDiveTransition>,
    /// Ordered deterministic adapter commands.
    pub commands: Vec<SaddleDiveCommand>,
    /// Newly emitted notification rows.
    pub events: Vec<GameplayEventRow>,
    /// Updated instrumentation snapshots.
    pub telemetry_updates: Vec<DiveInstrumentationRow>,
    /// Rows finalized exactly once.
    pub telemetry_finalized: Vec<DiveInstrumentationRow>,
}

/// Output policy and motion request for one begun tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaddleDiveTickOutput {
    /// Begun tick.
    pub tick: SimulationTick,
    /// Current state after timer/input transitions.
    pub state: SaddleDiveState,
    /// Wire stance derived from current state and horse floor state.
    pub stance: RiderStance,
    /// Current dive, if the gameplay state is dive-owned.
    pub dive_id: Option<DiveId>,
    /// Movement-input multiplier in thousandths.
    pub movement_input_scale_milli: u16,
    /// Whether combat fire is legal before post-move landing for this tick.
    pub can_fire: bool,
    /// Whether reload is legal.
    pub can_reload: bool,
    /// Dive sway multiplier; 1000 outside the dive.
    pub sway_scale_milli: u16,
    /// Collision request for a detached airborne rider.
    pub requested_rider_velocity_mmps: Option<[i32; 3]>,
    /// Whether this kernel consumed the E edge.
    pub interact_consumed: bool,
    /// Deterministic effects generated while beginning this tick.
    pub effects: SaddleDiveEffects,
}

/// Pre-move input sampled once for an absolute gameplay tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaddleDiveTickInput {
    /// Absolute shared gameplay tick.
    pub tick: SimulationTick,
    /// Raw E level; the kernel derives a rising edge to defend against held input.
    pub interact_pressed: bool,
    /// Quantized camera forward. `None` represents a non-finite conversion.
    pub chosen_direction: Option<QuantizedDirection>,
    /// Post-move horse floor state from the preceding tick.
    pub horse_grounded: bool,
    /// Collision-resolved horse position.
    pub horse_position: QuantizedOrigin,
    /// Collision-resolved horse velocity.
    pub horse_velocity_mmps: [i32; 3],
    /// Authoritative launch-time gait.
    pub horse_gait: CombatGait,
    /// Currently equipped weapon.
    pub equipped_weapon: WeaponId,
    /// Logical rider position.
    pub rider_position: QuantizedOrigin,
    /// Whether the same horse is idle and retrievable.
    pub horse_retrievable: bool,
    /// Bound authority epoch.
    pub authority_epoch: u64,
    /// Bound actor.
    pub actor: PlayerId,
}

/// Post-move collision observation accepted at most once for the begun tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiderMotionObservation {
    /// Must equal the current begun tick.
    pub tick: SimulationTick,
    /// Collision-resolved rider position.
    pub rider_position: QuantizedOrigin,
    /// Collision-resolved rider velocity.
    pub rider_velocity_mmps: [i32; 3],
    /// Whether the rider was descending into this contact.
    pub descending: bool,
    /// Most floor-like quantized upward normal selected by the adapter.
    pub landing_normal: Option<QuantizedDirection>,
    /// Material classification.
    pub landing_terrain: LandingTerrain,
}

/// Identity for one externally applied damage aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DamageObservationId {
    /// Authority epoch producing the observation.
    pub authority_epoch: u64,
    /// Damaged actor.
    pub actor: PlayerId,
    /// Original damage tick.
    pub tick: SimulationTick,
    /// Authority-unique sequence within the tick.
    pub sequence: u64,
}

/// External damage observation used by the rider-health and dive telemetry sinks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageObservation {
    /// Replay key.
    pub id: DamageObservationId,
    /// Damage amount.
    pub amount: u16,
}

/// Result of applying deterministic health damage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiderHealthApplication {
    /// Health before this unique command/observation.
    pub health_before: u16,
    /// Health after saturating subtraction.
    pub health_after: u16,
    /// Whether this application crossed from alive to dead.
    pub died: bool,
}

/// Minimal deterministic rider-health sink for M2 landing and instrumentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiderHealthKernel {
    health: u16,
    landing_commands: BTreeSet<RiderDamageCommandId>,
    external_observations: BTreeSet<DamageObservationId>,
}

impl Default for RiderHealthKernel {
    fn default() -> Self {
        Self {
            health: PROTOTYPE_RIDER_HEALTH,
            landing_commands: BTreeSet::new(),
            external_observations: BTreeSet::new(),
        }
    }
}

impl RiderHealthKernel {
    /// Current prototype health.
    #[must_use]
    pub const fn health(&self) -> u16 {
        self.health
    }

    /// Whether health is zero.
    #[must_use]
    pub const fn is_dead(&self) -> bool {
        self.health == 0
    }

    /// Applies a bad-landing command at most once.
    pub fn apply_landing_command(
        &mut self,
        command: RiderDamageCommand,
    ) -> Option<RiderHealthApplication> {
        if !self.landing_commands.insert(command.id) {
            return None;
        }
        Some(self.apply_amount(command.amount))
    }

    /// Applies one external authority aggregate at most once.
    pub fn apply_external(
        &mut self,
        observation: DamageObservation,
    ) -> Option<RiderHealthApplication> {
        if !self.external_observations.insert(observation.id) {
            return None;
        }
        Some(self.apply_amount(observation.amount))
    }

    /// Marks an externally observed death without inventing a damage amount.
    pub fn observe_death(&mut self) -> bool {
        if self.health == 0 {
            return false;
        }
        self.health = 0;
        true
    }

    /// Explicit course reset; this is not a normal retrieval path. Replay keys
    /// remain match-lifetime so an old authority observation cannot damage the
    /// reset life a second time.
    pub fn reset(&mut self) {
        self.health = PROTOTYPE_RIDER_HEALTH;
    }

    fn apply_amount(&mut self, amount: u16) -> RiderHealthApplication {
        let health_before = self.health;
        self.health = self.health.saturating_sub(amount);
        RiderHealthApplication {
            health_before,
            health_after: self.health,
            died: health_before > 0 && self.health == 0,
        }
    }
}

/// Saddle Dive API rejection with no state mutation for malformed ordering/context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum SaddleDiveError {
    /// Tick rate was zero.
    #[error("invalid_tick_rate")]
    InvalidTickRate,
    /// `begin_tick` repeated or regressed.
    #[error("tick_replay")]
    TickReplay,
    /// Motion was submitted without a matching begun tick.
    #[error("tick_mismatch")]
    TickMismatch,
    /// A second post-move observation was submitted for one tick.
    #[error("motion_already_resolved")]
    MotionAlreadyResolved,
    /// Input actor differs from the bound actor.
    #[error("actor_mismatch")]
    ActorMismatch,
    /// Input epoch differs from the bound authority epoch.
    #[error("authority_epoch_mismatch")]
    AuthorityEpochMismatch,
    /// The nonzero dive ID space was exhausted.
    #[error("dive_id_exhausted")]
    DiveIdExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiveRecord {
    row: DiveInstrumentationRow,
    attempt_ticks: BTreeSet<SimulationTick>,
    accepted_shots: BTreeMap<SimulationTick, AcceptedShotMetadata>,
    resolved_shots: BTreeSet<SimulationTick>,
    attributed_damage_observations: BTreeSet<DamageObservationId>,
    finalized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AirborneState {
    dive_id: DiveId,
    launch_tick: SimulationTick,
    launch_velocity_mmps: [i32; 3],
    resolved_horizontal_velocity_mmps: [i32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LandingState {
    dive_id: DiveId,
    landing_tick: SimulationTick,
    bad: bool,
}

/// Pure absolute-tick Saddle Dive state machine and instrumentation sink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaddleDiveKernel {
    tick_rate: u32,
    actor: PlayerId,
    authority_epoch: u64,
    current_tick: Option<SimulationTick>,
    motion_resolved: bool,
    previous_interact_level: bool,
    state: SaddleDiveState,
    state_enter_tick: SimulationTick,
    state_dive_id: Option<DiveId>,
    mounted_horse_grounded: bool,
    next_dive_id: Option<NonZeroU64>,
    airborne: Option<AirborneState>,
    landing: Option<LandingState>,
    rider_position: QuantizedOrigin,
    health: RiderHealthKernel,
    damage_observations: BTreeMap<DamageObservationId, u16>,
    observation_watermark: Option<SimulationTick>,
    rows: BTreeMap<DiveId, DiveRecord>,
}

impl SaddleDiveKernel {
    /// Creates a kernel bound to one actor and one authority epoch.
    pub fn new(
        tick_rate: u32,
        actor: PlayerId,
        authority_epoch: u64,
    ) -> Result<Self, SaddleDiveError> {
        if tick_rate == 0 {
            return Err(SaddleDiveError::InvalidTickRate);
        }
        Ok(Self {
            tick_rate,
            actor,
            authority_epoch,
            current_tick: None,
            motion_resolved: false,
            previous_interact_level: false,
            state: SaddleDiveState::Mounted,
            state_enter_tick: SimulationTick::new(0),
            state_dive_id: None,
            mounted_horse_grounded: true,
            next_dive_id: NonZeroU64::new(1),
            airborne: None,
            landing: None,
            rider_position: QuantizedOrigin::default(),
            health: RiderHealthKernel::default(),
            damage_observations: BTreeMap::new(),
            observation_watermark: None,
            rows: BTreeMap::new(),
        })
    }

    /// Fixed tick rate.
    #[must_use]
    pub const fn tick_rate(&self) -> u32 {
        self.tick_rate
    }

    /// Bound actor.
    #[must_use]
    pub const fn actor(&self) -> PlayerId {
        self.actor
    }

    /// Bound authority epoch.
    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    /// Advances the authority epoch without discarding match-lifetime movement
    /// and dive instrumentation. Migration never permits an epoch rollback.
    pub fn set_authority_epoch(&mut self, authority_epoch: u64) -> bool {
        if authority_epoch < self.authority_epoch {
            return false;
        }
        self.authority_epoch = authority_epoch;
        true
    }

    /// Most recently begun tick.
    #[must_use]
    pub const fn current_tick(&self) -> Option<SimulationTick> {
        self.current_tick
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> SaddleDiveState {
        self.state
    }

    /// Current wire stance.
    #[must_use]
    pub const fn stance(&self) -> RiderStance {
        match self.state {
            SaddleDiveState::Mounted if self.mounted_horse_grounded => RiderStance::Mounted,
            SaddleDiveState::Mounted => RiderStance::MountedAirborne,
            SaddleDiveState::SaddleDiveAirborne => RiderStance::SaddleDiveAirborne,
            SaddleDiveState::LandingProne => RiderStance::LandingProne,
            SaddleDiveState::LandingRecovery => RiderStance::LandingRecovery,
            SaddleDiveState::OnFootReady => RiderStance::OnFootStanding,
        }
    }

    /// Dive associated with the current gameplay state.
    #[must_use]
    pub const fn current_dive_id(&self) -> Option<DiveId> {
        self.state_dive_id
    }

    /// Current rider health.
    #[must_use]
    pub const fn rider_health(&self) -> u16 {
        self.health.health()
    }

    /// Read one retained instrumentation row.
    #[must_use]
    pub fn instrumentation_row(&self, dive_id: DiveId) -> Option<&DiveInstrumentationRow> {
        self.rows.get(&dive_id).map(|record| &record.row)
    }

    /// Iterates retained rows in stable dive-ID order.
    pub fn instrumentation_rows(&self) -> impl Iterator<Item = &DiveInstrumentationRow> {
        self.rows.values().map(|record| &record.row)
    }

    /// Begins one strictly newer absolute tick, crosses every timer boundary,
    /// then applies the rising E edge in context-priority order.
    pub fn begin_tick(
        &mut self,
        input: SaddleDiveTickInput,
    ) -> Result<SaddleDiveTickOutput, SaddleDiveError> {
        self.validate_identity(input.actor, input.authority_epoch)?;
        if self
            .current_tick
            .is_some_and(|current| input.tick <= current)
        {
            return Err(SaddleDiveError::TickReplay);
        }
        self.current_tick = Some(input.tick);
        self.motion_resolved = false;
        self.rider_position = input.rider_position;

        let mut effects = SaddleDiveEffects::default();
        self.advance_recovery_boundaries(input.tick, &mut effects);

        let interact_edge = input.interact_pressed && !self.previous_interact_level;
        self.previous_interact_level = input.interact_pressed;
        let mut interact_consumed = false;

        match self.state {
            SaddleDiveState::Mounted => {
                self.mounted_horse_grounded = input.horse_grounded;
                if interact_edge {
                    // Mounted dismount/dive owns E even when an airborne horse rejects it.
                    interact_consumed = true;
                    if input.horse_grounded {
                        if planar_speed_squared(input.horse_velocity_mmps)
                            >= u128::from(MIN_DIVE_SPEED_MMPS).pow(2)
                        {
                            self.launch_dive(input, &mut effects)?;
                        } else {
                            self.ordinary_dismount(input.tick, &mut effects);
                        }
                    }
                }
            }
            SaddleDiveState::OnFootReady => {
                if interact_edge
                    && input.horse_retrievable
                    && planar_distance_squared(input.rider_position, input.horse_position)
                        <= u128::from(REMOUNT_RANGE_MM).pow(2)
                {
                    interact_consumed = true;
                    self.complete_remount(input.tick, input.horse_position, &mut effects);
                    self.mounted_horse_grounded = input.horse_grounded;
                }
            }
            SaddleDiveState::SaddleDiveAirborne
            | SaddleDiveState::LandingProne
            | SaddleDiveState::LandingRecovery => {}
        }

        Ok(self.tick_output(input.tick, interact_consumed, effects))
    }

    /// Advances the authority observation watermark after every damage/death
    /// observation through `tick` has been processed. Timer progression alone
    /// never closes the inclusive three-second window, which keeps an original
    /// boundary-tick observation attributable even when gameplay ticks advance
    /// before its authority delivery.
    pub fn settle_observations_through(&mut self, tick: SimulationTick) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        if self.current_tick.is_none_or(|current| tick > current)
            || self
                .observation_watermark
                .is_some_and(|watermark| tick <= watermark)
        {
            return effects;
        }
        self.observation_watermark = Some(tick);
        self.resolve_elapsed_death_windows(tick, &mut effects);
        effects
    }

    /// Feeds collision-resolved rider motion back once for the current tick.
    pub fn resolve_motion(
        &mut self,
        observation: RiderMotionObservation,
    ) -> Result<SaddleDiveEffects, SaddleDiveError> {
        if self.current_tick != Some(observation.tick) {
            return Err(SaddleDiveError::TickMismatch);
        }
        if self.motion_resolved {
            return Err(SaddleDiveError::MotionAlreadyResolved);
        }
        self.motion_resolved = true;
        self.rider_position = observation.rider_position;
        let mut effects = SaddleDiveEffects::default();

        let Some(airborne) = self.airborne else {
            return Ok(effects);
        };
        if self.state != SaddleDiveState::SaddleDiveAirborne {
            return Ok(effects);
        }

        if observation.descending {
            if let Some(normal) = observation.landing_normal {
                if let Some(slope) = landing_slope_millidegrees(normal) {
                    self.land_dive(airborne, observation, slope, &mut effects);
                    return Ok(effects);
                }
            }
        }

        if let Some(state) = &mut self.airborne {
            state.resolved_horizontal_velocity_mmps = [
                observation.rider_velocity_mmps[0],
                observation.rider_velocity_mmps[2],
            ];
        }
        Ok(effects)
    }

    /// Counts one valid airborne trigger pull at most once per source tick.
    pub fn record_shot_attempt(&mut self, tick: SimulationTick) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        if self.current_tick != Some(tick) || self.state != SaddleDiveState::SaddleDiveAirborne {
            return effects;
        }
        let Some(dive_id) = self.state_dive_id else {
            return effects;
        };
        let Some(record) = self.rows.get_mut(&dive_id) else {
            return effects;
        };
        if record.attempt_ticks.insert(tick) {
            record.row.shot_attempts = record.row.shot_attempts.saturating_add(1);
            self.push_update(dive_id, &mut effects);
        }
        effects
    }

    /// Counts an ammo-consuming combat acceptance exactly once.
    pub fn record_accepted_shot(&mut self, shot: AcceptedShotMetadata) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        let Some(dive_id) = shot.dive_id else {
            return effects;
        };
        if shot.shooter != self.actor || !shot.is_consistent() {
            return effects;
        }
        let Some(record) = self.rows.get_mut(&dive_id) else {
            return effects;
        };
        if record.finalized
            || shot.tick < record.row.launch_tick
            || shot.weapon_id != record.row.launch_weapon
            || shot.prelaunch_horizontal_velocity_mmps != record.row.prelaunch_velocity_mmps
            || record
                .row
                .landing_tick
                .is_some_and(|landing| shot.tick > landing)
            || record.accepted_shots.contains_key(&shot.tick)
        {
            return effects;
        }
        record.accepted_shots.insert(shot.tick, shot);
        record.row.shots_fired = record.row.shots_fired.saturating_add(1);
        self.push_update(dive_id, &mut effects);
        effects
    }

    /// Applies deduplicated authority attribution to instrumentation and forwards
    /// its deterministic notification rows.
    pub fn record_authority_result(
        &mut self,
        attribution: &ShotResultAttribution,
    ) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        if attribution.result.shooter_peer_id != self.actor || attribution.duplicate {
            return effects;
        }
        let Some(shot) = attribution.accepted_shot else {
            return effects;
        };
        let Some(dive_id) = shot.dive_id else {
            effects.events.extend(attribution.events.iter().cloned());
            return effects;
        };
        let Some(record) = self.rows.get_mut(&dive_id) else {
            return effects;
        };
        if record.finalized
            || record.accepted_shots.get(&shot.tick) != Some(&shot)
            || !record.resolved_shots.insert(shot.tick)
        {
            return effects;
        }
        effects.events.extend(attribution.events.iter().cloned());
        if attribution.result.outcome == ShotOutcome::Hit {
            record.row.shots_hit = record.row.shots_hit.saturating_add(1);
            record.row.damage_dealt = record
                .row
                .damage_dealt
                .saturating_add(u32::from(attribution.result.damage));
            if attribution.result.hit_zone == Some(HitZone::Head) {
                record.row.headshots = record.row.headshots.saturating_add(1);
            }
            if attribution
                .result
                .resolved_direction
                .is_some_and(|direction| {
                    horizontal_dot(
                        record.row.prelaunch_velocity_mmps,
                        [direction.x, direction.z],
                    ) < 0
                })
            {
                record.row.reversal_hits = record.row.reversal_hits.saturating_add(1);
            }
        }
        self.push_update(dive_id, &mut effects);
        self.maybe_finalize(dive_id, &mut effects);
        effects
    }

    /// Applies one external damage aggregate, updates every inclusive open
    /// landing window, and observes a resulting death.
    pub fn apply_external_damage(&mut self, observation: DamageObservation) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        if observation.id.actor != self.actor
            || observation.id.authority_epoch != self.authority_epoch
        {
            return effects;
        }
        let Some(application) = self.health.apply_external(observation) else {
            return effects;
        };
        self.damage_observations
            .insert(observation.id, observation.amount);
        self.record_damage_in_windows(observation, &mut effects);
        if application.died {
            self.observe_death_internal(observation.id.tick, &mut effects);
        }
        effects
    }

    /// Records an externally observed death once.
    pub fn observe_death(&mut self, tick: SimulationTick) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        self.health.observe_death();
        self.observe_death_internal(tick, &mut effects);
        effects
    }

    /// Censors every open row at match end.
    pub fn end_match(&mut self, _tick: SimulationTick) -> SaddleDiveEffects {
        self.censor_open_rows(DiveCensorReason::MatchEnded)
    }

    /// Explicitly ends collection when no terminal observation can arrive.
    pub fn censor_not_observed(&mut self) -> SaddleDiveEffects {
        self.censor_open_rows(DiveCensorReason::NotObserved)
    }

    /// Explicit course reset. This is allowed to reposition entities in the
    /// adapter and is never used as normal horse retrieval.
    pub fn reset(&mut self, tick: SimulationTick) -> SaddleDiveEffects {
        let mut effects = self.censor_open_rows(DiveCensorReason::Reset);
        let previous = self.state;
        let dive_id = self.state_dive_id;
        self.state = SaddleDiveState::Mounted;
        self.state_enter_tick = tick;
        if self.current_tick.is_none_or(|current| tick > current) {
            self.current_tick = Some(tick);
        }
        self.motion_resolved = true;
        self.state_dive_id = None;
        self.airborne = None;
        self.landing = None;
        self.mounted_horse_grounded = true;
        self.previous_interact_level = false;
        self.health.reset();
        if previous != SaddleDiveState::Mounted {
            effects.transitions.push(SaddleDiveTransition {
                from: previous,
                to: SaddleDiveState::Mounted,
                tick,
                dive_id,
            });
        }
        effects
    }

    fn validate_identity(
        &self,
        actor: PlayerId,
        authority_epoch: u64,
    ) -> Result<(), SaddleDiveError> {
        if actor != self.actor {
            return Err(SaddleDiveError::ActorMismatch);
        }
        if authority_epoch != self.authority_epoch {
            return Err(SaddleDiveError::AuthorityEpochMismatch);
        }
        Ok(())
    }

    fn launch_dive(
        &mut self,
        input: SaddleDiveTickInput,
        effects: &mut SaddleDiveEffects,
    ) -> Result<(), SaddleDiveError> {
        let next = self
            .next_dive_id
            .take()
            .ok_or(SaddleDiveError::DiveIdExhausted)?;
        let dive_id = DiveId(next);
        self.next_dive_id = next.get().checked_add(1).and_then(NonZeroU64::new);

        let launch = clamp_launch_direction(input.horse_velocity_mmps, input.chosen_direction);
        let impulse_x = div_round_half_away(
            i128::from(launch.clamped_direction.x)
                .saturating_mul(i128::from(HORIZONTAL_LAUNCH_IMPULSE_MMPS)),
            i128::from(DIRECTION_UNITS),
        );
        let impulse_z = div_round_half_away(
            i128::from(launch.clamped_direction.z)
                .saturating_mul(i128::from(HORIZONTAL_LAUNCH_IMPULSE_MMPS)),
            i128::from(DIRECTION_UNITS),
        );
        let launch_velocity = [
            saturating_i128_to_i32(i128::from(input.horse_velocity_mmps[0]) + impulse_x),
            VERTICAL_POP_MMPS as i32,
            saturating_i128_to_i32(i128::from(input.horse_velocity_mmps[2]) + impulse_z),
        ];
        let prelaunch_speed = rounded_planar_magnitude(input.horse_velocity_mmps);
        let resulting_planar_speed = rounded_magnitude_2d([launch_velocity[0], launch_velocity[2]]);
        let resulting_total_speed = rounded_magnitude_3d(launch_velocity);
        let launch_height = input
            .rider_position
            .y
            .saturating_sub(input.horse_position.y);
        let nominal_ticks = nominal_airtime_ticks(self.tick_rate);

        let row = DiveInstrumentationRow {
            schema_version: 1,
            authority_epoch: self.authority_epoch,
            actor: self.actor,
            dive_id,
            launch_tick: input.tick,
            launch_weapon: input.equipped_weapon,
            launch_gait: input.horse_gait,
            prelaunch_velocity_mmps: [input.horse_velocity_mmps[0], input.horse_velocity_mmps[2]],
            prelaunch_speed_mmps: prelaunch_speed,
            requested_direction: launch.requested_direction,
            requested_angle_millidegrees: launch.requested_angle_millidegrees,
            clamped_direction: launch.clamped_direction,
            clamped_angle_millidegrees: launch.clamped_angle_millidegrees,
            direction_was_clamped: launch.direction_was_clamped,
            horizontal_impulse_mmps: HORIZONTAL_LAUNCH_IMPULSE_MMPS,
            resulting_planar_speed_mmps: resulting_planar_speed,
            resulting_total_speed_mmps: resulting_total_speed,
            vertical_pop_mmps: VERTICAL_POP_MMPS,
            launch_height_mm: launch_height,
            nominal_airtime_ticks: nominal_ticks,
            landing_tick: None,
            airtime_ticks: None,
            shot_attempts: 0,
            shots_fired: 0,
            shots_hit: 0,
            headshots: 0,
            reversal_hits: 0,
            damage_dealt: 0,
            landing_terrain: None,
            landing_slope_millidegrees: None,
            landing_outcome: None,
            landing_damage: 0,
            damage_taken_landing_through_3s: 0,
            death_tick: None,
            death_within_3s: None,
            remount_tick: None,
            time_to_remount_ticks: None,
            censor_reason: None,
        };
        self.rows.insert(
            dive_id,
            DiveRecord {
                row,
                attempt_ticks: BTreeSet::new(),
                accepted_shots: BTreeMap::new(),
                resolved_shots: BTreeSet::new(),
                attributed_damage_observations: BTreeSet::new(),
                finalized: false,
            },
        );

        let previous = self.state;
        self.state = SaddleDiveState::SaddleDiveAirborne;
        self.state_enter_tick = input.tick;
        self.state_dive_id = Some(dive_id);
        self.airborne = Some(AirborneState {
            dive_id,
            launch_tick: input.tick,
            launch_velocity_mmps: launch_velocity,
            resolved_horizontal_velocity_mmps: [launch_velocity[0], launch_velocity[2]],
        });
        self.landing = None;
        effects.transitions.push(SaddleDiveTransition {
            from: previous,
            to: self.state,
            tick: input.tick,
            dive_id: Some(dive_id),
        });
        effects.commands.push(SaddleDiveCommand::DetachRider {
            dive_id: Some(dive_id),
            tick: input.tick,
            launch_velocity_mmps: launch_velocity,
        });
        effects.commands.push(SaddleDiveCommand::StartHorseRunout {
            dive_id,
            tick: input.tick,
            horse_position: input.horse_position,
            horse_velocity_mmps: [
                input.horse_velocity_mmps[0],
                0,
                input.horse_velocity_mmps[2],
            ],
        });
        let kind = GameplayEventKind::FlyingDismount;
        effects.events.push(GameplayEventRow {
            id: GameplayEventId {
                authority_epoch: self.authority_epoch,
                actor: self.actor,
                source_tick: input.tick,
                kind,
                sequence: dive_id.get(),
            },
            kind,
            tick: input.tick,
            actor: self.actor,
            dive_id: Some(dive_id),
            weapon_id: Some(input.equipped_weapon),
            target_id: None,
            hit_zone: None,
            damage: 0,
            text: kind.text(),
        });
        self.push_update(dive_id, effects);
        Ok(())
    }

    fn ordinary_dismount(&mut self, tick: SimulationTick, effects: &mut SaddleDiveEffects) {
        let previous = self.state;
        self.state = SaddleDiveState::OnFootReady;
        self.state_enter_tick = tick;
        self.state_dive_id = None;
        self.airborne = None;
        self.landing = None;
        effects.transitions.push(SaddleDiveTransition {
            from: previous,
            to: self.state,
            tick,
            dive_id: None,
        });
        effects.commands.push(SaddleDiveCommand::DetachRider {
            dive_id: None,
            tick,
            launch_velocity_mmps: [0; 3],
        });
    }

    fn land_dive(
        &mut self,
        airborne: AirborneState,
        observation: RiderMotionObservation,
        slope_millidegrees: u32,
        effects: &mut SaddleDiveEffects,
    ) {
        let bad = slope_millidegrees > BAD_LANDING_THRESHOLD_MILLIDEGREES;
        let landing_outcome = if bad {
            LandingOutcome::Bad
        } else {
            LandingOutcome::Good
        };
        let previous = self.state;
        self.state = SaddleDiveState::LandingProne;
        self.state_enter_tick = observation.tick;
        self.state_dive_id = Some(airborne.dive_id);
        self.airborne = None;
        self.landing = Some(LandingState {
            dive_id: airborne.dive_id,
            landing_tick: observation.tick,
            bad,
        });
        effects.transitions.push(SaddleDiveTransition {
            from: previous,
            to: self.state,
            tick: observation.tick,
            dive_id: Some(airborne.dive_id),
        });

        if let Some(record) = self.rows.get_mut(&airborne.dive_id) {
            if record.row.landing_tick.is_none() {
                record.row.landing_tick = Some(observation.tick);
                record.row.airtime_ticks = observation
                    .tick
                    .checked_duration_since(airborne.launch_tick);
                record.row.landing_terrain = Some(observation.landing_terrain);
                record.row.landing_slope_millidegrees = Some(slope_millidegrees);
                record.row.landing_outcome = Some(landing_outcome);
                if bad {
                    record.row.landing_damage = BAD_LANDING_DAMAGE;
                    record.row.damage_taken_landing_through_3s = record
                        .row
                        .damage_taken_landing_through_3s
                        .saturating_add(u32::from(BAD_LANDING_DAMAGE));
                }
                if let Some(death_tick) = record.row.death_tick {
                    let window = duration_ticks_ceil(self.tick_rate, 3_000);
                    record.row.death_within_3s =
                        Some(death_tick <= observation.tick.saturating_add(window));
                    record.row.censor_reason = Some(DiveCensorReason::DiedBeforeRemount);
                }
            }
        }
        self.attribute_retained_damage_for_dive(airborne.dive_id, effects);

        if bad {
            let command = RiderDamageCommand {
                id: RiderDamageCommandId {
                    authority_epoch: self.authority_epoch,
                    actor: self.actor,
                    dive_id: airborne.dive_id,
                    landing_tick: observation.tick,
                },
                amount: BAD_LANDING_DAMAGE,
                tick: observation.tick,
                source: "bad_landing",
            };
            if let Some(application) = self.health.apply_landing_command(command) {
                effects
                    .commands
                    .push(SaddleDiveCommand::ApplyRiderDamage(command));
                if application.died {
                    self.observe_death_internal(observation.tick, effects);
                }
            }
        }
        self.push_update(airborne.dive_id, effects);
        self.maybe_finalize(airborne.dive_id, effects);
    }

    fn advance_recovery_boundaries(
        &mut self,
        tick: SimulationTick,
        effects: &mut SaddleDiveEffects,
    ) {
        while let Some(landing) = self.landing {
            match self.state {
                SaddleDiveState::LandingProne => {
                    let prone_ticks =
                        duration_ticks_ceil(self.tick_rate, 400).saturating_add(if landing.bad {
                            duration_ticks_ceil(self.tick_rate, 400)
                        } else {
                            0
                        });
                    let boundary = landing.landing_tick.saturating_add(prone_ticks);
                    if tick < boundary {
                        break;
                    }
                    self.state = SaddleDiveState::LandingRecovery;
                    self.state_enter_tick = boundary;
                    effects.transitions.push(SaddleDiveTransition {
                        from: SaddleDiveState::LandingProne,
                        to: SaddleDiveState::LandingRecovery,
                        tick: boundary,
                        dive_id: Some(landing.dive_id),
                    });
                }
                SaddleDiveState::LandingRecovery => {
                    let boundary = self
                        .state_enter_tick
                        .saturating_add(duration_ticks_ceil(self.tick_rate, 400));
                    if tick < boundary {
                        break;
                    }
                    self.state = SaddleDiveState::OnFootReady;
                    self.state_enter_tick = boundary;
                    self.state_dive_id = Some(landing.dive_id);
                    effects.transitions.push(SaddleDiveTransition {
                        from: SaddleDiveState::LandingRecovery,
                        to: SaddleDiveState::OnFootReady,
                        tick: boundary,
                        dive_id: Some(landing.dive_id),
                    });
                }
                _ => break,
            }
        }
    }

    fn resolve_elapsed_death_windows(
        &mut self,
        current_tick: SimulationTick,
        effects: &mut SaddleDiveEffects,
    ) {
        let observation_ticks = duration_ticks_ceil(self.tick_rate, 3_000);
        let ids: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(id, record)| {
                if record.finalized || record.row.death_within_3s.is_some() {
                    return None;
                }
                let landing = record.row.landing_tick?;
                (current_tick >= landing.saturating_add(observation_ticks)).then_some(*id)
            })
            .collect();
        for id in ids {
            if let Some(record) = self.rows.get_mut(&id) {
                record.row.death_within_3s = Some(false);
            }
            self.push_update(id, effects);
            self.maybe_finalize(id, effects);
        }
    }

    fn complete_remount(
        &mut self,
        tick: SimulationTick,
        horse_position: QuantizedOrigin,
        effects: &mut SaddleDiveEffects,
    ) {
        let previous = self.state;
        let dive_id = self.state_dive_id;
        self.state = SaddleDiveState::Mounted;
        self.state_enter_tick = tick;
        self.state_dive_id = None;
        self.airborne = None;
        self.landing = None;
        effects.transitions.push(SaddleDiveTransition {
            from: previous,
            to: self.state,
            tick,
            dive_id,
        });
        effects.commands.push(SaddleDiveCommand::AttachRider {
            tick,
            horse_position,
        });

        if let Some(id) = dive_id {
            if let Some(record) = self.rows.get_mut(&id) {
                if !record.finalized && record.row.remount_tick.is_none() {
                    record.row.remount_tick = Some(tick);
                    record.row.time_to_remount_ticks = record
                        .row
                        .landing_tick
                        .and_then(|landing| tick.checked_duration_since(landing));
                }
            }
            self.push_update(id, effects);
            self.maybe_finalize(id, effects);
        }
    }

    fn record_damage_in_windows(
        &mut self,
        observation: DamageObservation,
        effects: &mut SaddleDiveEffects,
    ) {
        let observation_ticks = duration_ticks_ceil(self.tick_rate, 3_000);
        let ids: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(id, record)| {
                if record.finalized
                    || record
                        .attributed_damage_observations
                        .contains(&observation.id)
                {
                    return None;
                }
                let landing = record.row.landing_tick?;
                (observation.id.tick >= landing
                    && observation.id.tick <= landing.saturating_add(observation_ticks))
                .then_some(*id)
            })
            .collect();
        for id in ids {
            if let Some(record) = self.rows.get_mut(&id) {
                if !record.attributed_damage_observations.insert(observation.id) {
                    continue;
                }
                record.row.damage_taken_landing_through_3s = record
                    .row
                    .damage_taken_landing_through_3s
                    .saturating_add(u32::from(observation.amount));
            }
            self.push_update(id, effects);
        }
    }

    fn attribute_retained_damage_for_dive(
        &mut self,
        dive_id: DiveId,
        effects: &mut SaddleDiveEffects,
    ) {
        let observations: Vec<_> = self
            .damage_observations
            .iter()
            .map(|(id, amount)| DamageObservation {
                id: *id,
                amount: *amount,
            })
            .collect();
        for observation in observations {
            let before = self
                .rows
                .get(&dive_id)
                .map(|record| record.row.damage_taken_landing_through_3s);
            self.record_damage_in_windows(observation, effects);
            let after = self
                .rows
                .get(&dive_id)
                .map(|record| record.row.damage_taken_landing_through_3s);
            if before != after {
                self.push_update(dive_id, effects);
            }
        }
    }

    fn observe_death_internal(&mut self, tick: SimulationTick, effects: &mut SaddleDiveEffects) {
        let observation_ticks = duration_ticks_ceil(self.tick_rate, 3_000);
        let ids: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(id, record)| {
                (!record.finalized
                    && record.row.death_tick.is_none()
                    && tick >= record.row.launch_tick)
                    .then_some(*id)
            })
            .collect();
        for id in ids {
            if let Some(record) = self.rows.get_mut(&id) {
                record.row.death_tick = Some(tick);
                match record.row.landing_tick {
                    None => {
                        record.row.censor_reason = Some(DiveCensorReason::DiedAirborne);
                    }
                    Some(landing) => {
                        record.row.death_within_3s =
                            Some(tick <= landing.saturating_add(observation_ticks));
                        if record.row.remount_tick.is_none_or(|remount| tick < remount) {
                            record.row.censor_reason = Some(DiveCensorReason::DiedBeforeRemount);
                        }
                    }
                }
            }
            self.push_update(id, effects);
            self.maybe_finalize(id, effects);
        }
    }

    fn censor_open_rows(&mut self, reason: DiveCensorReason) -> SaddleDiveEffects {
        let mut effects = SaddleDiveEffects::default();
        let ids: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(id, record)| (!record.finalized).then_some(*id))
            .collect();
        for id in ids {
            if let Some(record) = self.rows.get_mut(&id) {
                record.row.censor_reason = Some(reason);
            }
            self.push_update(id, &mut effects);
            self.maybe_finalize(id, &mut effects);
        }
        effects
    }

    fn maybe_finalize(&mut self, dive_id: DiveId, effects: &mut SaddleDiveEffects) {
        let current_tick = self.current_tick;
        let motion_resolved = self.motion_resolved;
        let should_finalize = self.rows.get(&dive_id).is_some_and(|record| {
            if record.finalized {
                return false;
            }
            let all_accepted_results_settled =
                record.accepted_shots.len() == record.resolved_shots.len();
            match record.row.censor_reason {
                Some(
                    DiveCensorReason::MatchEnded
                    | DiveCensorReason::Reset
                    | DiveCensorReason::NotObserved,
                ) => true,
                Some(DiveCensorReason::DiedAirborne) => {
                    all_accepted_results_settled
                        && (motion_resolved
                            || current_tick.is_some_and(|tick| {
                                record.row.death_tick.is_some_and(|death| tick > death)
                            }))
                }
                Some(DiveCensorReason::DiedBeforeRemount) => all_accepted_results_settled,
                None => {
                    all_accepted_results_settled
                        && record.row.death_within_3s.is_some()
                        && record.row.remount_tick.is_some()
                }
            }
        });
        if !should_finalize {
            return;
        }
        if let Some(record) = self.rows.get_mut(&dive_id) {
            record.finalized = true;
            effects.telemetry_finalized.push(record.row.clone());
        }
    }

    fn push_update(&self, dive_id: DiveId, effects: &mut SaddleDiveEffects) {
        let Some(record) = self.rows.get(&dive_id) else {
            return;
        };
        if let Some(existing) = effects
            .telemetry_updates
            .iter_mut()
            .find(|row| row.dive_id == dive_id)
        {
            *existing = record.row.clone();
        } else {
            effects.telemetry_updates.push(record.row.clone());
        }
    }

    fn tick_output(
        &self,
        tick: SimulationTick,
        interact_consumed: bool,
        effects: SaddleDiveEffects,
    ) -> SaddleDiveTickOutput {
        let stance = self.stance();
        let movement_input_scale_milli = match self.state {
            SaddleDiveState::LandingProne => MOVEMENT_SCALE_PRONE_MILLI,
            SaddleDiveState::LandingRecovery => MOVEMENT_SCALE_RECOVERY_MILLI,
            SaddleDiveState::Mounted
            | SaddleDiveState::SaddleDiveAirborne
            | SaddleDiveState::OnFootReady => MOVEMENT_SCALE_FULL_MILLI,
        };
        let can_fire = matches!(
            stance,
            RiderStance::Mounted | RiderStance::SaddleDiveAirborne
        );
        let can_reload = stance == RiderStance::Mounted;
        let (sway_scale_milli, requested_rider_velocity_mmps) =
            if let Some(airborne) = self.airborne {
                let elapsed = tick
                    .checked_duration_since(airborne.launch_tick)
                    .unwrap_or_default();
                let nominal = nominal_airtime_ticks(self.tick_rate);
                (
                    dive_sway_scale_milli(elapsed, nominal),
                    Some([
                        airborne.resolved_horizontal_velocity_mmps[0],
                        airborne_vertical_velocity_mmps(elapsed, self.tick_rate),
                        airborne.resolved_horizontal_velocity_mmps[1],
                    ]),
                )
            } else {
                (1_000, None)
            };
        SaddleDiveTickOutput {
            tick,
            state: self.state,
            stance,
            dive_id: self.state_dive_id,
            movement_input_scale_milli,
            can_fire,
            can_reload,
            sway_scale_milli,
            requested_rider_velocity_mmps,
            interact_consumed,
            effects,
        }
    }
}

/// Integer launch-clamp result used by movement and instrumentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchDirection {
    /// Projected/fallback request.
    pub requested_direction: QuantizedDirection,
    /// Signed request angle in `(-180000, 180000]`.
    pub requested_angle_millidegrees: i32,
    /// Reconstructed direction after clamping.
    pub clamped_direction: QuantizedDirection,
    /// Clamped signed angle.
    pub clamped_angle_millidegrees: i32,
    /// Whether clamping changed the angle.
    pub direction_was_clamped: bool,
}

/// Projects, falls back, measures, clamps, and reconstructs a launch direction
/// using fixed integer/CORDIC math.
#[must_use]
pub fn clamp_launch_direction(
    horse_velocity_mmps: [i32; 3],
    chosen_direction: Option<QuantizedDirection>,
) -> LaunchDirection {
    let base = normalize_planar([
        i128::from(horse_velocity_mmps[0]),
        i128::from(horse_velocity_mmps[2]),
    ])
    .unwrap_or([0, -DIRECTION_UNITS]);
    let requested = chosen_direction
        .filter(|direction| direction_is_normalized_integer(*direction))
        .and_then(|direction| normalize_planar([i128::from(direction.x), i128::from(direction.z)]))
        .unwrap_or(base);
    let dot = i128::from(base[0]) * i128::from(requested[0])
        + i128::from(base[1]) * i128::from(requested[1]);
    let cross_y = i128::from(base[1]) * i128::from(requested[0])
        - i128::from(base[0]) * i128::from(requested[1]);
    let requested_angle = atan2_millidegrees(cross_y, dot);
    let clamped_angle = requested_angle.clamp(
        -LAUNCH_CONE_HALF_ANGLE_MILLIDEGREES,
        LAUNCH_CONE_HALF_ANGLE_MILLIDEGREES,
    );
    let clamped = rotate_planar(base, clamped_angle);
    LaunchDirection {
        requested_direction: QuantizedDirection::new(requested[0], 0, requested[1]),
        requested_angle_millidegrees: requested_angle,
        clamped_direction: QuantizedDirection::new(clamped[0], 0, clamped[1]),
        clamped_angle_millidegrees: clamped_angle,
        direction_was_clamped: requested_angle != clamped_angle,
    }
}

/// Returns nearest-millidegree slope for a normalized upward normal.
#[must_use]
pub fn landing_slope_millidegrees(normal: QuantizedDirection) -> Option<u32> {
    if normal.y <= 0 || !direction_is_normalized_integer(normal) {
        return None;
    }
    let horizontal_squared =
        i128::from(normal.x) * i128::from(normal.x) + i128::from(normal.z) * i128::from(normal.z);
    let horizontal = i128::try_from((horizontal_squared as u128).isqrt()).ok()?;
    u32::try_from(atan2_millidegrees(horizontal, i128::from(normal.y))).ok()
}

/// Horse control mode retained by the same engine object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorseRunoutState {
    /// Normal player input controls the horse.
    #[default]
    PlayerControlled,
    /// Fixed-heading linear deceleration ignores player input.
    Runout,
    /// Horse is stopped in place and may be remounted.
    IdleRetrievable,
}

/// One runout state transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HorseRunoutTransition {
    /// Previous mode.
    pub from: HorseRunoutState,
    /// New mode.
    pub to: HorseRunoutState,
    /// Exact transition tick.
    pub tick: SimulationTick,
}

/// Per-tick runout request. The adapter must respect `maximum_step_mm` before
/// moving so the existing horse body never crosses the 25 m cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HorseRunoutTickOutput {
    /// Current mode.
    pub state: HorseRunoutState,
    /// Fixed-heading target velocity.
    pub requested_velocity_mmps: [i32; 3],
    /// Maximum planar displacement allowed this tick.
    pub maximum_step_mm: u32,
    /// Cumulative collision-resolved travel.
    pub cumulative_travel_mm: u32,
    /// Transition produced by a timer boundary.
    pub transition: Option<HorseRunoutTransition>,
}

/// Strict ordering errors for horse collision feedback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum HorseRunoutError {
    /// Tick rate was zero.
    #[error("invalid_tick_rate")]
    InvalidTickRate,
    /// Runout can start only from player control.
    #[error("not_player_controlled")]
    NotPlayerControlled,
    /// Begin tick repeated/regressed.
    #[error("tick_replay")]
    TickReplay,
    /// Resolve tick did not match the current begun tick.
    #[error("tick_mismatch")]
    TickMismatch,
    /// Resolve repeated for one tick.
    #[error("motion_already_resolved")]
    MotionAlreadyResolved,
    /// Collision feedback exceeded the movement request for this tick.
    #[error("motion_exceeds_maximum_step")]
    MotionExceedsMaximumStep,
    /// Only an idle horse can complete remount.
    #[error("not_retrievable")]
    NotRetrievable,
}

/// Fixed-heading, collision-feedback horse runout kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HorseRunoutKernel {
    tick_rate: u32,
    state: HorseRunoutState,
    launch_tick: Option<SimulationTick>,
    current_tick: Option<SimulationTick>,
    motion_resolved: bool,
    initial_planar_velocity_mmps: [i32; 2],
    last_position: QuantizedOrigin,
    current_maximum_step_mm: u32,
    cumulative_travel_mm: u32,
}

impl HorseRunoutKernel {
    /// Creates a player-controlled horse kernel.
    pub fn new(tick_rate: u32) -> Result<Self, HorseRunoutError> {
        if tick_rate == 0 {
            return Err(HorseRunoutError::InvalidTickRate);
        }
        Ok(Self {
            tick_rate,
            state: HorseRunoutState::PlayerControlled,
            launch_tick: None,
            current_tick: None,
            motion_resolved: false,
            initial_planar_velocity_mmps: [0; 2],
            last_position: QuantizedOrigin::default(),
            current_maximum_step_mm: 0,
            cumulative_travel_mm: 0,
        })
    }

    /// Current mode.
    #[must_use]
    pub const fn state(&self) -> HorseRunoutState {
        self.state
    }

    /// Only the stopped mode is retrievable in M2.
    #[must_use]
    pub const fn is_retrievable(&self) -> bool {
        matches!(self.state, HorseRunoutState::IdleRetrievable)
    }

    /// Cumulative collision-resolved travel.
    #[must_use]
    pub const fn cumulative_travel_mm(&self) -> u32 {
        self.cumulative_travel_mm
    }

    /// Starts runout on the same horse identity.
    pub fn start_runout(
        &mut self,
        tick: SimulationTick,
        position: QuantizedOrigin,
        velocity_mmps: [i32; 3],
    ) -> Result<HorseRunoutTransition, HorseRunoutError> {
        if self.state != HorseRunoutState::PlayerControlled {
            return Err(HorseRunoutError::NotPlayerControlled);
        }
        self.state = HorseRunoutState::Runout;
        self.launch_tick = Some(tick);
        self.current_tick = None;
        self.motion_resolved = false;
        self.initial_planar_velocity_mmps = [velocity_mmps[0], velocity_mmps[2]];
        self.last_position = position;
        self.current_maximum_step_mm = 0;
        self.cumulative_travel_mm = 0;
        Ok(HorseRunoutTransition {
            from: HorseRunoutState::PlayerControlled,
            to: HorseRunoutState::Runout,
            tick,
        })
    }

    /// Ordinary low-speed dismount stops at the existing transform.
    pub fn stop_for_dismount(
        &mut self,
        tick: SimulationTick,
        position: QuantizedOrigin,
    ) -> HorseRunoutTransition {
        let previous = self.state;
        self.state = HorseRunoutState::IdleRetrievable;
        self.launch_tick = None;
        self.current_tick = None;
        self.motion_resolved = false;
        self.initial_planar_velocity_mmps = [0; 2];
        self.last_position = position;
        self.current_maximum_step_mm = 0;
        self.cumulative_travel_mm = 0;
        HorseRunoutTransition {
            from: previous,
            to: HorseRunoutState::IdleRetrievable,
            tick,
        }
    }

    /// Begins one newer horse tick and returns a clamped movement request.
    pub fn begin_tick(
        &mut self,
        tick: SimulationTick,
    ) -> Result<HorseRunoutTickOutput, HorseRunoutError> {
        if self.current_tick.is_some_and(|current| tick <= current) {
            return Err(HorseRunoutError::TickReplay);
        }
        self.current_tick = Some(tick);
        self.motion_resolved = false;
        let mut transition = None;
        if self.state == HorseRunoutState::Runout {
            let launch = self.launch_tick.expect("runout has launch tick");
            let duration = duration_ticks_ceil(self.tick_rate, 2_000);
            if tick >= launch.saturating_add(duration)
                || self.cumulative_travel_mm >= HORSE_MAX_TRAVEL_MM
            {
                self.state = HorseRunoutState::IdleRetrievable;
                transition = Some(HorseRunoutTransition {
                    from: HorseRunoutState::Runout,
                    to: HorseRunoutState::IdleRetrievable,
                    tick: if self.cumulative_travel_mm >= HORSE_MAX_TRAVEL_MM {
                        tick
                    } else {
                        launch.saturating_add(duration)
                    },
                });
            }
        }

        let remaining = HORSE_MAX_TRAVEL_MM.saturating_sub(self.cumulative_travel_mm);
        let (velocity, maximum_step_mm) = if self.state == HorseRunoutState::Runout {
            let launch = self.launch_tick.expect("runout has launch tick");
            let elapsed = tick.checked_duration_since(launch).unwrap_or_default();
            let duration = duration_ticks_ceil(self.tick_rate, 2_000).max(1);
            let remaining_ticks = duration.saturating_sub(elapsed);
            let target = [
                scale_ratio_symmetric(
                    self.initial_planar_velocity_mmps[0],
                    remaining_ticks,
                    duration,
                ),
                scale_ratio_symmetric(
                    self.initial_planar_velocity_mmps[1],
                    remaining_ticks,
                    duration,
                ),
            ];
            let natural_step =
                u64::from(rounded_magnitude_2d(target)).div_ceil(u64::from(self.tick_rate));
            let maximum_step = u32::try_from(natural_step)
                .unwrap_or(u32::MAX)
                .min(remaining);
            let velocity = clamp_velocity_to_step(target, maximum_step, self.tick_rate);
            ([velocity[0], 0, velocity[1]], maximum_step)
        } else {
            ([0; 3], 0)
        };
        self.current_maximum_step_mm = maximum_step_mm;
        Ok(HorseRunoutTickOutput {
            state: self.state,
            requested_velocity_mmps: velocity,
            maximum_step_mm,
            cumulative_travel_mm: self.cumulative_travel_mm,
            transition,
        })
    }

    /// Clamps collision feedback to this tick's requested planar step. Adapters
    /// apply the returned position to the real horse body before resolution so
    /// the 25 m cap constrains world position, not only telemetry accounting.
    #[must_use]
    pub fn clamp_motion_position(&self, position: QuantizedOrigin) -> QuantizedOrigin {
        if self.state != HorseRunoutState::Runout {
            return position;
        }
        clamp_origin_to_planar_step(self.last_position, position, self.current_maximum_step_mm)
    }

    /// Records one collision-resolved horse movement and stops at the cap.
    pub fn resolve_motion(
        &mut self,
        tick: SimulationTick,
        position: QuantizedOrigin,
        _velocity_mmps: [i32; 3],
    ) -> Result<Option<HorseRunoutTransition>, HorseRunoutError> {
        if self.current_tick != Some(tick) {
            return Err(HorseRunoutError::TickMismatch);
        }
        if self.motion_resolved {
            return Err(HorseRunoutError::MotionAlreadyResolved);
        }
        if self.state != HorseRunoutState::Runout {
            self.motion_resolved = true;
            self.last_position = position;
            return Ok(None);
        }
        let travelled = rounded_planar_distance(self.last_position, position);
        if travelled > self.current_maximum_step_mm {
            return Err(HorseRunoutError::MotionExceedsMaximumStep);
        }
        self.motion_resolved = true;
        let remaining = HORSE_MAX_TRAVEL_MM.saturating_sub(self.cumulative_travel_mm);
        self.cumulative_travel_mm = self
            .cumulative_travel_mm
            .saturating_add(travelled.min(remaining));
        self.last_position = position;
        if self.cumulative_travel_mm < HORSE_MAX_TRAVEL_MM {
            return Ok(None);
        }
        self.state = HorseRunoutState::IdleRetrievable;
        Ok(Some(HorseRunoutTransition {
            from: HorseRunoutState::Runout,
            to: HorseRunoutState::IdleRetrievable,
            tick,
        }))
    }

    /// Returns control after an eligible remount; no position is changed.
    pub fn complete_remount(
        &mut self,
        tick: SimulationTick,
    ) -> Result<HorseRunoutTransition, HorseRunoutError> {
        if self.state != HorseRunoutState::IdleRetrievable {
            return Err(HorseRunoutError::NotRetrievable);
        }
        self.state = HorseRunoutState::PlayerControlled;
        self.launch_tick = None;
        self.current_tick = None;
        self.motion_resolved = false;
        self.initial_planar_velocity_mmps = [0; 2];
        self.current_maximum_step_mm = 0;
        Ok(HorseRunoutTransition {
            from: HorseRunoutState::IdleRetrievable,
            to: HorseRunoutState::PlayerControlled,
            tick,
        })
    }
}

fn clamp_origin_to_planar_step(
    origin: QuantizedOrigin,
    requested: QuantizedOrigin,
    maximum_step_mm: u32,
) -> QuantizedOrigin {
    let distance = rounded_planar_distance(origin, requested);
    if distance <= maximum_step_mm {
        return requested;
    }
    if maximum_step_mm == 0 {
        return QuantizedOrigin::new(origin.x, requested.y, origin.z);
    }
    let dx = i128::from(requested.x) - i128::from(origin.x);
    let dz = i128::from(requested.z) - i128::from(origin.z);
    let mut scaled_step = maximum_step_mm;
    loop {
        let x = origin
            .x
            .saturating_add(saturating_i128_to_i32(div_round_half_away(
                dx * i128::from(scaled_step),
                i128::from(distance),
            )));
        let z = origin
            .z
            .saturating_add(saturating_i128_to_i32(div_round_half_away(
                dz * i128::from(scaled_step),
                i128::from(distance),
            )));
        let clamped = QuantizedOrigin::new(x, requested.y, z);
        if rounded_planar_distance(origin, clamped) <= maximum_step_mm || scaled_step == 0 {
            return clamped;
        }
        scaled_step -= 1;
    }
}

fn clamp_velocity_to_step(target: [i32; 2], maximum_step_mm: u32, tick_rate: u32) -> [i32; 2] {
    let magnitude = rounded_magnitude_2d(target);
    if magnitude == 0 || maximum_step_mm == 0 {
        return [0; 2];
    }
    let maximum_velocity = u64::from(maximum_step_mm).saturating_mul(u64::from(tick_rate));
    if u64::from(magnitude) <= maximum_velocity {
        return target;
    }
    [
        saturating_i128_to_i32(div_round_half_away(
            i128::from(target[0]) * i128::from(maximum_velocity),
            i128::from(magnitude),
        )),
        saturating_i128_to_i32(div_round_half_away(
            i128::from(target[1]) * i128::from(maximum_velocity),
            i128::from(magnitude),
        )),
    ]
}

fn scale_ratio_symmetric(value: i32, numerator: u64, denominator: u64) -> i32 {
    if denominator == 0 {
        return 0;
    }
    saturating_i128_to_i32(div_round_half_away(
        i128::from(value) * i128::from(numerator),
        i128::from(denominator),
    ))
}

fn direction_is_normalized_integer(direction: QuantizedDirection) -> bool {
    let squared = i128::from(direction.x) * i128::from(direction.x)
        + i128::from(direction.y) * i128::from(direction.y)
        + i128::from(direction.z) * i128::from(direction.z);
    (NORMALIZED_LOWER_UNITS * NORMALIZED_LOWER_UNITS
        ..=NORMALIZED_UPPER_UNITS * NORMALIZED_UPPER_UNITS)
        .contains(&squared)
}

fn normalize_planar(value: [i128; 2]) -> Option<[i32; 2]> {
    let squared = value[0]
        .checked_mul(value[0])?
        .checked_add(value[1].checked_mul(value[1])?)?;
    if squared == 0 {
        return None;
    }
    let length = i128::try_from((squared as u128).isqrt()).ok()?.max(1);
    let normalized = [
        saturating_i128_to_i32(div_round_half_away(
            value[0] * i128::from(DIRECTION_UNITS),
            length,
        )),
        saturating_i128_to_i32(div_round_half_away(
            value[1] * i128::from(DIRECTION_UNITS),
            length,
        )),
    ];
    Some(normalized)
}

fn rotate_planar(base: [i32; 2], angle_millidegrees: i32) -> [i32; 2] {
    if angle_millidegrees.rem_euclid(360_000) == 0 {
        return base;
    }
    let (cosine, sine) = cordic_sin_cos(angle_millidegrees);
    let x = div_round_half_away(
        i128::from(base[0]) * cosine + i128::from(base[1]) * sine,
        CORDIC_SCALE,
    );
    let z = div_round_half_away(
        -i128::from(base[0]) * sine + i128::from(base[1]) * cosine,
        CORDIC_SCALE,
    );
    normalize_planar([x, z]).unwrap_or(base)
}

fn cordic_sin_cos(angle_millidegrees: i32) -> (i128, i128) {
    let mut reduced = angle_millidegrees.rem_euclid(360_000);
    if reduced > 180_000 {
        reduced -= 360_000;
    }
    let negate = if reduced > 90_000 {
        reduced -= 180_000;
        true
    } else if reduced < -90_000 {
        reduced += 180_000;
        true
    } else {
        false
    };

    let mut x = CORDIC_GAIN_INVERSE;
    let mut y = 0_i128;
    let mut angle = i64::from(reduced) * NANODEGREES_PER_MILLIDEGREE;
    for (shift, step) in CORDIC_ANGLES_NANODEGREES.iter().enumerate() {
        let previous_x = x;
        if angle > 0 {
            x -= y >> shift;
            y += previous_x >> shift;
            angle -= step;
        } else if angle < 0 {
            x += y >> shift;
            y -= previous_x >> shift;
            angle += step;
        }
    }
    if negate {
        (-x, -y)
    } else {
        (x, y)
    }
}

fn atan2_millidegrees(mut y: i128, mut x: i128) -> i32 {
    if x == 0 && y == 0 {
        return 0;
    }
    let mut angle = 0_i64;
    if x < 0 {
        if y >= 0 {
            angle = NANODEGREES_180;
        } else {
            angle = -NANODEGREES_180;
        }
        x = -x;
        y = -y;
    }
    for (shift, step) in CORDIC_ANGLES_NANODEGREES.iter().enumerate() {
        let previous_x = x;
        if y > 0 {
            x += y >> shift;
            y -= previous_x >> shift;
            angle = angle.saturating_add(*step);
        } else if y < 0 {
            x -= y >> shift;
            y += previous_x >> shift;
            angle = angle.saturating_sub(*step);
        }
    }
    let rounded = div_round_half_away(i128::from(angle), i128::from(NANODEGREES_PER_MILLIDEGREE));
    rounded.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32
}

fn div_round_half_away(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0);
    if numerator >= 0 {
        numerator.saturating_add(denominator / 2) / denominator
    } else {
        -numerator.saturating_abs().saturating_add(denominator / 2) / denominator
    }
}

fn saturating_i128_to_i32(value: i128) -> i32 {
    value.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32
}

fn horizontal_dot(first: [i32; 2], second: [i32; 2]) -> i128 {
    i128::from(first[0]) * i128::from(second[0]) + i128::from(first[1]) * i128::from(second[1])
}

fn planar_speed_squared(velocity: [i32; 3]) -> u128 {
    let x = i128::from(velocity[0]);
    let z = i128::from(velocity[2]);
    (x * x + z * z) as u128
}

fn planar_distance_squared(first: QuantizedOrigin, second: QuantizedOrigin) -> u128 {
    let x = i128::from(first.x) - i128::from(second.x);
    let z = i128::from(first.z) - i128::from(second.z);
    (x * x + z * z) as u128
}

fn rounded_planar_magnitude(velocity: [i32; 3]) -> u32 {
    rounded_magnitude_2d([velocity[0], velocity[2]])
}

fn rounded_magnitude_2d(value: [i32; 2]) -> u32 {
    let x = i128::from(value[0]);
    let y = i128::from(value[1]);
    rounded_sqrt((x * x + y * y) as u128)
}

fn rounded_magnitude_3d(value: [i32; 3]) -> u32 {
    let x = i128::from(value[0]);
    let y = i128::from(value[1]);
    let z = i128::from(value[2]);
    rounded_sqrt((x * x + y * y + z * z) as u128)
}

fn rounded_planar_distance(first: QuantizedOrigin, second: QuantizedOrigin) -> u32 {
    let x = i128::from(first.x) - i128::from(second.x);
    let z = i128::from(first.z) - i128::from(second.z);
    rounded_sqrt((x * x + z * z) as u128)
}

fn rounded_sqrt(value: u128) -> u32 {
    let floor = value.isqrt();
    let rounded = if floor
        .checked_mul(floor)
        .and_then(|square| square.checked_add(floor))
        .is_some_and(|midpoint_floor| value > midpoint_floor)
    {
        floor.saturating_add(1)
    } else {
        floor
    };
    u32::try_from(rounded).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> PlayerId {
        PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap()
    }

    fn direction(angle_millidegrees: i32) -> QuantizedDirection {
        let [x, z] = rotate_planar([0, -DIRECTION_UNITS], angle_millidegrees);
        QuantizedDirection::new(x, 0, z)
    }

    fn tick_input(tick: u64, speed_mmps: i32, interact: bool) -> SaddleDiveTickInput {
        SaddleDiveTickInput {
            tick: SimulationTick::new(tick),
            interact_pressed: interact,
            chosen_direction: Some(direction(0)),
            horse_grounded: true,
            horse_position: QuantizedOrigin::new(0, 0, 0),
            horse_velocity_mmps: [0, 0, -speed_mmps],
            horse_gait: CombatGait::Gallop,
            equipped_weapon: WeaponId::Dustwalker,
            rider_position: QuantizedOrigin::new(0, SADDLE_LAUNCH_HEIGHT_MM, 0),
            horse_retrievable: false,
            authority_epoch: 7,
            actor: player(),
        }
    }

    fn launch_kernel() -> (SaddleDiveKernel, DiveId) {
        let mut kernel = SaddleDiveKernel::new(60, player(), 7).unwrap();
        let output = kernel.begin_tick(tick_input(10, 8_000, true)).unwrap();
        (kernel, output.dive_id.unwrap())
    }

    #[test]
    fn authority_epoch_advances_without_resetting_active_dive() {
        let (mut kernel, dive_id) = launch_kernel();
        assert!(kernel.set_authority_epoch(8));
        assert_eq!(kernel.authority_epoch(), 8);
        assert_eq!(kernel.current_dive_id(), Some(dive_id));
        assert!(!kernel.set_authority_epoch(7));
        assert_eq!(kernel.authority_epoch(), 8);
    }

    fn land(
        kernel: &mut SaddleDiveKernel,
        tick: u64,
        slope_millidegrees: u32,
        terrain: LandingTerrain,
    ) -> SaddleDiveEffects {
        if kernel.current_tick() != Some(SimulationTick::new(tick)) {
            let mut input = tick_input(tick, 0, false);
            input.horse_grounded = true;
            kernel.begin_tick(input).unwrap();
        }
        kernel
            .resolve_motion(RiderMotionObservation {
                tick: SimulationTick::new(tick),
                rider_position: QuantizedOrigin::new(0, 0, -5_000),
                rider_velocity_mmps: [0, -5_000, -8_000],
                descending: true,
                landing_normal: Some(direction_for_slope(slope_millidegrees)),
                landing_terrain: terrain,
            })
            .unwrap()
    }

    fn direction_for_slope(slope_millidegrees: u32) -> QuantizedDirection {
        let (cosine, sine) = cordic_sin_cos(slope_millidegrees as i32);
        QuantizedDirection::new(
            saturating_i128_to_i32(div_round_half_away(
                sine * i128::from(DIRECTION_UNITS),
                CORDIC_SCALE,
            )),
            saturating_i128_to_i32(div_round_half_away(
                cosine * i128::from(DIRECTION_UNITS),
                CORDIC_SCALE,
            )),
            0,
        )
    }

    #[test]
    fn k01_speed_threshold_is_inclusive_and_uses_exact_planar_squared_speed() {
        for (velocity, dives) in [
            ([0, 0, -7_999], false),
            ([0, 0, -8_000], true),
            ([0, 0, -8_001], true),
            ([4_800, 0, -6_400], true),
            ([4_799, 0, -6_400], false),
        ] {
            let mut kernel = SaddleDiveKernel::new(60, player(), 7).unwrap();
            let mut input = tick_input(1, 0, true);
            input.horse_velocity_mmps = velocity;
            let output = kernel.begin_tick(input).unwrap();
            assert_eq!(output.state == SaddleDiveState::SaddleDiveAirborne, dives);
            assert_eq!(output.dive_id.is_some(), dives);
            if !dives {
                assert_eq!(output.state, SaddleDiveState::OnFootReady);
                assert!(output.effects.events.is_empty());
            }
        }
    }

    #[test]
    fn k02_interact_is_edge_defended_and_airborne_mount_dismount_is_ignored() {
        let mut kernel = SaddleDiveKernel::new(60, player(), 7).unwrap();
        let first = kernel.begin_tick(tick_input(1, 8_000, true)).unwrap();
        let first_id = first.dive_id.unwrap();
        assert!(first.interact_consumed);
        for tick in 2..=5 {
            let held = kernel.begin_tick(tick_input(tick, 8_000, true)).unwrap();
            assert_eq!(held.dive_id, Some(first_id));
            assert!(held.effects.events.is_empty());
        }

        let mut jumping = SaddleDiveKernel::new(60, player(), 7).unwrap();
        let mut input = tick_input(1, 14_000, true);
        input.horse_grounded = false;
        let ignored = jumping.begin_tick(input).unwrap();
        assert_eq!(ignored.state, SaddleDiveState::Mounted);
        assert_eq!(ignored.stance, RiderStance::MountedAirborne);
        assert!(ignored.interact_consumed);
        assert!(ignored.dive_id.is_none());
    }

    #[test]
    fn k03_cone_boundaries_fallbacks_and_opposite_tie_are_pinned() {
        for angle in [
            0, 74_999, 75_000, 75_001, 90_000, -74_999, -75_000, -75_001, -90_000,
        ] {
            let launch = clamp_launch_direction([0, 0, -8_000], Some(direction(angle)));
            assert!(
                (launch.requested_angle_millidegrees - angle).abs() <= 1,
                "{angle}"
            );
            let expected = angle.clamp(-75_000, 75_000);
            assert!(
                (launch.clamped_angle_millidegrees - expected).abs() <= 1,
                "{angle}"
            );
            assert_eq!(launch.direction_was_clamped, angle.abs() > 75_000);
        }
        let opposite = clamp_launch_direction([0, 0, -8_000], Some(direction(180_000)));
        assert_eq!(opposite.requested_angle_millidegrees, 180_000);
        assert_eq!(opposite.clamped_angle_millidegrees, 75_000);

        for fallback in [
            None,
            Some(QuantizedDirection::new(0, DIRECTION_UNITS, 0)),
            Some(QuantizedDirection::new(0, 0, 0)),
        ] {
            let launch = clamp_launch_direction([6_400, 0, -4_800], fallback);
            assert_eq!(launch.requested_angle_millidegrees, 0);
            assert_eq!(launch.requested_direction, launch.clamped_direction);
        }
    }

    #[test]
    fn k04_launch_is_additive_and_godot_forward_left_right_are_stable() {
        let forward = clamp_launch_direction([0, 0, -8_000], Some(direction(0)));
        assert_eq!(
            forward.clamped_direction,
            QuantizedDirection::new(0, 0, -1_000_000)
        );
        let right = clamp_launch_direction([0, 0, -8_000], Some(direction(-90_000)));
        let left = clamp_launch_direction([0, 0, -8_000], Some(direction(90_000)));
        assert!(right.clamped_direction.x > 0);
        assert!(left.clamped_direction.x < 0);

        let mut kernel = SaddleDiveKernel::new(60, player(), 7).unwrap();
        let output = kernel.begin_tick(tick_input(1, 8_000, true)).unwrap();
        assert_eq!(
            output.requested_rider_velocity_mmps,
            Some([0, 6_000, -14_000])
        );
        let row = kernel.instrumentation_row(output.dive_id.unwrap()).unwrap();
        assert_eq!(row.horizontal_impulse_mmps, 6_000);
        assert_eq!(row.vertical_pop_mmps, 6_000);
        assert_eq!(row.resulting_planar_speed_mmps, 14_000);
        assert_eq!(row.resulting_total_speed_mmps, 15_232);
    }

    #[test]
    fn k05_time_ballistics_and_sway_boundaries_hold_at_supported_rates() {
        for (hz, nominal, decay) in [(30, 23, 18), (60, 45, 36), (120, 89, 71)] {
            assert_eq!(nominal_airtime_ticks(hz), nominal);
            assert_eq!(sway_decay_start_tick(nominal), decay);
            assert_eq!(dive_sway_scale_milli(decay, nominal), 600);
            assert!(dive_sway_scale_milli(decay + 1, nominal) > 600);
            assert_eq!(dive_sway_scale_milli(nominal, nominal), 1_000);
            assert_eq!(dive_sway_scale_milli(nominal + 10, nominal), 1_000);
            assert_eq!(airborne_vertical_velocity_mmps(0, hz), 6_000);
            assert_eq!(airborne_vertical_velocity_mmps(u64::from(hz), hz), -16_000);
        }
        assert_eq!(NOMINAL_AIRTIME_TICKS_60_HZ, nominal_airtime_ticks(60));
        assert_eq!(DIVE_SWAY_DECAY_START_TICK_60_HZ, sway_decay_start_tick(45));
    }

    #[test]
    fn k06_normal_and_bad_recovery_boundaries_and_tick_jumps_are_exact() {
        for (slope, prone_end, ready) in [(30_000, 24, 48), (30_001, 48, 72)] {
            let (mut kernel, dive_id) = launch_kernel();
            land(&mut kernel, 20, slope, LandingTerrain::Flat);
            for elapsed in 1..ready {
                let output = kernel
                    .begin_tick(tick_input(20 + elapsed, 0, false))
                    .unwrap();
                if elapsed < prone_end {
                    assert_eq!(output.state, SaddleDiveState::LandingProne);
                    assert_eq!(output.movement_input_scale_milli, 0);
                } else {
                    assert_eq!(output.state, SaddleDiveState::LandingRecovery);
                    assert_eq!(output.movement_input_scale_milli, 500);
                }
                assert!(!output.can_fire);
            }
            let complete = kernel.begin_tick(tick_input(20 + ready, 0, false)).unwrap();
            assert_eq!(complete.state, SaddleDiveState::OnFootReady);
            assert_eq!(complete.movement_input_scale_milli, 1_000);
            assert!(!complete.can_fire);
            assert_eq!(complete.dive_id, Some(dive_id));
        }

        let (mut jumped, _) = launch_kernel();
        land(&mut jumped, 20, 30_001, LandingTerrain::Flat);
        let jumped_output = jumped.begin_tick(tick_input(200, 0, false)).unwrap();
        assert_eq!(jumped_output.state, SaddleDiveState::OnFootReady);
        assert_eq!(jumped_output.effects.transitions.len(), 2);
        assert_eq!(
            jumped_output.effects.transitions[0].tick,
            SimulationTick::new(68)
        );
        assert_eq!(
            jumped_output.effects.transitions[1].tick,
            SimulationTick::new(92)
        );
    }

    #[test]
    fn k07_landing_threshold_damage_and_duplicate_observation_are_exact() {
        for (slope, bad) in [(29_999, false), (30_000, false), (30_001, true)] {
            let (mut kernel, dive_id) = launch_kernel();
            let effects = land(&mut kernel, 20, slope, LandingTerrain::Scrub);
            let row = kernel.instrumentation_row(dive_id).unwrap();
            assert!((row.landing_slope_millidegrees.unwrap() as i32 - slope as i32).abs() <= 1);
            assert_eq!(
                row.landing_outcome,
                Some(if bad {
                    LandingOutcome::Bad
                } else {
                    LandingOutcome::Good
                })
            );
            assert_eq!(row.landing_damage, if bad { 15 } else { 0 });
            assert_eq!(
                effects
                    .commands
                    .iter()
                    .filter(|command| matches!(command, SaddleDiveCommand::ApplyRiderDamage(_)))
                    .count(),
                usize::from(bad)
            );
            assert_eq!(kernel.rider_health(), if bad { 85 } else { 100 });
            let duplicate = kernel.resolve_motion(RiderMotionObservation {
                tick: SimulationTick::new(20),
                rider_position: QuantizedOrigin::default(),
                rider_velocity_mmps: [0; 3],
                descending: true,
                landing_normal: Some(direction_for_slope(slope)),
                landing_terrain: LandingTerrain::Mud,
            });
            assert_eq!(duplicate, Err(SaddleDiveError::MotionAlreadyResolved));
            assert_eq!(
                kernel.instrumentation_row(dive_id).unwrap().landing_damage,
                if bad { 15 } else { 0 }
            );
        }
    }

    #[test]
    fn k08_horse_decelerates_linearly_uses_collision_travel_and_never_exceeds_cap() {
        let mut horse = HorseRunoutKernel::new(60).unwrap();
        horse
            .start_runout(
                SimulationTick::new(10),
                QuantizedOrigin::default(),
                [0, 0, -14_000],
            )
            .unwrap();
        assert!(!horse.is_retrievable());
        let first = horse.begin_tick(SimulationTick::new(10)).unwrap();
        assert_eq!(first.requested_velocity_mmps, [0, 0, -14_000]);
        horse
            .resolve_motion(
                SimulationTick::new(10),
                QuantizedOrigin::new(0, 0, -i32::try_from(first.maximum_step_mm).unwrap()),
                [0; 3],
            )
            .unwrap();
        let middle = horse.begin_tick(SimulationTick::new(70)).unwrap();
        assert_eq!(middle.requested_velocity_mmps, [0, 0, -7_000]);
        // Collision feedback may shorten and redirect the segment, but its
        // actual planar displacement remains bounded by the request.
        horse
            .resolve_motion(
                SimulationTick::new(70),
                QuantizedOrigin::new(60, 0, -314),
                [0; 3],
            )
            .unwrap();
        assert_eq!(horse.cumulative_travel_mm(), first.maximum_step_mm + 100);
        let stop = horse.begin_tick(SimulationTick::new(130)).unwrap();
        assert_eq!(stop.state, HorseRunoutState::IdleRetrievable);
        assert_eq!(stop.transition.unwrap().tick, SimulationTick::new(130));
        assert!(horse.is_retrievable());

        let mut capped = HorseRunoutKernel::new(60).unwrap();
        capped
            .start_runout(
                SimulationTick::new(0),
                QuantizedOrigin::default(),
                [0, 0, -100_000],
            )
            .unwrap();
        let first_cap = capped.begin_tick(SimulationTick::new(1)).unwrap();
        assert_eq!(
            capped.resolve_motion(
                SimulationTick::new(1),
                QuantizedOrigin::new(20_000, 0, 20_000),
                [0; 3],
            ),
            Err(HorseRunoutError::MotionExceedsMaximumStep)
        );
        assert_eq!(capped.cumulative_travel_mm(), 0);
        let impossible = QuantizedOrigin::new(20_000, 0, 20_000);
        let mut position = capped.clamp_motion_position(impossible);
        assert_ne!(position, impossible);
        assert!(
            rounded_planar_distance(QuantizedOrigin::default(), position)
                <= first_cap.maximum_step_mm
        );
        capped
            .resolve_motion(SimulationTick::new(1), position, [0; 3])
            .unwrap();
        for tick in 2..=120 {
            if capped.is_retrievable() {
                break;
            }
            let output = capped.begin_tick(SimulationTick::new(tick)).unwrap();
            let step = i32::try_from(output.maximum_step_mm).unwrap();
            position.z -= step;
            capped
                .resolve_motion(SimulationTick::new(tick), position, [0; 3])
                .unwrap();
        }
        assert_eq!(capped.cumulative_travel_mm(), 25_000);
        assert_eq!(capped.last_position, position);
        assert!(
            rounded_planar_distance(QuantizedOrigin::default(), capped.last_position)
                <= HORSE_MAX_TRAVEL_MM
        );
        assert!(capped.is_retrievable());
        assert_eq!(
            capped.complete_remount(SimulationTick::new(2)).unwrap().to,
            HorseRunoutState::PlayerControlled
        );
    }

    #[test]
    fn k13_style_events_use_stored_shot_state_and_deduplicate_results() {
        let mut ledger = ShotAttributionLedger::default();
        let dive_id = DiveId::new(9).unwrap();
        let dive_shot = AcceptedShotMetadata {
            shooter: player(),
            tick: SimulationTick::new(50),
            accepted_shot_index: 12,
            weapon_id: WeaponId::Longspur,
            stance: RiderStance::SaddleDiveAirborne,
            gait: CombatGait::Gallop,
            dive_id: Some(dive_id),
            prelaunch_horizontal_velocity_mmps: [0, -8_000],
        };
        assert!(ledger.record_accepted(dive_shot));
        assert!(!ledger.record_accepted(dive_shot));
        let result = ShotResult {
            tick: dive_shot.tick,
            shooter_peer_id: player(),
            weapon_id: WeaponId::Longspur,
            outcome: ShotOutcome::Hit,
            rejection_reason: None,
            resolved_direction: Some(QuantizedDirection::new(0, 0, DIRECTION_UNITS)),
            target_id: Some(EntityId(4)),
            hit_zone: Some(HitZone::Head),
            damage: 57,
            distance_mm: Some(10_000),
            eliminated: false,
        };
        let attributed = ledger.observe_result(7, &result);
        assert_eq!(
            attributed
                .events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![
                GameplayEventKind::SaddleDiveHeadshot,
                GameplayEventKind::AirborneReversal
            ]
        );
        assert!(attributed
            .events
            .iter()
            .all(|event| event.id.sequence == 12));
        let replay = ledger.observe_result(7, &result);
        assert!(replay.duplicate);
        assert!(replay.events.is_empty());

        let mounted = AcceptedShotMetadata {
            shooter: player(),
            tick: SimulationTick::new(60),
            accepted_shot_index: 13,
            weapon_id: WeaponId::Dustwalker,
            stance: RiderStance::Mounted,
            gait: CombatGait::Gallop,
            dive_id: None,
            prelaunch_horizontal_velocity_mmps: [0; 2],
        };
        assert!(ledger.record_accepted(mounted));
        let mut mounted_result = result.clone();
        mounted_result.tick = mounted.tick;
        mounted_result.weapon_id = mounted.weapon_id;
        mounted_result.hit_zone = Some(HitZone::Body);
        assert_eq!(
            ledger.observe_result(7, &mounted_result).events[0].kind,
            GameplayEventKind::FullGallopHit
        );

        let body_forward = AcceptedShotMetadata {
            shooter: player(),
            tick: SimulationTick::new(61),
            accepted_shot_index: 14,
            weapon_id: WeaponId::Dustwalker,
            stance: RiderStance::SaddleDiveAirborne,
            gait: CombatGait::Gallop,
            dive_id: Some(dive_id),
            prelaunch_horizontal_velocity_mmps: [0, -8_000],
        };
        assert!(ledger.record_accepted(body_forward));
        let mut body_result = mounted_result.clone();
        body_result.tick = body_forward.tick;
        body_result.outcome = ShotOutcome::Hit;
        body_result.hit_zone = Some(HitZone::Body);
        body_result.resolved_direction = Some(QuantizedDirection::new(0, 0, -DIRECTION_UNITS));
        assert!(ledger.observe_result(7, &body_result).events.is_empty());

        let miss = AcceptedShotMetadata {
            tick: SimulationTick::new(62),
            accepted_shot_index: 15,
            ..body_forward
        };
        assert!(ledger.record_accepted(miss));
        let mut miss_result = body_result.clone();
        miss_result.tick = miss.tick;
        miss_result.outcome = ShotOutcome::Miss;
        miss_result.hit_zone = Some(HitZone::Head);
        miss_result.resolved_direction = Some(QuantizedDirection::new(0, 0, DIRECTION_UNITS));
        assert!(ledger.observe_result(7, &miss_result).events.is_empty());

        let ordinary_jump = AcceptedShotMetadata {
            tick: SimulationTick::new(63),
            accepted_shot_index: 16,
            stance: RiderStance::MountedAirborne,
            dive_id: None,
            ..body_forward
        };
        assert!(!ledger.record_accepted(ordinary_jump));
        let mut jump_result = body_result.clone();
        jump_result.tick = ordinary_jump.tick;
        assert!(ledger.observe_result(7, &jump_result).events.is_empty());

        let unknown = AcceptedShotMetadata {
            tick: SimulationTick::new(64),
            accepted_shot_index: 17,
            stance: RiderStance::Unknown(200),
            dive_id: None,
            ..body_forward
        };
        assert!(!ledger.record_accepted(unknown));
        let mut unlinked = body_result;
        unlinked.tick = SimulationTick::new(999);
        assert!(ledger.observe_result(7, &unlinked).events.is_empty());
    }

    #[test]
    fn k14_cloned_kernels_replay_identically() {
        let mut first = SaddleDiveKernel::new(60, player(), 7).unwrap();
        let mut second = first.clone();
        for tick in 1..=200 {
            let input = tick_input(tick, if tick == 1 { 8_000 } else { 0 }, tick == 1);
            let left = first.begin_tick(input);
            let right = second.begin_tick(input);
            assert_eq!(left, right, "begin {tick}");
            if tick == 45 {
                let observation = RiderMotionObservation {
                    tick: SimulationTick::new(tick),
                    rider_position: QuantizedOrigin::new(0, 0, -4_000),
                    rider_velocity_mmps: [0, -4_000, -8_000],
                    descending: true,
                    landing_normal: Some(direction_for_slope(30_001)),
                    landing_terrain: LandingTerrain::Riverbed,
                };
                assert_eq!(
                    first.resolve_motion(observation),
                    second.resolve_motion(observation)
                );
            }
        }
        assert_eq!(first, second);
    }

    #[test]
    fn k15_instrumentation_records_late_results_and_inclusive_damage_window() {
        let (mut kernel, dive_id) = launch_kernel();
        let launch = kernel.instrumentation_row(dive_id).unwrap();
        assert_eq!(launch.schema_version, 1);
        assert_eq!(launch.authority_epoch, 7);
        assert_eq!(launch.launch_tick, SimulationTick::new(10));
        assert_eq!(launch.prelaunch_velocity_mmps, [0, -8_000]);
        assert_eq!(launch.prelaunch_speed_mmps, 8_000);
        assert_eq!(launch.horizontal_impulse_mmps, 6_000);
        assert_eq!(launch.vertical_pop_mmps, 6_000);
        assert_eq!(launch.launch_height_mm, 1_600);
        assert_eq!(launch.nominal_airtime_ticks, 45);

        kernel.begin_tick(tick_input(11, 0, false)).unwrap();
        assert_eq!(
            kernel
                .record_shot_attempt(SimulationTick::new(11))
                .telemetry_updates
                .len(),
            1
        );
        assert!(kernel
            .record_shot_attempt(SimulationTick::new(11))
            .telemetry_updates
            .is_empty());
        let accepted = AcceptedShotMetadata {
            shooter: player(),
            tick: SimulationTick::new(11),
            accepted_shot_index: 3,
            weapon_id: WeaponId::Dustwalker,
            stance: RiderStance::SaddleDiveAirborne,
            gait: CombatGait::Gallop,
            dive_id: Some(dive_id),
            prelaunch_horizontal_velocity_mmps: [0, -8_000],
        };
        assert_eq!(
            kernel
                .record_accepted_shot(accepted)
                .telemetry_updates
                .len(),
            1
        );
        assert!(kernel
            .record_accepted_shot(accepted)
            .telemetry_updates
            .is_empty());

        land(&mut kernel, 20, 30_000, LandingTerrain::Riverbed);
        let mut ledger = ShotAttributionLedger::default();
        assert!(ledger.record_accepted(accepted));
        let result = ShotResult {
            tick: accepted.tick,
            shooter_peer_id: player(),
            weapon_id: accepted.weapon_id,
            outcome: ShotOutcome::Hit,
            rejection_reason: None,
            resolved_direction: Some(QuantizedDirection::new(0, 0, DIRECTION_UNITS)),
            target_id: Some(EntityId(8)),
            hit_zone: Some(HitZone::Head),
            damage: 28,
            distance_mm: Some(25_000),
            eliminated: false,
        };
        let attribution = ledger.observe_result(7, &result);
        let result_effects = kernel.record_authority_result(&attribution);
        assert_eq!(result_effects.events.len(), 2);
        assert!(kernel
            .record_authority_result(&ledger.observe_result(7, &result))
            .telemetry_updates
            .is_empty());

        let damage = |tick, sequence, amount| DamageObservation {
            id: DamageObservationId {
                authority_epoch: 7,
                actor: player(),
                tick: SimulationTick::new(tick),
                sequence,
            },
            amount,
        };
        let at_landing = damage(20, 1, 5);
        kernel.apply_external_damage(at_landing);
        assert!(kernel
            .apply_external_damage(at_landing)
            .telemetry_updates
            .is_empty());
        kernel.apply_external_damage(damage(200, 2, 7));
        kernel.apply_external_damage(damage(201, 3, 9));

        let row = kernel.instrumentation_row(dive_id).unwrap();
        assert_eq!(row.airtime_ticks, Some(10));
        assert_eq!(row.shot_attempts, 1);
        assert_eq!(row.shots_fired, 1);
        assert_eq!(row.shots_hit, 1);
        assert_eq!(row.headshots, 1);
        assert_eq!(row.reversal_hits, 1);
        assert_eq!(row.damage_dealt, 28);
        assert_eq!(row.landing_terrain, Some(LandingTerrain::Riverbed));
        assert_eq!(row.landing_outcome, Some(LandingOutcome::Good));
        assert_eq!(row.landing_damage, 0);
        assert_eq!(row.damage_taken_landing_through_3s, 12);
        assert_eq!(kernel.rider_health(), 79);

        let (mut precontact, precontact_id) = launch_kernel();
        precontact.begin_tick(tick_input(20, 0, false)).unwrap();
        precontact.apply_external_damage(damage(20, 9, 5));
        assert_eq!(
            precontact
                .instrumentation_row(precontact_id)
                .unwrap()
                .damage_taken_landing_through_3s,
            0
        );
        land(&mut precontact, 20, 30_000, LandingTerrain::Flat);
        assert_eq!(
            precontact
                .instrumentation_row(precontact_id)
                .unwrap()
                .damage_taken_landing_through_3s,
            5
        );
    }

    #[test]
    fn telemetry_waits_for_accepted_results_before_one_final_snapshot() {
        let (mut kernel, dive_id) = launch_kernel();
        kernel.begin_tick(tick_input(11, 0, false)).unwrap();
        let accepted = AcceptedShotMetadata {
            shooter: player(),
            tick: SimulationTick::new(11),
            accepted_shot_index: 1,
            weapon_id: WeaponId::Dustwalker,
            stance: RiderStance::SaddleDiveAirborne,
            gait: CombatGait::Gallop,
            dive_id: Some(dive_id),
            prelaunch_horizontal_velocity_mmps: [0, -8_000],
        };
        kernel.record_accepted_shot(accepted);
        land(&mut kernel, 20, 30_000, LandingTerrain::Flat);
        kernel.begin_tick(tick_input(68, 0, false)).unwrap();
        let mut remount = tick_input(69, 0, true);
        remount.horse_retrievable = true;
        remount.horse_position = remount.rider_position;
        kernel.begin_tick(remount).unwrap();
        kernel.begin_tick(tick_input(201, 0, false)).unwrap();
        let settled = kernel.settle_observations_through(SimulationTick::new(200));
        assert!(settled.telemetry_finalized.is_empty());

        let mut ledger = ShotAttributionLedger::default();
        assert!(ledger.record_accepted(accepted));
        let result = ShotResult {
            tick: accepted.tick,
            shooter_peer_id: player(),
            weapon_id: accepted.weapon_id,
            outcome: ShotOutcome::Miss,
            rejection_reason: None,
            resolved_direction: Some(direction(0)),
            target_id: None,
            hit_zone: None,
            damage: 0,
            distance_mm: None,
            eliminated: false,
        };
        let finalized = kernel.record_authority_result(&ledger.observe_result(7, &result));
        assert_eq!(finalized.telemetry_finalized.len(), 1);
        assert!(kernel
            .record_authority_result(&ledger.observe_result(7, &result))
            .telemetry_finalized
            .is_empty());
    }

    #[test]
    fn k15_instrumentation_inclusive_death_remount_and_censors_are_exact() {
        for (elapsed, expected) in [(0, true), (180, true), (181, false)] {
            let (mut kernel, dive_id) = launch_kernel();
            land(&mut kernel, 20, 30_000, LandingTerrain::Mud);
            let death = kernel.observe_death(SimulationTick::new(20 + elapsed));
            let row = kernel.instrumentation_row(dive_id).unwrap();
            assert_eq!(row.death_within_3s, Some(expected));
            assert_eq!(row.censor_reason, Some(DiveCensorReason::DiedBeforeRemount));
            assert_eq!(death.telemetry_finalized.len(), 1);
        }

        let (mut remounted, dive_id) = launch_kernel();
        land(&mut remounted, 20, 30_000, LandingTerrain::Flat);
        remounted.begin_tick(tick_input(68, 0, false)).unwrap();
        let mut remount_input = tick_input(69, 0, true);
        remount_input.horse_retrievable = true;
        remount_input.horse_position = remount_input.rider_position;
        let remount = remounted.begin_tick(remount_input).unwrap();
        assert_eq!(remount.state, SaddleDiveState::Mounted);
        let row = remounted.instrumentation_row(dive_id).unwrap();
        assert_eq!(row.remount_tick, Some(SimulationTick::new(69)));
        assert_eq!(row.time_to_remount_ticks, Some(49));
        assert!(remount.effects.telemetry_finalized.is_empty());
        let resolved = remounted.begin_tick(tick_input(201, 0, false)).unwrap();
        assert!(resolved.effects.telemetry_finalized.is_empty());
        assert_eq!(
            remounted
                .instrumentation_row(dive_id)
                .unwrap()
                .death_within_3s,
            None
        );
        let settled = remounted.settle_observations_through(SimulationTick::new(200));
        assert_eq!(settled.telemetry_finalized.len(), 1);
        assert_eq!(
            remounted
                .instrumentation_row(dive_id)
                .unwrap()
                .death_within_3s,
            Some(false)
        );

        let (mut delayed_boundary, delayed_id) = launch_kernel();
        land(&mut delayed_boundary, 20, 30_000, LandingTerrain::Flat);
        delayed_boundary
            .begin_tick(tick_input(68, 0, false))
            .unwrap();
        let mut delayed_remount = tick_input(69, 0, true);
        delayed_remount.horse_retrievable = true;
        delayed_remount.horse_position = delayed_remount.rider_position;
        delayed_boundary.begin_tick(delayed_remount).unwrap();
        delayed_boundary
            .begin_tick(tick_input(201, 0, false))
            .unwrap();
        let delayed = delayed_boundary.observe_death(SimulationTick::new(200));
        assert_eq!(
            delayed_boundary
                .instrumentation_row(delayed_id)
                .unwrap()
                .death_within_3s,
            Some(true)
        );
        assert_eq!(delayed.telemetry_finalized.len(), 1);

        for (reason, action) in [
            (DiveCensorReason::MatchEnded, 0_u8),
            (DiveCensorReason::Reset, 1),
            (DiveCensorReason::NotObserved, 2),
        ] {
            let (mut kernel, dive_id) = launch_kernel();
            let effects = match action {
                0 => kernel.end_match(SimulationTick::new(11)),
                1 => kernel.reset(SimulationTick::new(11)),
                _ => kernel.censor_not_observed(),
            };
            assert_eq!(
                kernel.instrumentation_row(dive_id).unwrap().censor_reason,
                Some(reason)
            );
            assert_eq!(effects.telemetry_finalized.len(), 1);
        }

        let (mut airborne_death, dive_id) = launch_kernel();
        airborne_death.observe_death(SimulationTick::new(11));
        assert_eq!(airborne_death.rider_health(), 0);
        assert_eq!(
            airborne_death
                .instrumentation_row(dive_id)
                .unwrap()
                .censor_reason,
            Some(DiveCensorReason::DiedAirborne)
        );
    }

    #[test]
    fn malformed_order_actor_epoch_and_non_upward_contacts_do_not_mutate() {
        let mut kernel = SaddleDiveKernel::new(60, player(), 7).unwrap();
        let mut wrong_actor = tick_input(1, 8_000, true);
        wrong_actor.actor = PlayerId::parse("00000000-0000-4000-8000-000000000003").unwrap();
        assert_eq!(
            kernel.begin_tick(wrong_actor),
            Err(SaddleDiveError::ActorMismatch)
        );
        let mut wrong_epoch = tick_input(1, 8_000, true);
        wrong_epoch.authority_epoch = 8;
        assert_eq!(
            kernel.begin_tick(wrong_epoch),
            Err(SaddleDiveError::AuthorityEpochMismatch)
        );
        assert!(kernel.current_tick().is_none());

        kernel.begin_tick(tick_input(1, 8_000, true)).unwrap();
        assert_eq!(
            kernel.begin_tick(tick_input(1, 8_000, false)),
            Err(SaddleDiveError::TickReplay)
        );
        let no_landing = kernel
            .resolve_motion(RiderMotionObservation {
                tick: SimulationTick::new(1),
                rider_position: QuantizedOrigin::default(),
                rider_velocity_mmps: [0, -1, 0],
                descending: true,
                landing_normal: Some(QuantizedDirection::new(0, -DIRECTION_UNITS, 0)),
                landing_terrain: LandingTerrain::Flat,
            })
            .unwrap();
        assert!(no_landing.transitions.is_empty());
        assert_eq!(kernel.state(), SaddleDiveState::SaddleDiveAirborne);
    }
}
