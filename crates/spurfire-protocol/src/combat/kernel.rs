//! Pure fixed-tick combat state, deterministic spread, and authority validation.

use std::collections::{btree_map::Entry, BTreeMap, VecDeque};
use std::f64::consts::{PI, TAU};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    dive_shot_cap, dive_sway_scale_milli, AcceptedShotMetadata, DiveId, GameplayEventRow,
    RiderStance, ShotAttributionLedger, CHARGE_SWAY_MULTIPLIER_MILLI,
};

use super::*;

const ADS_SPREAD_NUMERATOR: u64 = 3;
const ADS_SPREAD_DENOMINATOR: u64 = 5;
const MAX_TURN_RATE_MILLIDEGREES_PER_SECOND: u32 = 120_000;
const MAX_TURN_SPREAD_PENALTY_DIVISOR: u64 = 4;
const AIRBORNE_SPREAD_PENALTY_NUMERATOR: u64 = 3;
const AIRBORNE_SPREAD_PENALTY_DENOMINATOR: u64 = 2;

/// Invalid fixed simulation frequency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("tick rate must be greater than zero")]
pub struct InvalidTickRate;

/// Ammo held for one owned weapon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponAmmo {
    /// Rounds currently loaded.
    pub magazine: u16,
    /// Rounds carried outside the magazine.
    pub reserve: u16,
}

impl WeaponAmmo {
    /// A full magazine and full reserve for a weapon row.
    #[must_use]
    pub const fn full(stats: WeaponStats) -> Self {
        Self {
            magazine: stats.magazine_capacity,
            reserve: stats.reserve_capacity,
        }
    }

    fn clamped(self, stats: WeaponStats) -> Self {
        Self {
            magazine: self.magazine.min(stats.magazine_capacity),
            reserve: self.reserve.min(stats.reserve_capacity),
        }
    }
}

/// Rewound rider handling inputs consumed by the weapon kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RidingState {
    /// Sole authoritative mounted/airborne source of truth.
    pub stance: RiderStance,
    /// Authority-owned dive ID, present exactly for `SaddleDiveAirborne`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dive_id: Option<DiveId>,
    /// Current discrete gait.
    pub gait: CombatGait,
    /// Planar mount or launch-time speed in millimetres per second.
    pub planar_speed_mmps: u32,
    /// Full-speed denominator for spread interpolation.
    pub gait_top_speed_mmps: u32,
    /// Signed yaw rate in thousandths of a degree per second.
    pub yaw_rate_millidegrees_per_second: i32,
    /// Horse/rider is in a stumble recovery frame.
    pub stumbling: bool,
    /// Aim-down-sights is held.
    pub ads: bool,
    /// Sprint-gallop input is held and pauses reload time.
    pub sprint_gallop: bool,
    /// Authority-owned M4 Majestic Charge window.
    #[serde(default)]
    pub majestic_charge: bool,
}

impl Default for RidingState {
    fn default() -> Self {
        Self {
            stance: RiderStance::Mounted,
            dive_id: None,
            gait: CombatGait::Idle,
            planar_speed_mmps: 0,
            gait_top_speed_mmps: 1,
            yaw_rate_millidegrees_per_second: 0,
            stumbling: false,
            ads: false,
            sprint_gallop: false,
            majestic_charge: false,
        }
    }
}

impl RidingState {
    /// Whether stance and authority dive context agree.
    #[must_use]
    pub const fn is_consistent(self) -> bool {
        matches!(self.stance, RiderStance::SaddleDiveAirborne) == self.dive_id.is_some()
            && !matches!(self.stance, RiderStance::Unknown(_))
    }

    /// Speed ratio in thousandths, clamped to the design range.
    #[must_use]
    pub fn speed_ratio_milli(self) -> u32 {
        if self.gait_top_speed_mmps == 0 {
            return 0;
        }
        let ratio = u64::from(self.planar_speed_mmps)
            .saturating_mul(1_000)
            .div_ceil(u64::from(self.gait_top_speed_mmps));
        u32::try_from(ratio.min(1_000)).unwrap_or(1_000)
    }
}

/// Deterministic two-axis riding sway sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwaySample {
    /// Pitch offset in thousandths of a degree.
    pub pitch_millidegrees: i32,
    /// Yaw offset in thousandths of a degree.
    pub yaw_millidegrees: i32,
}

impl SwaySample {
    /// Largest absolute axis, suitable for compact telemetry.
    #[must_use]
    pub fn magnitude_millidegrees(self) -> u32 {
        self.pitch_millidegrees
            .unsigned_abs()
            .max(self.yaw_millidegrees.unsigned_abs())
    }
}

/// Current camera recoil accumulator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoilState {
    /// Upward camera kick remaining in thousandths of a degree.
    pub pitch_millidegrees: i32,
    /// Signed camera yaw kick remaining in thousandths of a degree.
    pub yaw_millidegrees: i32,
}

impl RecoilState {
    fn recover(&mut self, elapsed_ticks: u64, tick_rate: u32) {
        let amount = elapsed_ticks
            .saturating_mul(u64::from(RECOIL_RECOVERY_MILLIDEGREES_PER_SECOND))
            / u64::from(tick_rate);
        let amount = i32::try_from(amount).unwrap_or(i32::MAX);
        self.pitch_millidegrees = move_toward_zero(self.pitch_millidegrees, amount);
        self.yaw_millidegrees = move_toward_zero(self.yaw_millidegrees, amount);
    }

    fn apply_impulse(&mut self, stats: WeaponStats, seed: u64) {
        let pitch = i32::try_from(stats.recoil_vertical_millidegrees).unwrap_or(i32::MAX);
        self.pitch_millidegrees = self.pitch_millidegrees.saturating_add(pitch);
        let yaw = deterministic_signed(seed ^ 0xd1b5_4a32_d192_ed03, stats.recoil_yaw_millidegrees);
        self.yaw_millidegrees = self.yaw_millidegrees.saturating_add(yaw);
    }
}

fn move_toward_zero(value: i32, amount: i32) -> i32 {
    if value > 0 {
        value.saturating_sub(amount).max(0)
    } else if value < 0 {
        value.saturating_add(amount).min(0)
    } else {
        0
    }
}

/// Public reload snapshot for HUD and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadSnapshot {
    /// Weapon being reloaded.
    pub weapon_id: WeaponId,
    /// Active (unpaused) ticks accumulated.
    pub active_ticks: u64,
    /// Active ticks required to finish.
    pub required_ticks: u64,
}

impl ReloadSnapshot {
    /// Progress in thousandths.
    #[must_use]
    pub fn progress_milli(self) -> u16 {
        if self.required_ticks == 0 {
            return 1_000;
        }
        let progress = self.active_ticks.saturating_mul(1_000) / self.required_ticks;
        u16::try_from(progress.min(1_000)).unwrap_or(1_000)
    }
}

/// Result of advancing deterministic weapon time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdvanceOutcome {
    /// Reload crossed its completion tick.
    pub reload_completed: bool,
    /// Magazine/reserve changed during this advance.
    pub ammo_changed: bool,
}

/// Kernel time was asked to move backward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("simulation tick moved backward")]
pub struct TickRegression;

/// Why a reload could not begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ReloadStartError {
    /// Time cannot move backward.
    #[error("tick_replay")]
    TickReplay,
    /// Rifle is holstered or unavailable.
    #[error("holstered")]
    Holstered,
    /// Mounted jump or Saddle Dive reload is forbidden and mutates nothing.
    #[error("airborne")]
    Airborne,
    /// Rider is not mounted.
    #[error("dismounted")]
    Dismounted,
    /// A reload is already active.
    #[error("reloading")]
    AlreadyReloading,
    /// Magazine is already full.
    #[error("magazine_full")]
    MagazineFull,
    /// No reserve rounds are available.
    #[error("no_reserve")]
    NoReserve,
}

/// Accepted shot state before target resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedShot {
    /// Shot tick.
    pub tick: SimulationTick,
    /// Weapon used.
    pub weapon_id: WeaponId,
    /// Authority-derived spread seed.
    pub spread_seed: u64,
    /// Direction after deterministic sway and spread.
    pub resolved_direction: QuantizedDirection,
    /// Effective cone spread.
    pub spread_millidegrees: u32,
    /// Deterministic riding sway sample.
    pub sway: SwaySample,
    /// Ammo after consuming exactly one round.
    pub ammo: WeaponAmmo,
    /// Camera recoil after the shot impulse.
    pub recoil: RecoilState,
    /// Match-lifetime accepted-shot index used for seed and event identity.
    pub accepted_shot_index: u64,
    /// Authority-owned dive attribution for this acceptance.
    pub dive_id: Option<DiveId>,
}

/// A world rifle with retained ammunition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponPickup {
    /// Rifle type.
    pub weapon_id: WeaponId,
    /// Retained or spawn-granted ammo.
    pub ammo: WeaponAmmo,
    /// Tick at which the pickup appeared.
    pub spawned_at: SimulationTick,
    /// Dropped rifles expire; map-spawn rifles do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<SimulationTick>,
}

impl WeaponPickup {
    /// Creates a map spawn granting one magazine and half reserve.
    #[must_use]
    pub fn world_spawn(weapon_id: WeaponId, tick: SimulationTick) -> Self {
        let stats = *weapon_id.stats();
        Self {
            weapon_id,
            ammo: WeaponAmmo {
                magazine: stats.magazine_capacity,
                reserve: stats.reserve_capacity / 2,
            },
            spawned_at: tick,
            expires_at: None,
        }
    }

    /// Returns whether a dropped pickup has reached its despawn tick.
    #[must_use]
    pub fn is_expired(self, tick: SimulationTick) -> bool {
        self.expires_at.is_some_and(|expires| tick >= expires)
    }
}

/// Pickup interaction failed without mutating inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PickupError {
    /// Rider is farther than three metres from the crate/drop.
    #[error("out_of_range")]
    OutOfRange,
    /// Dropped rifle has reached its 30-second expiry.
    #[error("expired")]
    Expired,
    /// Dive-airborne weapon changes are locked.
    #[error("airborne")]
    Airborne,
}

/// Successful pickup behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickupOutcome {
    /// Duplicate weapon became reserve ammo, with no second inventory copy.
    AmmoOnly {
        /// Existing weapon receiving ammo.
        weapon_id: WeaponId,
        /// Reserve rounds added.
        added_reserve: u16,
        /// New capped reserve total.
        reserve: u16,
    },
    /// Equipped rifle changed and the old rifle became a timed drop.
    Swapped {
        /// Newly equipped rifle.
        weapon_id: WeaponId,
        /// Old rifle retaining its exact ammo for 30 seconds.
        dropped: WeaponPickup,
    },
}

/// Authority-owned per-dive firing context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiveFireContext {
    /// Current dive.
    pub dive_id: DiveId,
    /// Accepted movement launch tick.
    pub launch_tick: SimulationTick,
    /// Weapon locked at launch.
    pub launch_weapon: WeaponId,
    /// Number of ammo-consuming shots accepted in this dive.
    pub accepted_count: u16,
    /// Planar horse velocity captured at launch as `[x, z]`.
    pub prelaunch_horizontal_velocity_mmps: [i32; 2],
    /// Nominal sway schedule duration.
    pub nominal_airtime_ticks: u64,
    /// Landing closes new fire but preserves late-result attribution.
    pub closed_to_new_shots: bool,
    /// Authoritative gait/speed/turn/ADS handling captured at launch.
    pub launch_handling: RidingState,
}

/// Exact internal dive gate, kept out of the closed wire rejection enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiveFireRejection {
    /// Saddle Dive stance did not match the authority-owned context.
    ContextMismatch,
    /// Equipped weapon differs from the launch lock.
    WeaponMismatch,
    /// Landing already closed this context.
    Closed,
    /// Per-weapon accepted-shot cap was reached.
    ShotCap,
}

/// Detailed local/authority fire rejection. `wire_reason` remains compatible
/// with wire 1.0 readers while `dive_reason` retains M2 precision internally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FireRejection {
    /// Existing closed wire reason.
    pub wire_reason: ShotRejectionReason,
    /// Optional M2-only reason.
    pub dive_reason: Option<DiveFireRejection>,
}

impl FireRejection {
    const fn wire(wire_reason: ShotRejectionReason) -> Self {
        Self {
            wire_reason,
            dive_reason: None,
        }
    }

    const fn dive(wire_reason: ShotRejectionReason, dive_reason: DiveFireRejection) -> Self {
        Self {
            wire_reason,
            dive_reason: Some(dive_reason),
        }
    }
}

/// Invalid authority transition into or out of a dive fire context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum DiveContextError {
    /// Time cannot move backward.
    #[error("tick_replay")]
    TickReplay,
    /// Movement and combat selected different launch weapons.
    #[error("weapon_mismatch")]
    WeaponMismatch,
    /// A previous dive has not landed.
    #[error("dive_already_open")]
    DiveAlreadyOpen,
    /// Finish did not match the retained context.
    #[error("dive_context_mismatch")]
    ContextMismatch,
}

/// Pure deterministic ammo, cadence, reload, handling, spread, recoil, and pickup state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CombatKernel {
    tick_rate: u32,
    lobby_seed: u64,
    shooter_peer_id: PlayerId,
    inventory: BTreeMap<WeaponId, WeaponAmmo>,
    equipped: WeaponId,
    holstered: bool,
    reload: Option<ReloadSnapshot>,
    last_accepted_tick: Option<SimulationTick>,
    last_advanced_tick: SimulationTick,
    shot_index: u64,
    recoil: RecoilState,
    riding: RidingState,
    dive_context: Option<DiveFireContext>,
}

impl CombatKernel {
    /// Creates a mounted shooter carrying a full Dustwalker.
    pub fn new(
        tick_rate: u32,
        lobby_seed: u64,
        shooter_peer_id: PlayerId,
    ) -> Result<Self, InvalidTickRate> {
        Self::with_weapon(tick_rate, lobby_seed, shooter_peer_id, WeaponId::Dustwalker)
    }

    /// Creates a mounted shooter carrying one selected full rifle.
    pub fn with_weapon(
        tick_rate: u32,
        lobby_seed: u64,
        shooter_peer_id: PlayerId,
        weapon_id: WeaponId,
    ) -> Result<Self, InvalidTickRate> {
        if tick_rate == 0 {
            return Err(InvalidTickRate);
        }
        let mut inventory = BTreeMap::new();
        inventory.insert(weapon_id, WeaponAmmo::full(*weapon_id.stats()));
        Ok(Self {
            tick_rate,
            lobby_seed,
            shooter_peer_id,
            inventory,
            equipped: weapon_id,
            holstered: false,
            reload: None,
            last_accepted_tick: None,
            last_advanced_tick: SimulationTick::new(0),
            shot_index: 0,
            recoil: RecoilState::default(),
            riding: RidingState::default(),
            dive_context: None,
        })
    }

    /// Creates the Godot prototype loadout with every sidegrade available.
    pub fn with_full_loadout(
        tick_rate: u32,
        lobby_seed: u64,
        shooter_peer_id: PlayerId,
    ) -> Result<Self, InvalidTickRate> {
        let mut kernel = Self::new(tick_rate, lobby_seed, shooter_peer_id)?;
        for weapon_id in WeaponId::ALL {
            kernel
                .inventory
                .insert(weapon_id, WeaponAmmo::full(*weapon_id.stats()));
        }
        Ok(kernel)
    }

    /// Fixed simulation frequency.
    #[must_use]
    pub const fn tick_rate(&self) -> u32 {
        self.tick_rate
    }

    /// Stable shooter identity.
    #[must_use]
    pub const fn shooter_peer_id(&self) -> PlayerId {
        self.shooter_peer_id
    }

    /// Currently selected rifle.
    #[must_use]
    pub const fn equipped_weapon(&self) -> WeaponId {
        self.equipped
    }

    /// Whether the current rifle is holstered.
    #[must_use]
    pub const fn is_holstered(&self) -> bool {
        self.holstered
    }

    /// Accepted-shot index used to derive the next seed.
    #[must_use]
    pub const fn shot_index(&self) -> u64 {
        self.shot_index
    }

    /// Last ammo-consuming accepted shot, if any.
    #[must_use]
    pub const fn last_accepted_tick(&self) -> Option<SimulationTick> {
        self.last_accepted_tick
    }

    /// Current recoil accumulator.
    #[must_use]
    pub const fn recoil(&self) -> RecoilState {
        self.recoil
    }

    /// Current reload, if any.
    #[must_use]
    pub const fn reload(&self) -> Option<ReloadSnapshot> {
        self.reload
    }

    /// Last handling state applied while advancing.
    #[must_use]
    pub const fn riding_state(&self) -> RidingState {
        self.riding
    }

    /// Retained authority dive context, including a closed context needed by
    /// late shot-result attribution.
    #[must_use]
    pub const fn dive_fire_context(&self) -> Option<DiveFireContext> {
        self.dive_context
    }

    /// Restores the selected Alpha loadout at an M5 respawn without resetting
    /// replay receipts or the deterministic shot index.
    pub fn respawn_at(&mut self, tick: SimulationTick) -> bool {
        if tick < self.last_advanced_tick {
            return false;
        }
        for (weapon, ammo) in &mut self.inventory {
            *ammo = WeaponAmmo::full(*weapon.stats());
        }
        self.holstered = false;
        self.reload = None;
        self.recoil = RecoilState::default();
        self.riding = RidingState::default();
        self.dive_context = None;
        self.last_advanced_tick = tick;
        true
    }

    const fn has_open_dive_context(&self) -> bool {
        matches!(self.dive_context, Some(context) if !context.closed_to_new_shots)
    }

    /// Begins an authority-accepted Saddle Dive before same-tick fire/reload.
    /// Active reload progress is discarded without transferring rounds.
    pub fn begin_saddle_dive(
        &mut self,
        dive_id: DiveId,
        launch_tick: SimulationTick,
        launch_weapon: WeaponId,
        prelaunch_horizontal_velocity_mmps: [i32; 2],
        nominal_airtime_ticks: u64,
    ) -> Result<(), DiveContextError> {
        launch_tick
            .checked_duration_since(self.last_advanced_tick)
            .ok_or(DiveContextError::TickReplay)?;
        if launch_weapon != self.equipped {
            return Err(DiveContextError::WeaponMismatch);
        }
        if self
            .dive_context
            .is_some_and(|context| !context.closed_to_new_shots)
        {
            return Err(DiveContextError::DiveAlreadyOpen);
        }
        if self.riding.stance != RiderStance::Mounted || !self.riding.is_consistent() {
            return Err(DiveContextError::ContextMismatch);
        }

        let launch_handling = self.riding;
        // A jump in absolute ticks still completes reload boundaries strictly
        // before launch. A reload whose boundary is the launch tick is canceled
        // because accepted E processing precedes same-tick reload processing.
        if launch_tick > self.last_advanced_tick {
            let prelaunch_tick = SimulationTick::new(launch_tick.as_u64() - 1);
            if prelaunch_tick > self.last_advanced_tick {
                self.advance_to(prelaunch_tick, launch_handling)
                    .expect("prelaunch tick was validated as monotonic");
            }
            let final_elapsed = launch_tick
                .checked_duration_since(self.last_advanced_tick)
                .expect("prelaunch advance cannot pass launch");
            self.recoil.recover(final_elapsed, self.tick_rate);
        }
        self.reload = None;
        self.last_advanced_tick = launch_tick;
        self.riding.stance = RiderStance::SaddleDiveAirborne;
        self.riding.dive_id = Some(dive_id);
        self.dive_context = Some(DiveFireContext {
            dive_id,
            launch_tick,
            launch_weapon,
            accepted_count: 0,
            prelaunch_horizontal_velocity_mmps,
            nominal_airtime_ticks,
            closed_to_new_shots: false,
            launch_handling,
        });
        Ok(())
    }

    /// Closes new dive fire at first landing while retaining accepted-shot context.
    pub fn finish_saddle_dive(
        &mut self,
        dive_id: DiveId,
        landing_tick: SimulationTick,
    ) -> Result<(), DiveContextError> {
        if landing_tick < self.last_advanced_tick {
            return Err(DiveContextError::TickReplay);
        }
        let Some(context) = &mut self.dive_context else {
            return Err(DiveContextError::ContextMismatch);
        };
        if context.dive_id != dive_id {
            return Err(DiveContextError::ContextMismatch);
        }
        context.closed_to_new_shots = true;
        let elapsed = landing_tick
            .checked_duration_since(self.last_advanced_tick)
            .expect("landing regression was rejected");
        self.recoil.recover(elapsed, self.tick_rate);
        self.last_advanced_tick = landing_tick;
        self.riding.stance = RiderStance::LandingProne;
        self.riding.dive_id = None;
        self.holster();
        Ok(())
    }

    /// Ammo for an owned rifle.
    #[must_use]
    pub fn ammo(&self, weapon_id: WeaponId) -> Option<WeaponAmmo> {
        self.inventory.get(&weapon_id).copied()
    }

    /// Ammo for the equipped rifle.
    #[must_use]
    pub fn equipped_ammo(&self) -> WeaponAmmo {
        self.ammo(self.equipped).unwrap_or_default()
    }

    /// Grants or replaces an inventory rifle, clamping ammo to its row.
    pub fn grant_weapon(&mut self, weapon_id: WeaponId, ammo: WeaponAmmo) {
        self.inventory
            .insert(weapon_id, ammo.clamped(*weapon_id.stats()));
    }

    /// Refills the equipped rifle from an authority-observed ammo-wagon reward.
    pub fn refill_equipped_ammo(&mut self) -> WeaponAmmo {
        let ammo = WeaponAmmo::full(*self.equipped.stats());
        self.inventory.insert(self.equipped, ammo);
        self.reload = None;
        ammo
    }

    /// Selects an owned rifle and cancels any reload. Dive-airborne weapon
    /// switching is refused so it cannot reset or enlarge the shot cap.
    pub fn equip_weapon(&mut self, weapon_id: WeaponId) -> bool {
        if self.has_open_dive_context()
            || self.riding.stance == RiderStance::SaddleDiveAirborne
            || !self.inventory.contains_key(&weapon_id)
        {
            return false;
        }
        let changed = self.equipped != weapon_id || self.holstered;
        self.equipped = weapon_id;
        self.holstered = false;
        self.reload = None;
        changed
    }

    /// Holsters without discarding ammo or inventory.
    pub fn holster(&mut self) {
        self.holstered = true;
        self.reload = None;
    }

    /// Replaces ammo for an owned weapon, clamped to immutable capacities.
    pub fn set_ammo(&mut self, weapon_id: WeaponId, ammo: WeaponAmmo) -> bool {
        let Some(slot) = self.inventory.get_mut(&weapon_id) else {
            return false;
        };
        *slot = ammo.clamped(*weapon_id.stats());
        true
    }

    /// Seed the next accepted shot must carry.
    #[must_use]
    pub fn next_spread_seed(&self) -> u64 {
        shot_spread_seed(self.lobby_seed, self.shooter_peer_id, self.shot_index)
    }

    /// Advances reload and recoil to an absolute tick.
    pub fn advance_to(
        &mut self,
        tick: SimulationTick,
        riding: RidingState,
    ) -> Result<AdvanceOutcome, TickRegression> {
        let Some(elapsed_ticks) = tick.checked_duration_since(self.last_advanced_tick) else {
            return Err(TickRegression);
        };
        self.riding = riding;
        if !riding.stance.carries_mounted_weapon() {
            self.holster();
        }
        self.recoil.recover(elapsed_ticks, self.tick_rate);
        let outcome = self.advance_reload_by(elapsed_ticks, riding.sprint_gallop);
        self.last_advanced_tick = tick;
        Ok(outcome)
    }

    fn advance_reload_by(&mut self, elapsed_ticks: u64, paused: bool) -> AdvanceOutcome {
        let mut outcome = AdvanceOutcome::default();
        if let Some(mut reload) = self.reload {
            if !paused {
                reload.active_ticks = reload.active_ticks.saturating_add(elapsed_ticks);
            }
            if reload.active_ticks >= reload.required_ticks {
                let stats = *reload.weapon_id.stats();
                if let Some(ammo) = self.inventory.get_mut(&reload.weapon_id) {
                    let needed = stats.magazine_capacity.saturating_sub(ammo.magazine);
                    let loaded = needed.min(ammo.reserve);
                    ammo.magazine = ammo.magazine.saturating_add(loaded);
                    ammo.reserve = ammo.reserve.saturating_sub(loaded);
                    outcome.ammo_changed = loaded > 0;
                }
                self.reload = None;
                outcome.reload_completed = true;
            } else {
                self.reload = Some(reload);
            }
        }
        outcome
    }

    fn begin_reload(&mut self) -> Result<ReloadSnapshot, ReloadStartError> {
        if self.holstered {
            return Err(ReloadStartError::Holstered);
        }
        if self.reload.is_some() {
            return Err(ReloadStartError::AlreadyReloading);
        }
        let stats = *self.equipped.stats();
        let ammo = self.equipped_ammo();
        if ammo.magazine >= stats.magazine_capacity {
            return Err(ReloadStartError::MagazineFull);
        }
        if ammo.reserve == 0 {
            return Err(ReloadStartError::NoReserve);
        }
        let reload = ReloadSnapshot {
            weapon_id: self.equipped,
            active_ticks: 0,
            required_ticks: stats.reload_ticks(self.tick_rate),
        };
        self.reload = Some(reload);
        Ok(reload)
    }

    /// Starts the composed M3 authority reload without moving the shot-command
    /// clock. Actor inputs and rollback shot commands deliberately have
    /// separate monotonic clocks.
    pub(crate) fn request_m3_reload(&mut self) -> Result<ReloadSnapshot, ReloadStartError> {
        if self.has_open_dive_context() {
            return Err(ReloadStartError::Airborne);
        }
        self.begin_reload()
    }

    /// Advances only M3 reload time. This cannot make a subsequently arriving
    /// historical shot command fail solely because actor simulation is newer.
    pub(crate) fn advance_m3_reload(&mut self, elapsed_ticks: u64, paused: bool) -> AdvanceOutcome {
        self.advance_reload_by(elapsed_ticks, paused)
    }

    /// Restores an authenticated M3 reload snapshot after ammo/loadout state.
    pub(crate) fn restore_m3_reload(&mut self, reload: Option<ReloadSnapshot>) -> bool {
        if let Some(reload) = reload {
            let stats = *self.equipped.stats();
            let ammo = self.equipped_ammo();
            if self.holstered
                || reload.weapon_id != self.equipped
                || reload.required_ticks != stats.reload_ticks(self.tick_rate)
                || reload.active_ticks >= reload.required_ticks
                || ammo.magazine >= stats.magazine_capacity
                || ammo.reserve == 0
            {
                return false;
            }
        }
        self.reload = reload;
        true
    }

    /// Starts a reload at an absolute tick.
    pub fn request_reload(
        &mut self,
        tick: SimulationTick,
        riding: RidingState,
    ) -> Result<ReloadSnapshot, ReloadStartError> {
        if tick < self.last_advanced_tick {
            return Err(ReloadStartError::TickReplay);
        }
        if self.has_open_dive_context()
            || matches!(
                riding.stance,
                RiderStance::MountedAirborne | RiderStance::SaddleDiveAirborne
            )
        {
            return Err(ReloadStartError::Airborne);
        }
        if riding.stance != RiderStance::Mounted || !riding.is_consistent() {
            return Err(ReloadStartError::Dismounted);
        }
        self.advance_to(tick, riding)
            .map_err(|_| ReloadStartError::TickReplay)?;
        self.begin_reload()
    }

    /// Requests a shot using the kernel-derived seed and the unchanged wire
    /// rejection enum.
    pub fn request_fire(
        &mut self,
        tick: SimulationTick,
        direction: QuantizedDirection,
        riding: RidingState,
    ) -> Result<PreparedShot, ShotRejectionReason> {
        let seed = self.next_spread_seed();
        self.request_fire_detailed(tick, direction, riding, seed)
            .map_err(|rejection| rejection.wire_reason)
    }

    /// Requests a shot while validating the wire-provided seed.
    pub fn request_fire_with_seed(
        &mut self,
        tick: SimulationTick,
        direction: QuantizedDirection,
        riding: RidingState,
        supplied_seed: u64,
    ) -> Result<PreparedShot, ShotRejectionReason> {
        self.request_fire_detailed(tick, direction, riding, supplied_seed)
            .map_err(|rejection| rejection.wire_reason)
    }

    /// Detailed fire path retaining an internal M2 cap/context reason. The cap
    /// is checked before clock, ammo, seed, cadence, recoil, or telemetry state
    /// can mutate.
    pub fn request_fire_detailed(
        &mut self,
        tick: SimulationTick,
        direction: QuantizedDirection,
        riding: RidingState,
        supplied_seed: u64,
    ) -> Result<PreparedShot, FireRejection> {
        if tick < self.last_advanced_tick {
            return Err(FireRejection::wire(ShotRejectionReason::TickReplay));
        }
        if !riding.is_consistent()
            || (self.has_open_dive_context() && riding.stance != RiderStance::SaddleDiveAirborne)
        {
            return Err(FireRejection::dive(
                ShotRejectionReason::Dismounted,
                DiveFireRejection::ContextMismatch,
            ));
        }

        let dive_context = if riding.stance == RiderStance::SaddleDiveAirborne {
            let Some(context) = self.dive_context else {
                return Err(FireRejection::dive(
                    ShotRejectionReason::Dismounted,
                    DiveFireRejection::ContextMismatch,
                ));
            };
            if riding.dive_id != Some(context.dive_id) {
                return Err(FireRejection::dive(
                    ShotRejectionReason::Dismounted,
                    DiveFireRejection::ContextMismatch,
                ));
            }
            if context.closed_to_new_shots {
                return Err(FireRejection::dive(
                    ShotRejectionReason::Dismounted,
                    DiveFireRejection::Closed,
                ));
            }
            if self.equipped != context.launch_weapon {
                return Err(FireRejection::dive(
                    ShotRejectionReason::Weapon,
                    DiveFireRejection::WeaponMismatch,
                ));
            }
            if context.accepted_count >= dive_shot_cap(context.launch_weapon) {
                return Err(FireRejection::dive(
                    ShotRejectionReason::Rate,
                    DiveFireRejection::ShotCap,
                ));
            }
            Some(context)
        } else {
            None
        };

        self.advance_to(tick, riding)
            .map_err(|_| FireRejection::wire(ShotRejectionReason::TickReplay))?;
        if !direction.is_normalized() {
            return Err(FireRejection::wire(ShotRejectionReason::InvalidDirection));
        }
        match riding.stance {
            RiderStance::Mounted => {}
            RiderStance::MountedAirborne => {
                return Err(FireRejection::wire(ShotRejectionReason::Airborne));
            }
            RiderStance::SaddleDiveAirborne => {}
            RiderStance::LandingProne
            | RiderStance::LandingRecovery
            | RiderStance::OnFootStanding
            | RiderStance::Unknown(_) => {
                return Err(FireRejection::wire(ShotRejectionReason::Dismounted));
            }
        }
        if riding.stumbling {
            return Err(FireRejection::wire(ShotRejectionReason::Stumble));
        }
        if self.holstered {
            return Err(FireRejection::wire(ShotRejectionReason::Holstered));
        }
        if self.reload.is_some() {
            return Err(FireRejection::wire(ShotRejectionReason::Reloading));
        }

        let stats = *self.equipped.stats();
        if let Some(last_tick) = self.last_accepted_tick {
            let elapsed = tick
                .checked_duration_since(last_tick)
                .ok_or_else(|| FireRejection::wire(ShotRejectionReason::TickReplay))?;
            if elapsed < stats.cadence_ticks(self.tick_rate) {
                return Err(FireRejection::wire(ShotRejectionReason::Rate));
            }
        }
        if self.equipped_ammo().magazine == 0 {
            return Err(FireRejection::wire(ShotRejectionReason::Empty));
        }

        let expected_seed = self.next_spread_seed();
        if supplied_seed != expected_seed {
            return Err(FireRejection::wire(ShotRejectionReason::SpreadSeed));
        }
        let handling = dive_context.map_or(riding, |context| {
            let mut handling = context.launch_handling;
            handling.stance = RiderStance::SaddleDiveAirborne;
            handling.dive_id = Some(context.dive_id);
            handling
        });
        let spread_millidegrees = effective_spread_millidegrees(stats, handling);
        let mut sway = deterministic_sway(tick, self.tick_rate, handling);
        if let Some(context) = dive_context {
            let elapsed = tick
                .checked_duration_since(context.launch_tick)
                .unwrap_or_default();
            sway = scale_sway(
                sway,
                dive_sway_scale_milli(elapsed, context.nominal_airtime_ticks),
            );
        }
        let resolved_direction =
            spread_direction(direction, expected_seed, spread_millidegrees, sway)
                .ok_or_else(|| FireRejection::wire(ShotRejectionReason::InvalidDirection))?;

        let accepted_index = self.shot_index;
        let ammo = self
            .inventory
            .get_mut(&self.equipped)
            .expect("equipped weapon must be owned");
        ammo.magazine = ammo.magazine.saturating_sub(1);
        let ammo_after = *ammo;
        self.last_accepted_tick = Some(tick);
        self.shot_index = self.shot_index.saturating_add(1);
        self.recoil.apply_impulse(stats, expected_seed);
        if let Some(context) = &mut self.dive_context {
            if dive_context.is_some() {
                context.accepted_count = context.accepted_count.saturating_add(1);
            }
        }

        Ok(PreparedShot {
            tick,
            weapon_id: self.equipped,
            spread_seed: expected_seed,
            resolved_direction,
            spread_millidegrees,
            sway,
            ammo: ammo_after,
            recoil: self.recoil,
            accepted_shot_index: accepted_index,
            dive_id: dive_context.map(|context| context.dive_id),
        })
    }

    /// Applies map-pickup, duplicate-ammo, swap, and timed-drop rules.
    pub fn pickup(
        &mut self,
        pickup: WeaponPickup,
        distance_mm: u32,
        tick: SimulationTick,
    ) -> Result<PickupOutcome, PickupError> {
        if self.has_open_dive_context() || self.riding.stance == RiderStance::SaddleDiveAirborne {
            return Err(PickupError::Airborne);
        }
        if distance_mm > PICKUP_RANGE_MM {
            return Err(PickupError::OutOfRange);
        }
        if pickup.is_expired(tick) {
            return Err(PickupError::Expired);
        }

        let stats = *pickup.weapon_id.stats();
        if let Some(existing) = self.inventory.get_mut(&pickup.weapon_id) {
            let before = existing.reserve;
            existing.reserve = stats.reserve_capacity;
            return Ok(PickupOutcome::AmmoOnly {
                weapon_id: pickup.weapon_id,
                added_reserve: existing.reserve - before,
                reserve: existing.reserve,
            });
        }

        let old_weapon = self.equipped;
        let old_ammo = self.inventory.remove(&old_weapon).unwrap_or_default();
        let lifetime_ticks = u64::from(self.tick_rate)
            .saturating_mul(u64::from(DROPPED_WEAPON_LIFETIME_MS))
            .div_ceil(1_000);
        let dropped = WeaponPickup {
            weapon_id: old_weapon,
            ammo: old_ammo,
            spawned_at: tick,
            expires_at: Some(tick.saturating_add(lifetime_ticks)),
        };
        self.inventory
            .insert(pickup.weapon_id, pickup.ammo.clamped(stats));
        self.equipped = pickup.weapon_id;
        self.holstered = false;
        self.reload = None;
        Ok(PickupOutcome::Swapped {
            weapon_id: pickup.weapon_id,
            dropped,
        })
    }
}

/// Effective cone spread from speed, gait, turning, airborne state, and ADS.
#[must_use]
pub fn effective_spread_millidegrees(stats: WeaponStats, riding: RidingState) -> u32 {
    let ratio = u64::from(riding.speed_ratio_milli());
    let target = if riding.gait == CombatGait::Gallop {
        stats.gallop_spread_millidegrees
    } else {
        stats.moving_spread_millidegrees
    };
    let base = u64::from(stats.base_spread_millidegrees);
    let interpolated = base + (u64::from(target).saturating_sub(base) * ratio + 500) / 1_000;

    let turn_fraction = u64::from(
        riding
            .yaw_rate_millidegrees_per_second
            .unsigned_abs()
            .min(MAX_TURN_RATE_MILLIDEGREES_PER_SECOND),
    );
    let turn_penalty = u64::from(target) * turn_fraction
        / u64::from(MAX_TURN_RATE_MILLIDEGREES_PER_SECOND)
        / MAX_TURN_SPREAD_PENALTY_DIVISOR;
    let mut spread = interpolated.saturating_add(turn_penalty);
    if riding.stance == RiderStance::MountedAirborne {
        spread = spread.saturating_mul(AIRBORNE_SPREAD_PENALTY_NUMERATOR)
            / AIRBORNE_SPREAD_PENALTY_DENOMINATOR;
    }
    if riding.ads {
        spread = (spread.saturating_mul(ADS_SPREAD_NUMERATOR) + ADS_SPREAD_DENOMINATOR / 2)
            / ADS_SPREAD_DENOMINATOR;
    }
    u32::try_from(spread).unwrap_or(u32::MAX)
}

/// Sway sampled only from fixed tick plus rewound riding state.
#[must_use]
pub fn deterministic_sway(tick: SimulationTick, tick_rate: u32, riding: RidingState) -> SwaySample {
    if tick_rate == 0 || riding.gait == CombatGait::Idle {
        return SwaySample::default();
    }
    let (peak_to_peak_millidegrees, frequency_millihertz) = match riding.gait {
        CombatGait::Idle => (0, 0),
        CombatGait::Walk => (300, 1_200),
        CombatGait::Trot => (600, 1_800),
        CombatGait::Canter => (900, 2_400),
        CombatGait::Gallop => (1_300, 3_000),
    };
    let amplitude = f64::from(peak_to_peak_millidegrees) / 2.0;
    let cycles =
        tick.as_u64() as f64 * f64::from(frequency_millihertz) / (f64::from(tick_rate) * 1_000.0);
    let phase = TAU * cycles;
    let turn_coupling = (riding.yaw_rate_millidegrees_per_second / 200).clamp(-450, 450);
    let sample = SwaySample {
        pitch_millidegrees: (phase.sin() * amplitude).round() as i32,
        yaw_millidegrees: (phase.cos() * amplitude).round() as i32 + turn_coupling,
    };
    if riding.majestic_charge {
        scale_sway(sample, CHARGE_SWAY_MULTIPLIER_MILLI)
    } else {
        sample
    }
}

fn scale_sway(sample: SwaySample, scale_milli: u16) -> SwaySample {
    fn scale_axis(value: i32, scale_milli: u16) -> i32 {
        let product = i64::from(value) * i64::from(scale_milli);
        let rounded = if product >= 0 {
            (product + 500) / 1_000
        } else {
            -((-product + 500) / 1_000)
        };
        i32::try_from(rounded).unwrap_or(if rounded < 0 { i32::MIN } else { i32::MAX })
    }
    SwaySample {
        pitch_millidegrees: scale_axis(sample.pitch_millidegrees, scale_milli),
        yaw_millidegrees: scale_axis(sample.yaw_millidegrees, scale_milli),
    }
}

/// Stable per-shot seed derived without platform hashing or process randomness.
#[must_use]
pub fn shot_spread_seed(lobby_seed: u64, shooter_peer_id: PlayerId, shot_index: u64) -> u64 {
    let bytes = shooter_peer_id.as_bytes();
    let first = u64::from_be_bytes(bytes[0..8].try_into().expect("fixed UUID half"));
    let second = u64::from_be_bytes(bytes[8..16].try_into().expect("fixed UUID half"));
    let state = splitmix64(lobby_seed ^ 0x7370_7572_6669_7265);
    let state = splitmix64(state ^ first);
    let state = splitmix64(state ^ second);
    splitmix64(state ^ shot_index.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn unit_sample(seed: u64) -> f64 {
    let mantissa = splitmix64(seed) >> 11;
    mantissa as f64 / ((1_u64 << 53) as f64)
}

fn deterministic_signed(seed: u64, maximum: u32) -> i32 {
    if maximum == 0 {
        return 0;
    }
    let width = u64::from(maximum).saturating_mul(2).saturating_add(1);
    let sample = splitmix64(seed) % width;
    i32::try_from(sample).unwrap_or(i32::MAX) - i32::try_from(maximum).unwrap_or(i32::MAX)
}

#[derive(Clone, Copy, Debug, Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Vec3 {
    const X: Self = Self {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    };
    const Y: Self = Self {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    };

    fn from_array(value: [f64; 3]) -> Self {
        Self {
            x: value[0],
            y: value[1],
            z: value[2],
        }
    }

    fn from_origin(value: QuantizedOrigin) -> Self {
        Self {
            x: f64::from(value.x),
            y: f64::from(value.y),
            z: f64::from(value.z),
        }
    }

    fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn cross(self, other: Self) -> Self {
        Self {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    fn normalized(self) -> Option<Self> {
        let length = self.length();
        if !length.is_finite() || length <= f64::EPSILON {
            None
        } else {
            Some(self.scale(1.0 / length))
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
            z: self.z + other.z,
        }
    }

    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }

    fn scale(self, scalar: f64) -> Self {
        Self {
            x: self.x * scalar,
            y: self.y * scalar,
            z: self.z * scalar,
        }
    }
}

fn aiming_basis(forward: Vec3) -> Option<(Vec3, Vec3)> {
    let helper = if forward.y.abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let right = forward.cross(helper).normalized()?;
    let up = right.cross(forward).normalized()?;
    Some((right, up))
}

fn spread_direction(
    base: QuantizedDirection,
    seed: u64,
    spread_millidegrees: u32,
    sway: SwaySample,
) -> Option<QuantizedDirection> {
    let forward = Vec3::from_array(base.to_components()).normalized()?;
    let (right, up) = aiming_basis(forward)?;
    let yaw_tangent = (f64::from(sway.yaw_millidegrees) / 1_000.0 * PI / 180.0).tan();
    let pitch_tangent = (f64::from(sway.pitch_millidegrees) / 1_000.0 * PI / 180.0).tan();
    let swayed = forward
        .add(right.scale(yaw_tangent))
        .add(up.scale(pitch_tangent))
        .normalized()?;
    let (swayed_right, swayed_up) = aiming_basis(swayed)?;

    let radius_fraction = unit_sample(seed ^ 0xa076_1d64_78bd_642f).sqrt();
    let azimuth = TAU * unit_sample(seed ^ 0xe703_7ed1_a0b4_28db);
    let cone_tangent = (f64::from(spread_millidegrees) / 1_000.0 * PI / 180.0).tan();
    let radius = radius_fraction * cone_tangent;
    let resolved = swayed
        .add(swayed_right.scale(radius * azimuth.cos()))
        .add(swayed_up.scale(radius * azimuth.sin()))
        .normalized()?;
    QuantizedDirection::from_components(resolved.x, resolved.y, resolved.z).ok()
}

/// Immutable target identity/team/health row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetDefinition {
    /// Stable tie-break ID.
    pub entity_id: EntityId,
    /// Owning peer, excluded from self hits.
    pub owner_peer_id: Option<PlayerId>,
    /// Team mask, excluded when equal to the shooter team.
    pub team_id: TeamId,
    /// Spawn health.
    pub max_health: u16,
}

/// Rewindable target geometry at one simulation tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetPoseSnapshot {
    /// Snapshot tick.
    pub tick: SimulationTick,
    /// Target identity.
    pub entity_id: EntityId,
    /// Rewound logical stance retained for D7 geometry selection.
    pub stance: RiderStance,
    /// Center of the vertical body capsule.
    pub body_center: QuantizedOrigin,
    /// Half-length of the capsule's inner vertical segment.
    pub body_half_height_mm: u16,
    /// Body capsule radius.
    pub body_radius_mm: u16,
    /// Center of the head sphere.
    pub head_center: QuantizedOrigin,
    /// Head sphere radius.
    pub head_radius_mm: u16,
    /// Whether geometry is hittable at this tick.
    pub active: bool,
}

/// Rewindable horizontally oriented horse geometry at one simulation tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HorseTargetPoseSnapshot {
    /// Snapshot tick.
    pub tick: SimulationTick,
    /// Stable horse target identity.
    pub entity_id: EntityId,
    /// Center of the horizontal body capsule.
    pub body_center: QuantizedOrigin,
    /// Normalized planar nose-forward direction.
    pub body_forward: QuantizedDirection,
    /// Half-length of the body capsule's inner segment.
    pub body_half_length_mm: u16,
    /// Body capsule radius.
    pub body_radius_mm: u16,
    /// Center of the head sphere.
    pub head_center: QuantizedOrigin,
    /// Head sphere radius.
    pub head_radius_mm: u16,
    /// Whether geometry is hittable at this tick.
    pub active: bool,
}

/// Invalid registry operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TargetRegistryError {
    /// Tick rate was zero.
    #[error("invalid_tick_rate")]
    InvalidTickRate,
    /// Entity was already registered.
    #[error("duplicate_target")]
    DuplicateTarget,
    /// Entity has no definition.
    #[error("unknown_target")]
    UnknownTarget,
    /// Capsule/sphere dimensions or health are zero.
    #[error("invalid_target")]
    InvalidTarget,
    /// Target history must be recorded monotonically.
    #[error("tick_replay")]
    TickReplay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TargetRecord {
    definition: TargetDefinition,
    health: u16,
    history: VecDeque<TargetGeometrySnapshot>,
    geometry_kind: Option<TargetGeometryKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetGeometryKind {
    Rider,
    Horse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetGeometrySnapshot {
    Rider(TargetPoseSnapshot),
    Horse(HorseTargetPoseSnapshot),
}

impl TargetGeometrySnapshot {
    const fn tick(self) -> SimulationTick {
        match self {
            Self::Rider(pose) => pose.tick,
            Self::Horse(pose) => pose.tick,
        }
    }

    const fn kind(self) -> TargetGeometryKind {
        match self {
            Self::Rider(_) => TargetGeometryKind::Rider,
            Self::Horse(_) => TargetGeometryKind::Horse,
        }
    }
}

/// Server-owned deterministic target registry with a 250 ms pose ring buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetRegistry {
    history_ticks: u64,
    targets: BTreeMap<EntityId, TargetRecord>,
}

impl TargetRegistry {
    /// Creates an empty registry for a fixed simulation frequency.
    pub fn new(tick_rate: u32) -> Result<Self, TargetRegistryError> {
        if tick_rate == 0 {
            return Err(TargetRegistryError::InvalidTickRate);
        }
        let history_ticks = u64::from(tick_rate)
            .saturating_mul(u64::from(ROLLBACK_WINDOW_MS))
            .div_ceil(1_000);
        Ok(Self {
            history_ticks,
            targets: BTreeMap::new(),
        })
    }

    /// Registers a stable target definition at full spawn health.
    pub fn register(&mut self, definition: TargetDefinition) -> Result<(), TargetRegistryError> {
        self.restore(definition, definition.max_health)
    }

    /// Number of immutable target identities in the authority registry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// Whether no target identities are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Immutable identity row for migration graph validation.
    #[must_use]
    pub fn definition(&self, entity_id: EntityId) -> Option<TargetDefinition> {
        self.targets.get(&entity_id).map(|record| record.definition)
    }

    /// Restores a stable target definition and authority-owned health.
    ///
    /// This is intended for a hash-checked migration checkpoint. Pose history is
    /// deliberately empty until the successor records its first authoritative
    /// simulation tick.
    pub fn restore(
        &mut self,
        definition: TargetDefinition,
        health: u16,
    ) -> Result<(), TargetRegistryError> {
        if definition.max_health == 0 || health > definition.max_health {
            return Err(TargetRegistryError::InvalidTarget);
        }
        if self.targets.contains_key(&definition.entity_id) {
            return Err(TargetRegistryError::DuplicateTarget);
        }
        self.targets.insert(
            definition.entity_id,
            TargetRecord {
                definition,
                health,
                history: VecDeque::new(),
                geometry_kind: None,
            },
        );
        Ok(())
    }

    /// Adds one monotonic pose and prunes geometry older than the rewind window.
    pub fn record_pose(&mut self, pose: TargetPoseSnapshot) -> Result<(), TargetRegistryError> {
        if pose.body_radius_mm == 0 || pose.head_radius_mm == 0 {
            return Err(TargetRegistryError::InvalidTarget);
        }
        let record = self
            .targets
            .get_mut(&pose.entity_id)
            .ok_or(TargetRegistryError::UnknownTarget)?;
        Self::push_geometry(
            record,
            TargetGeometrySnapshot::Rider(pose),
            self.history_ticks,
        )?;
        Ok(())
    }

    /// Adds one monotonic, horizontally oriented horse pose.
    pub fn record_horse_pose(
        &mut self,
        pose: HorseTargetPoseSnapshot,
    ) -> Result<(), TargetRegistryError> {
        if pose.body_half_length_mm == 0
            || pose.body_radius_mm == 0
            || pose.head_radius_mm == 0
            || !pose.body_forward.is_normalized()
            || pose.body_forward.y.abs() > DIRECTION_UNITS / 100
        {
            return Err(TargetRegistryError::InvalidTarget);
        }
        let record = self
            .targets
            .get_mut(&pose.entity_id)
            .ok_or(TargetRegistryError::UnknownTarget)?;
        Self::push_geometry(
            record,
            TargetGeometrySnapshot::Horse(pose),
            self.history_ticks,
        )?;
        Ok(())
    }

    fn push_geometry(
        record: &mut TargetRecord,
        geometry: TargetGeometrySnapshot,
        history_ticks: u64,
    ) -> Result<(), TargetRegistryError> {
        let tick = geometry.tick();
        let geometry_kind = geometry.kind();
        if record
            .geometry_kind
            .is_some_and(|existing| existing != geometry_kind)
        {
            return Err(TargetRegistryError::InvalidTarget);
        }
        if record
            .history
            .back()
            .is_some_and(|previous| tick <= previous.tick())
        {
            return Err(TargetRegistryError::TickReplay);
        }
        record.history.push_back(geometry);
        record.geometry_kind = Some(geometry_kind);
        let oldest = tick.as_u64().saturating_sub(history_ticks);
        while record
            .history
            .front()
            .is_some_and(|snapshot| snapshot.tick().as_u64() < oldest)
        {
            record.history.pop_front();
        }
        Ok(())
    }

    /// Current health for a target.
    #[must_use]
    pub fn health(&self, entity_id: EntityId) -> Option<u16> {
        self.targets.get(&entity_id).map(|record| record.health)
    }

    /// Synchronizes externally composed authority health, such as M3 horse
    /// regeneration/remount, without changing immutable target identity.
    pub fn synchronize_health(
        &mut self,
        entity_id: EntityId,
        health: u16,
    ) -> Result<(), TargetRegistryError> {
        let record = self
            .targets
            .get_mut(&entity_id)
            .ok_or(TargetRegistryError::UnknownTarget)?;
        if health > record.definition.max_health {
            return Err(TargetRegistryError::InvalidTarget);
        }
        record.health = health;
        Ok(())
    }

    /// Rewound pose selected for an exact target/tick, if history exists.
    #[must_use]
    pub fn target_pose_at(
        &self,
        entity_id: EntityId,
        tick: SimulationTick,
    ) -> Option<TargetPoseSnapshot> {
        self.targets
            .get(&entity_id)
            .and_then(|record| match Self::geometry_at(record, tick)? {
                TargetGeometrySnapshot::Rider(pose) => Some(pose),
                TargetGeometrySnapshot::Horse(_) => None,
            })
    }

    /// Rewound horse pose selected for an exact target/tick, if history exists.
    #[must_use]
    pub fn horse_pose_at(
        &self,
        entity_id: EntityId,
        tick: SimulationTick,
    ) -> Option<HorseTargetPoseSnapshot> {
        self.targets
            .get(&entity_id)
            .and_then(|record| match Self::geometry_at(record, tick)? {
                TargetGeometrySnapshot::Horse(pose) => Some(pose),
                TargetGeometrySnapshot::Rider(_) => None,
            })
    }

    /// Restores target health to its immutable spawn value.
    pub fn respawn(&mut self, entity_id: EntityId) -> Result<(), TargetRegistryError> {
        let record = self
            .targets
            .get_mut(&entity_id)
            .ok_or(TargetRegistryError::UnknownTarget)?;
        record.health = record.definition.max_health;
        Ok(())
    }

    fn geometry_at(record: &TargetRecord, tick: SimulationTick) -> Option<TargetGeometrySnapshot> {
        record
            .history
            .iter()
            .rev()
            .find(|snapshot| snapshot.tick() <= tick)
            .copied()
    }

    fn nearest_hit(
        &self,
        tick: SimulationTick,
        origin: QuantizedOrigin,
        direction: QuantizedDirection,
        maximum_distance_mm: u32,
        shooter_peer_id: PlayerId,
        shooter_team: TeamId,
    ) -> Option<ResolvedTargetHit> {
        let ray_origin = Vec3::from_origin(origin);
        let ray_direction = Vec3::from_array(direction.to_components()).normalized()?;
        let mut nearest: Option<ResolvedTargetHit> = None;

        for record in self.targets.values() {
            if record.health == 0
                || record.definition.owner_peer_id == Some(shooter_peer_id)
                || record.definition.team_id == shooter_team
            {
                continue;
            }
            let Some(geometry) = Self::geometry_at(record, tick) else {
                continue;
            };
            let (active, body_distance, head_distance) = match geometry {
                TargetGeometrySnapshot::Rider(pose) => (
                    pose.active,
                    ray_vertical_capsule_intersection(
                        ray_origin,
                        ray_direction,
                        Vec3::from_origin(pose.body_center),
                        f64::from(pose.body_half_height_mm),
                        f64::from(pose.body_radius_mm),
                    ),
                    ray_sphere_intersection(
                        ray_origin,
                        ray_direction,
                        Vec3::from_origin(pose.head_center),
                        f64::from(pose.head_radius_mm),
                    ),
                ),
                TargetGeometrySnapshot::Horse(pose) => {
                    let center = Vec3::from_origin(pose.body_center);
                    let forward = Vec3::from_array(pose.body_forward.to_components())
                        .normalized()
                        .expect("recorded horse forward is normalized");
                    let half_length = f64::from(pose.body_half_length_mm);
                    (
                        pose.active,
                        ray_capsule_intersection(
                            ray_origin,
                            ray_direction,
                            center.sub(forward.scale(half_length)),
                            center.add(forward.scale(half_length)),
                            f64::from(pose.body_radius_mm),
                        ),
                        ray_sphere_intersection(
                            ray_origin,
                            ray_direction,
                            Vec3::from_origin(pose.head_center),
                            f64::from(pose.head_radius_mm),
                        ),
                    )
                }
            };
            if !active {
                continue;
            }
            for (zone, distance) in [
                (HitZone::Head, head_distance),
                (HitZone::Body, body_distance),
            ] {
                let Some(distance) = distance else {
                    continue;
                };
                if !distance.is_finite()
                    || distance < 0.0
                    || distance > f64::from(maximum_distance_mm)
                {
                    continue;
                }
                let distance_mm = distance.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                let candidate = ResolvedTargetHit {
                    target_id: record.definition.entity_id,
                    hit_zone: zone,
                    distance_mm,
                };
                if nearest.is_none_or(|current| candidate.sort_key() < current.sort_key()) {
                    nearest = Some(candidate);
                }
            }
        }
        nearest
    }

    fn apply_damage(&mut self, target_id: EntityId, damage: u16) -> DamageApplication {
        let Some(record) = self.targets.get_mut(&target_id) else {
            return DamageApplication {
                remaining_health: 0,
                eliminated: false,
            };
        };
        let was_alive = record.health > 0;
        record.health = record.health.saturating_sub(damage);
        DamageApplication {
            remaining_health: record.health,
            eliminated: was_alive && record.health == 0,
        }
    }
}

/// Nearest server-computed geometry hit before damage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedTargetHit {
    /// Stable target ID.
    pub target_id: EntityId,
    /// Geometry-derived zone.
    pub hit_zone: HitZone,
    /// Rounded ray distance in millimetres.
    pub distance_mm: u32,
}

impl ResolvedTargetHit {
    fn sort_key(self) -> (u32, EntityId, u8) {
        let zone_order = match self.hit_zone {
            HitZone::Head => 0,
            HitZone::Body => 1,
        };
        (self.distance_mm, self.target_id, zone_order)
    }
}

/// Result of applying authority damage to current target health.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageApplication {
    /// Health after damage.
    pub remaining_health: u16,
    /// This application crossed from alive to zero.
    pub eliminated: bool,
}

fn ray_sphere_intersection(
    origin: Vec3,
    direction: Vec3,
    center: Vec3,
    radius: f64,
) -> Option<f64> {
    let offset = origin.sub(center);
    let c = offset.dot(offset) - radius * radius;
    if c <= 0.0 {
        return Some(0.0);
    }
    let b = offset.dot(direction);
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    let near = -b - root;
    if near >= 0.0 {
        Some(near)
    } else {
        let far = -b + root;
        (far >= 0.0).then_some(far)
    }
}

fn ray_vertical_capsule_intersection(
    origin: Vec3,
    direction: Vec3,
    center: Vec3,
    half_height: f64,
    radius: f64,
) -> Option<f64> {
    let minimum_y = center.y - half_height;
    let maximum_y = center.y + half_height;
    let closest_y = origin.y.clamp(minimum_y, maximum_y);
    let inside_offset = origin.sub(Vec3 {
        x: center.x,
        y: closest_y,
        z: center.z,
    });
    if inside_offset.dot(inside_offset) <= radius * radius {
        return Some(0.0);
    }

    let mut nearest = None;
    let horizontal_x = origin.x - center.x;
    let horizontal_z = origin.z - center.z;
    let a = direction.x * direction.x + direction.z * direction.z;
    if a > f64::EPSILON {
        let b = 2.0 * (horizontal_x * direction.x + horizontal_z * direction.z);
        let c = horizontal_x * horizontal_x + horizontal_z * horizontal_z - radius * radius;
        let discriminant = b * b - 4.0 * a * c;
        if discriminant >= 0.0 {
            let root = discriminant.sqrt();
            for distance in [(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)] {
                if distance >= 0.0 {
                    let y = origin.y + direction.y * distance;
                    if (minimum_y..=maximum_y).contains(&y) {
                        nearest = choose_nearer(nearest, Some(distance));
                    }
                }
            }
        }
    }

    for cap_y in [minimum_y, maximum_y] {
        nearest = choose_nearer(
            nearest,
            ray_sphere_intersection(
                origin,
                direction,
                Vec3 {
                    x: center.x,
                    y: cap_y,
                    z: center.z,
                },
                radius,
            ),
        );
    }
    nearest
}

fn ray_capsule_intersection(
    origin: Vec3,
    direction: Vec3,
    start: Vec3,
    end: Vec3,
    radius: f64,
) -> Option<f64> {
    let axis = end.sub(start);
    let axis_length_squared = axis.dot(axis);
    if axis_length_squared <= f64::EPSILON {
        return ray_sphere_intersection(origin, direction, start, radius);
    }
    let origin_from_start = origin.sub(start);
    let closest_fraction = (origin_from_start.dot(axis) / axis_length_squared).clamp(0.0, 1.0);
    let closest = start.add(axis.scale(closest_fraction));
    let inside_offset = origin.sub(closest);
    if inside_offset.dot(inside_offset) <= radius * radius {
        return Some(0.0);
    }

    // Analytic ray/infinite-cylinder intersection, restricted to the inner
    // segment. End-cap sphere tests below complete the capsule.
    let axis_dot_ray = axis.dot(direction);
    let axis_dot_origin = axis.dot(origin_from_start);
    let ray_dot_origin = direction.dot(origin_from_start);
    let origin_length_squared = origin_from_start.dot(origin_from_start);
    let quadratic_a = axis_length_squared - axis_dot_ray * axis_dot_ray;
    let quadratic_b = axis_length_squared * ray_dot_origin - axis_dot_origin * axis_dot_ray;
    let quadratic_c = axis_length_squared * origin_length_squared
        - axis_dot_origin * axis_dot_origin
        - radius * radius * axis_length_squared;
    let discriminant = quadratic_b * quadratic_b - quadratic_a * quadratic_c;
    let mut nearest = None;
    if quadratic_a.abs() > f64::EPSILON && discriminant >= 0.0 {
        let root = discriminant.sqrt();
        for distance in [
            (-quadratic_b - root) / quadratic_a,
            (-quadratic_b + root) / quadratic_a,
        ] {
            if distance >= 0.0 {
                let axial = axis_dot_origin + distance * axis_dot_ray;
                if (0.0..=axis_length_squared).contains(&axial) {
                    nearest = choose_nearer(nearest, Some(distance));
                }
            }
        }
    }
    nearest = choose_nearer(
        nearest,
        ray_sphere_intersection(origin, direction, start, radius),
    );
    choose_nearer(
        nearest,
        ray_sphere_intersection(origin, direction, end, radius),
    )
}

fn choose_nearer(first: Option<f64>, second: Option<f64>) -> Option<f64> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// Rewound authority rider/muzzle state for one command tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiderSnapshot {
    /// Tick represented by this snapshot.
    pub tick: SimulationTick,
    /// Shooter represented by this snapshot.
    pub shooter_peer_id: PlayerId,
    /// Authority-simulated muzzle origin.
    pub muzzle_origin: QuantizedOrigin,
    /// Shooter team used for friendly exclusion.
    pub team_id: TeamId,
    /// Handling inputs at this tick.
    pub riding: RidingState,
}

/// Result plus secret-free telemetry emitted for every command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityShot {
    /// Wire result.
    pub result: ShotResult,
    /// Persistable deterministic telemetry.
    pub telemetry: ShotTelemetry,
    /// M2-only precision when the wire reason remains `rate`/`dismounted`.
    pub dive_fire_rejection: Option<DiveFireRejection>,
    /// Authority-owned acceptance retained for instrumentation.
    pub accepted_shot: Option<AcceptedShotMetadata>,
    /// Newly derived deterministic style notifications.
    pub gameplay_events: Vec<GameplayEventRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthorityShooter {
    kernel: CombatKernel,
    last_command_tick: Option<SimulationTick>,
}

/// Deterministic elected-authority combat validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CombatAuthority {
    tick_rate: u32,
    lobby_seed: u64,
    authority_epoch: u64,
    shooters: BTreeMap<PlayerId, AuthorityShooter>,
    shot_ledger: ShotAttributionLedger,
}

impl CombatAuthority {
    /// Creates an authority with no registered shooters.
    pub fn new(tick_rate: u32, lobby_seed: u64) -> Result<Self, InvalidTickRate> {
        if tick_rate == 0 {
            return Err(InvalidTickRate);
        }
        Ok(Self {
            tick_rate,
            lobby_seed,
            authority_epoch: 0,
            shooters: BTreeMap::new(),
            shot_ledger: ShotAttributionLedger::default(),
        })
    }

    /// Sets the current authority epoch monotonically. Offline play leaves it zero.
    pub fn set_authority_epoch(&mut self, authority_epoch: u64) -> bool {
        if authority_epoch < self.authority_epoch {
            return false;
        }
        self.authority_epoch = authority_epoch;
        true
    }

    /// Current authority epoch used in event IDs.
    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    /// Number of authority-owned shooter kernels.
    #[must_use]
    pub fn shooter_count(&self) -> usize {
        self.shooters.len()
    }

    /// Match-lifetime accepted-shot ledger.
    #[must_use]
    pub const fn shot_attribution_ledger(&self) -> &ShotAttributionLedger {
        &self.shot_ledger
    }

    /// Attributes a delayed/replayed authority result through the retained
    /// match-lifetime ledger. Target damage remains owned by the authority's
    /// normal resolution path; this method derives telemetry/events only.
    pub fn observe_authority_result(
        &mut self,
        result: &ShotResult,
    ) -> crate::ShotResultAttribution {
        self.shot_ledger
            .observe_result(self.authority_epoch, result)
    }

    /// Begins the authority-owned combat context for one accepted movement dive.
    pub fn begin_saddle_dive(
        &mut self,
        shooter_peer_id: PlayerId,
        dive_id: DiveId,
        launch_tick: SimulationTick,
        launch_weapon: WeaponId,
        prelaunch_horizontal_velocity_mmps: [i32; 2],
        nominal_airtime_ticks: u64,
    ) -> Result<(), DiveContextError> {
        self.shooter_kernel_mut(shooter_peer_id)
            .ok_or(DiveContextError::ContextMismatch)?
            .begin_saddle_dive(
                dive_id,
                launch_tick,
                launch_weapon,
                prelaunch_horizontal_velocity_mmps,
                nominal_airtime_ticks,
            )
    }

    /// Closes one authority-owned dive at first valid landing.
    pub fn finish_saddle_dive(
        &mut self,
        shooter_peer_id: PlayerId,
        dive_id: DiveId,
        landing_tick: SimulationTick,
    ) -> Result<(), DiveContextError> {
        self.shooter_kernel_mut(shooter_peer_id)
            .ok_or(DiveContextError::ContextMismatch)?
            .finish_saddle_dive(dive_id, landing_tick)
    }

    /// Registers one shooter with one full selected rifle. Re-registering an
    /// existing shooter is mutation-free so replay/cadence/ammo/dive state can
    /// never be reset by a reconnect or duplicate registration message.
    pub fn register_shooter(&mut self, shooter_peer_id: PlayerId, weapon_id: WeaponId) -> bool {
        match self.shooters.entry(shooter_peer_id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(entry) => {
                let kernel = CombatKernel::with_weapon(
                    self.tick_rate,
                    self.lobby_seed,
                    shooter_peer_id,
                    weapon_id,
                )
                .expect("authority tick rate is valid");
                entry.insert(AuthorityShooter {
                    kernel,
                    last_command_tick: None,
                });
                true
            }
        }
    }

    /// Restore one bounded shooter checkpoint during an authenticated epoch handoff.
    pub fn restore_shooter(
        &mut self,
        shooter_peer_id: PlayerId,
        weapon_id: WeaponId,
        ammo: WeaponAmmo,
        last_command_tick: Option<SimulationTick>,
        last_accepted_tick: Option<SimulationTick>,
        shot_index: u64,
    ) -> bool {
        let Ok(mut kernel) =
            CombatKernel::with_weapon(self.tick_rate, self.lobby_seed, shooter_peer_id, weapon_id)
        else {
            return false;
        };
        if !kernel.set_ammo(weapon_id, ammo)
            || last_accepted_tick.is_some() != (shot_index > 0)
            || (last_accepted_tick.is_some() && last_command_tick.is_none())
            || last_command_tick.is_some_and(|command| {
                last_accepted_tick.is_some_and(|accepted| accepted > command)
            })
        {
            return false;
        }
        kernel.last_accepted_tick = last_accepted_tick;
        kernel.last_advanced_tick = last_command_tick.or(last_accepted_tick).unwrap_or_default();
        kernel.shot_index = shot_index;
        self.shooters.insert(
            shooter_peer_id,
            AuthorityShooter {
                kernel,
                last_command_tick,
            },
        );
        true
    }

    /// Last admitted command tick for checkpoint export.
    #[must_use]
    pub fn last_command_tick(&self, shooter_peer_id: PlayerId) -> Option<SimulationTick> {
        self.shooters
            .get(&shooter_peer_id)
            .and_then(|state| state.last_command_tick)
    }

    /// Immutable shooter kernel for snapshots/tests.
    #[must_use]
    pub fn shooter_kernel(&self, shooter_peer_id: PlayerId) -> Option<&CombatKernel> {
        self.shooters
            .get(&shooter_peer_id)
            .map(|state| &state.kernel)
    }

    /// Mutable shooter kernel for equipment, reload, pickup, and respawn setup.
    pub fn shooter_kernel_mut(&mut self, shooter_peer_id: PlayerId) -> Option<&mut CombatKernel> {
        self.shooters
            .get_mut(&shooter_peer_id)
            .map(|state| &mut state.kernel)
    }

    /// Authority-derived seed expected on the next command.
    #[must_use]
    pub fn expected_spread_seed(&self, shooter_peer_id: PlayerId) -> Option<u64> {
        self.shooter_kernel(shooter_peer_id)
            .map(CombatKernel::next_spread_seed)
    }

    /// Validates a command and computes nearest hit, damage, and telemetry.
    pub fn validate_shot(
        &mut self,
        command: &ShotCommand,
        authority_tick: SimulationTick,
        rider: RiderSnapshot,
        targets: &mut TargetRegistry,
    ) -> AuthorityShot {
        let Some(shooter) = self.shooters.get_mut(&command.shooter_peer_id) else {
            return rejected_authority_shot(
                command,
                command.weapon_id,
                WeaponAmmo::default(),
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::UnknownShooter,
            );
        };
        let equipped = shooter.kernel.equipped_weapon();
        let ammo_before = shooter.kernel.equipped_ammo();

        if rider.shooter_peer_id != command.shooter_peer_id
            || rider.tick != command.tick
            || !rider.riding.is_consistent()
        {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::RiderSnapshot,
            );
        }
        if command.tick > authority_tick {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::TickFuture,
            );
        }
        let rollback_ticks = u64::from(self.tick_rate)
            .saturating_mul(u64::from(ROLLBACK_WINDOW_MS))
            .div_ceil(1_000);
        let age = authority_tick
            .checked_duration_since(command.tick)
            .unwrap_or(u64::MAX);
        if age > rollback_ticks {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::TickStale,
            );
        }
        if shooter
            .last_command_tick
            .is_some_and(|last_tick| command.tick <= last_tick)
        {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::TickReplay,
            );
        }
        shooter.last_command_tick = Some(command.tick);

        if command.weapon_id != equipped {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::Weapon,
            );
        }
        if !command.direction.is_normalized() {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::InvalidDirection,
            );
        }
        let leash_squared = u128::from(ORIGIN_LEASH_MM).pow(2);
        if command.origin.squared_distance_mm(rider.muzzle_origin) > leash_squared {
            return rejected_authority_shot(
                command,
                equipped,
                ammo_before,
                rider.riding,
                self.tick_rate,
                ShotRejectionReason::OriginLeash,
            );
        }

        let prepared = match shooter.kernel.request_fire_detailed(
            command.tick,
            command.direction,
            rider.riding,
            command.spread_seed,
        ) {
            Ok(prepared) => prepared,
            Err(rejection) => {
                return rejected_authority_shot_detailed(
                    command,
                    equipped,
                    shooter.kernel.equipped_ammo(),
                    rider.riding,
                    self.tick_rate,
                    rejection.wire_reason,
                    rejection.dive_reason,
                );
            }
        };

        let stats = *prepared.weapon_id.stats();
        let resolved_hit = targets.nearest_hit(
            command.tick,
            command.origin,
            prepared.resolved_direction,
            stats.hitscan_clamp_mm,
            command.shooter_peer_id,
            rider.team_id,
        );
        let (outcome, target_id, hit_zone, damage, distance_mm, eliminated) =
            if let Some(hit) = resolved_hit {
                let damage = stats.damage_at(hit.distance_mm, hit.hit_zone);
                let application = targets.apply_damage(hit.target_id, damage);
                (
                    ShotOutcome::Hit,
                    Some(hit.target_id),
                    Some(hit.hit_zone),
                    damage,
                    Some(hit.distance_mm),
                    application.eliminated,
                )
            } else {
                (ShotOutcome::Miss, None, None, 0, None, false)
            };

        let result = ShotResult {
            tick: command.tick,
            shooter_peer_id: command.shooter_peer_id,
            weapon_id: prepared.weapon_id,
            outcome,
            rejection_reason: None,
            resolved_direction: Some(prepared.resolved_direction),
            target_id,
            hit_zone,
            damage,
            distance_mm,
            eliminated,
        };
        let telemetry = ShotTelemetry {
            tick: command.tick,
            shooter: command.shooter_peer_id,
            weapon_id: prepared.weapon_id,
            ammo_mag: prepared.ammo.magazine,
            ammo_reserve: prepared.ammo.reserve,
            spread_millidegrees: prepared.spread_millidegrees,
            sway_millidegrees: prepared.sway.magnitude_millidegrees(),
            gait: rider.riding.gait,
            stance: rider.riding.stance,
            speed_mmps: rider.riding.planar_speed_mmps,
            origin: command.origin,
            direction: prepared.resolved_direction,
            result: outcome,
            reject_reason: None,
            target_id,
            hit_zone,
            damage,
            distance_mm,
        };
        let prelaunch_horizontal_velocity_mmps = prepared
            .dive_id
            .and_then(|dive_id| {
                shooter
                    .kernel
                    .dive_fire_context()
                    .filter(|context| context.dive_id == dive_id)
                    .map(|context| context.prelaunch_horizontal_velocity_mmps)
            })
            .unwrap_or([0; 2]);
        let accepted_shot = AcceptedShotMetadata {
            shooter: command.shooter_peer_id,
            tick: command.tick,
            accepted_shot_index: prepared.accepted_shot_index,
            weapon_id: prepared.weapon_id,
            stance: rider.riding.stance,
            gait: rider.riding.gait,
            dive_id: prepared.dive_id,
            prelaunch_horizontal_velocity_mmps,
        };
        let recorded = self.shot_ledger.record_accepted(accepted_shot);
        debug_assert!(
            recorded,
            "authority admits at most one accepted shooter/tick"
        );
        let attribution = self
            .shot_ledger
            .observe_result(self.authority_epoch, &result);
        AuthorityShot {
            result,
            telemetry,
            dive_fire_rejection: None,
            accepted_shot: Some(accepted_shot),
            gameplay_events: attribution.events,
        }
    }
}

fn rejected_authority_shot(
    command: &ShotCommand,
    authority_weapon: WeaponId,
    ammo: WeaponAmmo,
    riding: RidingState,
    tick_rate: u32,
    reason: ShotRejectionReason,
) -> AuthorityShot {
    rejected_authority_shot_detailed(
        command,
        authority_weapon,
        ammo,
        riding,
        tick_rate,
        reason,
        None,
    )
}

fn rejected_authority_shot_detailed(
    command: &ShotCommand,
    authority_weapon: WeaponId,
    ammo: WeaponAmmo,
    riding: RidingState,
    tick_rate: u32,
    reason: ShotRejectionReason,
    dive_fire_rejection: Option<DiveFireRejection>,
) -> AuthorityShot {
    let spread = effective_spread_millidegrees(*authority_weapon.stats(), riding);
    let sway = deterministic_sway(command.tick, tick_rate, riding);
    let result = ShotResult {
        tick: command.tick,
        shooter_peer_id: command.shooter_peer_id,
        weapon_id: authority_weapon,
        outcome: ShotOutcome::Reject,
        rejection_reason: Some(reason),
        resolved_direction: None,
        target_id: None,
        hit_zone: None,
        damage: 0,
        distance_mm: None,
        eliminated: false,
    };
    let telemetry = ShotTelemetry {
        tick: command.tick,
        shooter: command.shooter_peer_id,
        weapon_id: authority_weapon,
        ammo_mag: ammo.magazine,
        ammo_reserve: ammo.reserve,
        spread_millidegrees: spread,
        sway_millidegrees: sway.magnitude_millidegrees(),
        gait: riding.gait,
        stance: riding.stance,
        speed_mmps: riding.planar_speed_mmps,
        origin: command.origin,
        direction: command.direction,
        result: ShotOutcome::Reject,
        reject_reason: Some(reason),
        target_id: None,
        hit_zone: None,
        damage: 0,
        distance_mm: None,
    };
    AuthorityShot {
        result,
        telemetry,
        dive_fire_rejection,
        accepted_shot: None,
        gameplay_events: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ_VALUES: [u32; 3] = [30, 60, 120];
    const LOBBY_SEED: u64 = 0x0123_4567_89ab_cdef;

    fn player(number: u64) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{number:012x}")).unwrap()
    }

    fn forward() -> QuantizedDirection {
        QuantizedDirection::new(0, 0, -DIRECTION_UNITS)
    }

    fn riding() -> RidingState {
        RidingState::default()
    }

    fn kernel(hz: u32, weapon: WeaponId) -> CombatKernel {
        CombatKernel::with_weapon(hz, LOBBY_SEED, player(1), weapon).unwrap()
    }

    fn target_pose(entity_id: u64, tick: u64, z_mm: i32, head_y_mm: i32) -> TargetPoseSnapshot {
        TargetPoseSnapshot {
            tick: SimulationTick::new(tick),
            entity_id: EntityId(entity_id),
            stance: RiderStance::Mounted,
            body_center: QuantizedOrigin::new(0, 900, z_mm),
            body_half_height_mm: 400,
            body_radius_mm: 300,
            head_center: QuantizedOrigin::new(0, head_y_mm, z_mm),
            head_radius_mm: 200,
            active: true,
        }
    }

    fn register_target(registry: &mut TargetRegistry, entity_id: u64, team: u16, health: u16) {
        registry
            .register(TargetDefinition {
                entity_id: EntityId(entity_id),
                owner_peer_id: None,
                team_id: TeamId(team),
                max_health: health,
            })
            .unwrap();
    }

    fn command(
        authority: &CombatAuthority,
        shooter: PlayerId,
        weapon: WeaponId,
        tick: u64,
        origin: QuantizedOrigin,
        direction: QuantizedDirection,
    ) -> ShotCommand {
        ShotCommand {
            tick: SimulationTick::new(tick),
            shooter_peer_id: shooter,
            weapon_id: weapon,
            origin,
            direction,
            spread_seed: authority.expected_spread_seed(shooter).unwrap(),
            claimed_target: None,
        }
    }

    fn rider_snapshot(shooter: PlayerId, tick: u64, muzzle: QuantizedOrigin) -> RiderSnapshot {
        RiderSnapshot {
            tick: SimulationTick::new(tick),
            shooter_peer_id: shooter,
            muzzle_origin: muzzle,
            team_id: TeamId(1),
            riding: riding(),
        }
    }

    #[test]
    fn cadence_ammo_and_dry_fire_hold_at_all_supported_rates() {
        for hz in HZ_VALUES {
            for weapon in WeaponId::ALL {
                let mut kernel = kernel(hz, weapon);
                let cadence = weapon.stats().cadence_ticks(hz);
                let first = kernel
                    .request_fire(SimulationTick::new(0), forward(), riding())
                    .unwrap();
                assert_eq!(
                    first.ammo.magazine,
                    weapon.stats().magazine_capacity - 1,
                    "{weapon:?} at {hz} Hz"
                );
                assert_eq!(
                    kernel.request_fire(SimulationTick::new(cadence - 1), forward(), riding()),
                    Err(ShotRejectionReason::Rate),
                    "{weapon:?} at {hz} Hz"
                );
                assert!(kernel
                    .request_fire(SimulationTick::new(cadence), forward(), riding())
                    .is_ok());

                kernel.set_ammo(
                    weapon,
                    WeaponAmmo {
                        magazine: 0,
                        reserve: weapon.stats().reserve_capacity,
                    },
                );
                assert_eq!(
                    kernel.request_fire(SimulationTick::new(cadence * 2), forward(), riding()),
                    Err(ShotRejectionReason::Empty)
                );
            }
        }
    }

    #[test]
    fn reload_blocks_fire_completes_and_pauses_for_sprint_gallop() {
        for hz in HZ_VALUES {
            for weapon in WeaponId::ALL {
                let mut kernel = kernel(hz, weapon);
                let stats = *weapon.stats();
                kernel.set_ammo(
                    weapon,
                    WeaponAmmo {
                        magazine: 1,
                        reserve: stats.reserve_capacity,
                    },
                );
                let required = kernel
                    .request_reload(SimulationTick::new(0), riding())
                    .unwrap()
                    .required_ticks;
                assert_eq!(required, stats.reload_ticks(hz));
                assert_eq!(
                    kernel.request_fire(SimulationTick::new(1), forward(), riding()),
                    Err(ShotRejectionReason::Reloading)
                );

                let mut paused = riding();
                paused.gait = CombatGait::Gallop;
                paused.sprint_gallop = true;
                kernel
                    .advance_to(SimulationTick::new(required), paused)
                    .unwrap();
                assert_eq!(kernel.reload().unwrap().active_ticks, 1);

                let finish_tick = required + (required - 1);
                let outcome = kernel
                    .advance_to(SimulationTick::new(finish_tick), riding())
                    .unwrap();
                assert!(outcome.reload_completed);
                assert_eq!(kernel.equipped_ammo().magazine, stats.magazine_capacity);
                assert_eq!(
                    kernel.equipped_ammo().reserve,
                    stats.reserve_capacity - (stats.magazine_capacity - 1)
                );
            }
        }
    }

    #[test]
    fn deterministic_spread_vectors_are_pinned_and_seeded_by_shot_index() {
        let mut first = kernel(60, WeaponId::Dustwalker);
        let mut second = first.clone();
        let shot_a = first
            .request_fire(SimulationTick::new(0), forward(), riding())
            .unwrap();
        let shot_b = second
            .request_fire(SimulationTick::new(0), forward(), riding())
            .unwrap();
        assert_eq!(shot_a, shot_b);
        assert_eq!(shot_a.spread_seed, 0xe7fd_0801_7441_664a);
        assert_eq!(
            shot_a.resolved_direction,
            QuantizedDirection::new(9_445, 6_122, -999_937)
        );

        let cadence = WeaponId::Dustwalker.stats().cadence_ticks(60);
        let next = first
            .request_fire(SimulationTick::new(cadence), forward(), riding())
            .unwrap();
        assert_ne!(shot_a.spread_seed, next.spread_seed);
        assert_ne!(shot_a.resolved_direction, next.resolved_direction);
    }

    #[test]
    fn equal_wall_time_sway_and_spread_match_at_all_supported_rates() {
        let mut reference = None;
        for hz in HZ_VALUES {
            let mut gallop = riding();
            gallop.gait = CombatGait::Gallop;
            gallop.planar_speed_mmps = 14_000;
            gallop.gait_top_speed_mmps = 14_000;
            let mut kernel = kernel(hz, WeaponId::Rattler);
            let shot = kernel
                .request_fire(SimulationTick::new(u64::from(hz)), forward(), gallop)
                .unwrap();
            let sample = (
                shot.spread_seed,
                shot.resolved_direction,
                shot.spread_millidegrees,
                shot.sway,
            );
            if let Some(reference) = reference {
                assert_eq!(sample, reference, "{hz} Hz deterministic handling");
            } else {
                reference = Some(sample);
            }
        }
    }

    #[test]
    fn majestic_charge_scales_only_authority_sway_to_seventy_percent() {
        for tick in [1, 17, 60, 359] {
            let normal = RidingState {
                gait: CombatGait::Gallop,
                yaw_rate_millidegrees_per_second: 45_000,
                ..RidingState::default()
            };
            let charged = RidingState {
                majestic_charge: true,
                ..normal
            };
            let normal_sample = deterministic_sway(SimulationTick::new(tick), 60, normal);
            assert_eq!(
                deterministic_sway(SimulationTick::new(tick), 60, charged),
                scale_sway(normal_sample, CHARGE_SWAY_MULTIPLIER_MILLI)
            );
            assert_eq!(
                effective_spread_millidegrees(*WeaponId::Dustwalker.stats(), charged),
                effective_spread_millidegrees(*WeaponId::Dustwalker.stats(), normal)
            );
        }
    }

    #[test]
    fn spread_ads_gallop_turn_and_airborne_penalties_are_deterministic() {
        for weapon in WeaponId::ALL {
            let stats = *weapon.stats();
            let idle = riding();
            assert_eq!(
                effective_spread_millidegrees(stats, idle),
                stats.base_spread_millidegrees
            );

            let mut gallop = riding();
            gallop.gait = CombatGait::Gallop;
            gallop.planar_speed_mmps = 14_000;
            gallop.gait_top_speed_mmps = 14_000;
            assert_eq!(
                effective_spread_millidegrees(stats, gallop),
                stats.gallop_spread_millidegrees
            );
            gallop.ads = true;
            assert_eq!(
                effective_spread_millidegrees(stats, gallop),
                (stats.gallop_spread_millidegrees * 3 + 2) / 5
            );
            gallop.ads = false;
            gallop.yaw_rate_millidegrees_per_second = 120_000;
            assert!(
                effective_spread_millidegrees(stats, gallop) > stats.gallop_spread_millidegrees
            );
            gallop.stance = RiderStance::MountedAirborne;
            assert!(
                effective_spread_millidegrees(stats, gallop) > stats.gallop_spread_millidegrees
            );

            let mut airborne_kernel = kernel(60, weapon);
            assert_eq!(
                airborne_kernel.request_fire(SimulationTick::new(0), forward(), gallop),
                Err(ShotRejectionReason::Airborne)
            );
        }
    }

    #[test]
    fn recoil_accumulates_and_recovers_at_six_degrees_per_second() {
        for hz in HZ_VALUES {
            let mut kernel = kernel(hz, WeaponId::Longspur);
            let shot = kernel
                .request_fire(SimulationTick::new(0), forward(), riding())
                .unwrap();
            assert_eq!(shot.recoil.pitch_millidegrees, 1_100);
            assert!(shot.recoil.yaw_millidegrees.unsigned_abs() <= 350);
            kernel
                .advance_to(SimulationTick::new(u64::from(hz) / 10), riding())
                .unwrap();
            assert_eq!(kernel.recoil().pitch_millidegrees, 500);
            kernel
                .advance_to(SimulationTick::new(u64::from(hz)), riding())
                .unwrap();
            assert_eq!(kernel.recoil(), RecoilState::default());
        }
    }

    #[test]
    fn pickup_swaps_retains_ammo_expires_and_duplicate_caps_reserve() {
        let mut kernel = kernel(60, WeaponId::Dustwalker);
        kernel.set_ammo(
            WeaponId::Dustwalker,
            WeaponAmmo {
                magazine: 7,
                reserve: 11,
            },
        );
        let pickup = WeaponPickup::world_spawn(WeaponId::Longspur, SimulationTick::new(10));
        assert_eq!(
            kernel.pickup(pickup, PICKUP_RANGE_MM + 1, SimulationTick::new(10)),
            Err(PickupError::OutOfRange)
        );
        let swapped = kernel
            .pickup(pickup, PICKUP_RANGE_MM, SimulationTick::new(10))
            .unwrap();
        let PickupOutcome::Swapped { dropped, .. } = swapped else {
            panic!("expected swap");
        };
        assert_eq!(dropped.ammo.magazine, 7);
        assert_eq!(dropped.ammo.reserve, 11);
        assert_eq!(dropped.expires_at, Some(SimulationTick::new(1_810)));
        assert!(!dropped.is_expired(SimulationTick::new(1_809)));
        assert!(dropped.is_expired(SimulationTick::new(1_810)));
        assert_eq!(
            kernel.equipped_ammo(),
            WeaponAmmo {
                magazine: 18,
                reserve: 36
            }
        );

        let duplicate = kernel.pickup(pickup, 0, SimulationTick::new(11)).unwrap();
        assert_eq!(
            duplicate,
            PickupOutcome::AmmoOnly {
                weapon_id: WeaponId::Longspur,
                added_reserve: 36,
                reserve: 72,
            }
        );
    }

    #[test]
    fn authority_enforces_cadence_ammo_and_reload_at_all_supported_rates() {
        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_600, 0);
        for hz in HZ_VALUES {
            let mut targets = TargetRegistry::new(hz).unwrap();

            let mut cadence_authority = CombatAuthority::new(hz, LOBBY_SEED).unwrap();
            cadence_authority.register_shooter(shooter, WeaponId::Dustwalker);
            let first = command(
                &cadence_authority,
                shooter,
                WeaponId::Dustwalker,
                100,
                muzzle,
                forward(),
            );
            assert_eq!(
                cadence_authority
                    .validate_shot(
                        &first,
                        SimulationTick::new(100),
                        rider_snapshot(shooter, 100, muzzle),
                        &mut targets,
                    )
                    .result
                    .outcome,
                ShotOutcome::Miss
            );
            let too_fast_tick = 100 + WeaponId::Dustwalker.stats().cadence_ticks(hz) - 1;
            let too_fast = command(
                &cadence_authority,
                shooter,
                WeaponId::Dustwalker,
                too_fast_tick,
                muzzle,
                forward(),
            );
            assert_eq!(
                cadence_authority
                    .validate_shot(
                        &too_fast,
                        SimulationTick::new(too_fast_tick),
                        rider_snapshot(shooter, too_fast_tick, muzzle),
                        &mut targets,
                    )
                    .result
                    .rejection_reason,
                Some(ShotRejectionReason::Rate)
            );

            let mut empty_authority = CombatAuthority::new(hz, LOBBY_SEED).unwrap();
            empty_authority.register_shooter(shooter, WeaponId::Dustwalker);
            empty_authority
                .shooter_kernel_mut(shooter)
                .unwrap()
                .set_ammo(
                    WeaponId::Dustwalker,
                    WeaponAmmo {
                        magazine: 0,
                        reserve: 120,
                    },
                );
            let empty = command(
                &empty_authority,
                shooter,
                WeaponId::Dustwalker,
                100,
                muzzle,
                forward(),
            );
            assert_eq!(
                empty_authority
                    .validate_shot(
                        &empty,
                        SimulationTick::new(100),
                        rider_snapshot(shooter, 100, muzzle),
                        &mut targets,
                    )
                    .result
                    .rejection_reason,
                Some(ShotRejectionReason::Empty)
            );

            let mut reload_authority = CombatAuthority::new(hz, LOBBY_SEED).unwrap();
            reload_authority.register_shooter(shooter, WeaponId::Dustwalker);
            let reload_kernel = reload_authority.shooter_kernel_mut(shooter).unwrap();
            reload_kernel.set_ammo(
                WeaponId::Dustwalker,
                WeaponAmmo {
                    magazine: 1,
                    reserve: 120,
                },
            );
            reload_kernel
                .request_reload(SimulationTick::new(99), riding())
                .unwrap();
            let reloading = command(
                &reload_authority,
                shooter,
                WeaponId::Dustwalker,
                100,
                muzzle,
                forward(),
            );
            assert_eq!(
                reload_authority
                    .validate_shot(
                        &reloading,
                        SimulationTick::new(100),
                        rider_snapshot(shooter, 100, muzzle),
                        &mut targets,
                    )
                    .result
                    .rejection_reason,
                Some(ShotRejectionReason::Reloading)
            );
        }
    }

    #[test]
    fn duplicate_registration_cannot_reset_replay_ammo_or_dive_state() {
        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_600, 0);
        let mut authority = CombatAuthority::new(60, LOBBY_SEED).unwrap();
        assert!(authority.register_shooter(shooter, WeaponId::Dustwalker));
        let mut targets = TargetRegistry::new(60).unwrap();
        let shot = command(
            &authority,
            shooter,
            WeaponId::Dustwalker,
            100,
            muzzle,
            forward(),
        );
        let first = authority.validate_shot(
            &shot,
            SimulationTick::new(100),
            rider_snapshot(shooter, 100, muzzle),
            &mut targets,
        );
        assert_eq!(first.result.outcome, ShotOutcome::Miss);

        let preserved = authority.clone();
        assert!(!authority.register_shooter(shooter, WeaponId::Rattler));
        assert_eq!(authority, preserved);
        let replay = authority.validate_shot(
            &shot,
            SimulationTick::new(100),
            rider_snapshot(shooter, 100, muzzle),
            &mut targets,
        );
        assert_eq!(
            replay.result.rejection_reason,
            Some(ShotRejectionReason::TickReplay)
        );
        assert_eq!(
            authority
                .shooter_kernel(shooter)
                .expect("shooter remains registered")
                .equipped_weapon(),
            WeaponId::Dustwalker
        );
    }

    #[test]
    fn authority_rejects_invalid_vectors_tick_bounds_replays_and_origin_leash() {
        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_600, 0);
        let mut authority = CombatAuthority::new(60, LOBBY_SEED).unwrap();
        authority.register_shooter(shooter, WeaponId::Dustwalker);
        let mut targets = TargetRegistry::new(60).unwrap();

        let future = command(
            &authority,
            shooter,
            WeaponId::Dustwalker,
            101,
            muzzle,
            forward(),
        );
        assert_eq!(
            authority
                .validate_shot(
                    &future,
                    SimulationTick::new(100),
                    rider_snapshot(shooter, 101, muzzle),
                    &mut targets
                )
                .result
                .rejection_reason,
            Some(ShotRejectionReason::TickFuture)
        );

        let stale = command(
            &authority,
            shooter,
            WeaponId::Dustwalker,
            84,
            muzzle,
            forward(),
        );
        assert_eq!(
            authority
                .validate_shot(
                    &stale,
                    SimulationTick::new(100),
                    rider_snapshot(shooter, 84, muzzle),
                    &mut targets
                )
                .result
                .rejection_reason,
            Some(ShotRejectionReason::TickStale)
        );

        let invalid = command(
            &authority,
            shooter,
            WeaponId::Dustwalker,
            90,
            muzzle,
            QuantizedDirection::new(0, 0, 0),
        );
        assert_eq!(
            authority
                .validate_shot(
                    &invalid,
                    SimulationTick::new(100),
                    rider_snapshot(shooter, 90, muzzle),
                    &mut targets
                )
                .result
                .rejection_reason,
            Some(ShotRejectionReason::InvalidDirection)
        );
        assert_eq!(
            authority
                .validate_shot(
                    &invalid,
                    SimulationTick::new(100),
                    rider_snapshot(shooter, 90, muzzle),
                    &mut targets
                )
                .result
                .rejection_reason,
            Some(ShotRejectionReason::TickReplay)
        );

        let far_origin = QuantizedOrigin::new(1_501, 1_600, 0);
        let leashed = command(
            &authority,
            shooter,
            WeaponId::Dustwalker,
            91,
            far_origin,
            forward(),
        );
        assert_eq!(
            authority
                .validate_shot(
                    &leashed,
                    SimulationTick::new(100),
                    rider_snapshot(shooter, 91, muzzle),
                    &mut targets
                )
                .result
                .rejection_reason,
            Some(ShotRejectionReason::OriginLeash)
        );
    }

    #[test]
    fn authority_uses_nearest_target_id_tie_break_range_and_server_damage() {
        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_650, 0);
        let mut authority = CombatAuthority::new(60, LOBBY_SEED).unwrap();
        authority.register_shooter(shooter, WeaponId::Longspur);
        let mut targets = TargetRegistry::new(60).unwrap();
        register_target(&mut targets, 9, 2, 100);
        register_target(&mut targets, 3, 2, 100);
        targets
            .record_pose(target_pose(9, 100, -20_000, 1_650))
            .unwrap();
        targets
            .record_pose(target_pose(3, 100, -20_000, 1_650))
            .unwrap();

        let mut shot = command(
            &authority,
            shooter,
            WeaponId::Longspur,
            100,
            muzzle,
            forward(),
        );
        shot.claimed_target = Some(ClaimedTarget {
            target_id: EntityId(9),
            hit_zone: Some(HitZone::Body),
            damage: Some(u16::MAX),
            distance_mm: Some(1),
        });
        let resolved = authority.validate_shot(
            &shot,
            SimulationTick::new(100),
            rider_snapshot(shooter, 100, muzzle),
            &mut targets,
        );
        assert_eq!(resolved.result.outcome, ShotOutcome::Hit);
        assert_eq!(resolved.result.target_id, Some(EntityId(3)));
        assert_eq!(resolved.result.hit_zone, Some(HitZone::Head));
        assert_eq!(resolved.result.damage, 57);
        assert_eq!(targets.health(EntityId(3)), Some(43));
        assert_eq!(targets.health(EntityId(9)), Some(100));

        let mut far_authority = CombatAuthority::new(60, LOBBY_SEED).unwrap();
        far_authority.register_shooter(shooter, WeaponId::Rattler);
        let mut far_targets = TargetRegistry::new(60).unwrap();
        register_target(&mut far_targets, 1, 2, 100);
        far_targets
            .record_pose(target_pose(1, 100, -151_000, 1_650))
            .unwrap();
        let far = command(
            &far_authority,
            shooter,
            WeaponId::Rattler,
            100,
            muzzle,
            forward(),
        );
        let result = far_authority.validate_shot(
            &far,
            SimulationTick::new(100),
            rider_snapshot(shooter, 100, muzzle),
            &mut far_targets,
        );
        assert_eq!(result.result.outcome, ShotOutcome::Miss);
        assert_eq!(far_targets.health(EntityId(1)), Some(100));
    }

    #[test]
    fn target_registry_enforces_the_exact_hitscan_clamp() {
        let shooter = player(1);
        let origin = QuantizedOrigin::new(0, 1_650, 0);
        let mut targets = TargetRegistry::new(60).unwrap();
        register_target(&mut targets, 1, 2, 100);
        targets
            .record_pose(target_pose(1, 10, -150_500, 1_650))
            .unwrap();

        assert!(targets
            .nearest_hit(
                SimulationTick::new(10),
                origin,
                forward(),
                150_000,
                shooter,
                TeamId(1),
            )
            .is_none());
        let hit = targets
            .nearest_hit(
                SimulationTick::new(10),
                origin,
                forward(),
                151_000,
                shooter,
                TeamId(1),
            )
            .expect("target enters the longer clamp");
        assert_eq!(hit.target_id, EntityId(1));
        assert_eq!(hit.hit_zone, HitZone::Head);
        assert_eq!(hit.distance_mm, 150_300);
    }

    #[test]
    fn horse_body_rewind_uses_a_horizontal_oriented_capsule() {
        let shooter = player(1);
        let mut targets = TargetRegistry::new(60).unwrap();
        register_target(&mut targets, 2, 2, 320);
        targets
            .record_horse_pose(HorseTargetPoseSnapshot {
                tick: SimulationTick::new(10),
                entity_id: EntityId(2),
                body_center: QuantizedOrigin::new(0, 1_000, -10_000),
                body_forward: QuantizedDirection::new(DIRECTION_UNITS, 0, 0),
                body_half_length_mm: 900,
                body_radius_mm: 650,
                head_center: QuantizedOrigin::new(0, 3_000, -10_000),
                head_radius_mm: 350,
                active: true,
            })
            .unwrap();

        let broadside = targets
            .nearest_hit(
                SimulationTick::new(10),
                QuantizedOrigin::new(800, 1_000, 0),
                forward(),
                20_000,
                shooter,
                TeamId(0),
            )
            .expect("ray inside the longitudinal segment must hit the horse body");
        assert_eq!(broadside.target_id, EntityId(2));
        assert_eq!(broadside.hit_zone, HitZone::Body);
        assert_eq!(broadside.distance_mm, 9_350);
        assert!(targets
            .nearest_hit(
                SimulationTick::new(10),
                QuantizedOrigin::new(1_600, 1_000, 0),
                forward(),
                20_000,
                shooter,
                TeamId(0),
            )
            .is_none());
    }

    #[test]
    fn body_hit_counts_match_sidegrade_tte_contract() {
        for (weapon, hits_to_eliminate) in [
            (WeaponId::Dustwalker, 8_u16),
            (WeaponId::Longspur, 4_u16),
            (WeaponId::Rattler, 12_u16),
        ] {
            let damage = weapon.stats().damage_at(0, HitZone::Body);
            assert!((hits_to_eliminate - 1) * damage < 100);
            assert!(hits_to_eliminate * damage >= 100);
        }
    }

    #[test]
    fn k09_stance_legality_and_authority_context_are_conservative() {
        let mut mounted = kernel(60, WeaponId::Dustwalker);
        assert!(mounted
            .request_fire(SimulationTick::new(0), forward(), riding())
            .is_ok());

        let mut jump = riding();
        jump.stance = RiderStance::MountedAirborne;
        let mut jump_kernel = kernel(60, WeaponId::Dustwalker);
        assert_eq!(
            jump_kernel.request_fire(SimulationTick::new(0), forward(), jump),
            Err(ShotRejectionReason::Airborne)
        );

        let dive_id = DiveId::new(1).unwrap();
        let mut dive_kernel = kernel(60, WeaponId::Dustwalker);
        dive_kernel
            .begin_saddle_dive(
                dive_id,
                SimulationTick::new(0),
                WeaponId::Dustwalker,
                [0, -8_000],
                45,
            )
            .unwrap();
        let mut dive = riding();
        dive.stance = RiderStance::SaddleDiveAirborne;
        dive.dive_id = Some(dive_id);
        assert!(dive_kernel
            .request_fire(SimulationTick::new(0), forward(), dive)
            .is_ok());

        let before_spoof = dive_kernel.clone();
        let spoofed_mounted = dive_kernel
            .request_fire_detailed(
                SimulationTick::new(1),
                forward(),
                riding(),
                dive_kernel.next_spread_seed(),
            )
            .unwrap_err();
        assert_eq!(
            spoofed_mounted.dive_reason,
            Some(DiveFireRejection::ContextMismatch)
        );
        assert_eq!(dive_kernel, before_spoof);

        let mut mismatch = dive;
        mismatch.dive_id = DiveId::new(2);
        let before = dive_kernel.clone();
        let rejection = dive_kernel
            .request_fire_detailed(
                SimulationTick::new(1),
                forward(),
                mismatch,
                dive_kernel.next_spread_seed(),
            )
            .unwrap_err();
        assert_eq!(
            rejection.dive_reason,
            Some(DiveFireRejection::ContextMismatch)
        );
        assert_eq!(dive_kernel, before);

        for stance in [
            RiderStance::LandingProne,
            RiderStance::LandingRecovery,
            RiderStance::OnFootStanding,
            RiderStance::Unknown(200),
        ] {
            let mut state = riding();
            state.stance = stance;
            let mut candidate = kernel(60, WeaponId::Dustwalker);
            assert_eq!(
                candidate.request_fire(SimulationTick::new(0), forward(), state),
                Err(ShotRejectionReason::Dismounted)
            );
        }

        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_600, 0);
        let mut authority = CombatAuthority::new(60, LOBBY_SEED).unwrap();
        authority.register_shooter(shooter, WeaponId::Dustwalker);
        let before = authority.shooter_kernel(shooter).unwrap().clone();
        let command = command(
            &authority,
            shooter,
            WeaponId::Dustwalker,
            1,
            muzzle,
            forward(),
        );
        let mut unknown = rider_snapshot(shooter, 1, muzzle);
        unknown.riding.stance = RiderStance::Unknown(200);
        let mut targets = TargetRegistry::new(60).unwrap();
        let rejected =
            authority.validate_shot(&command, SimulationTick::new(1), unknown, &mut targets);
        assert_eq!(
            rejected.result.rejection_reason,
            Some(ShotRejectionReason::RiderSnapshot)
        );
        assert_eq!(authority.shooter_kernel(shooter).unwrap(), &before);
    }

    #[test]
    fn k10_per_weapon_caps_precede_every_weapon_mutation_and_new_dive_resets() {
        for weapon in WeaponId::ALL {
            let mut kernel = kernel(60, weapon);
            let first_dive = DiveId::new(1).unwrap();
            kernel
                .begin_saddle_dive(first_dive, SimulationTick::new(0), weapon, [0, -8_000], 45)
                .unwrap();
            let mut dive = riding();
            dive.stance = RiderStance::SaddleDiveAirborne;
            dive.dive_id = Some(first_dive);
            let cadence = weapon.stats().cadence_ticks(60);
            for index in 0..u64::from(dive_shot_cap(weapon)) {
                assert!(kernel
                    .request_fire(SimulationTick::new(index * cadence), forward(), dive)
                    .is_ok());
            }
            let cap_tick = u64::from(dive_shot_cap(weapon)) * cadence;
            let before = kernel.clone();
            let rejection = kernel
                .request_fire_detailed(
                    SimulationTick::new(cap_tick),
                    forward(),
                    dive,
                    kernel.next_spread_seed(),
                )
                .unwrap_err();
            assert_eq!(rejection.wire_reason, ShotRejectionReason::Rate);
            assert_eq!(rejection.dive_reason, Some(DiveFireRejection::ShotCap));
            assert_eq!(kernel, before, "{weapon:?} cap rejection mutated state");

            kernel
                .finish_saddle_dive(first_dive, SimulationTick::new(cap_tick))
                .unwrap();
            assert!(kernel.equip_weapon(weapon));
            let second_dive = DiveId::new(2).unwrap();
            kernel
                .advance_to(SimulationTick::new(cap_tick + cadence), riding())
                .unwrap();
            kernel
                .begin_saddle_dive(
                    second_dive,
                    SimulationTick::new(cap_tick + cadence),
                    weapon,
                    [0, -8_000],
                    45,
                )
                .unwrap();
            dive.dive_id = Some(second_dive);
            assert!(kernel
                .request_fire(SimulationTick::new(cap_tick + cadence), forward(), dive,)
                .is_ok());
            assert_eq!(kernel.dive_fire_context().unwrap().accepted_count, 1);
        }
    }

    #[test]
    fn k11_launch_cancels_reload_and_airborne_reload_or_equipment_mutate_nothing() {
        let mut kernel = CombatKernel::with_full_loadout(60, LOBBY_SEED, player(1)).unwrap();
        kernel.set_ammo(
            WeaponId::Dustwalker,
            WeaponAmmo {
                magazine: 10,
                reserve: 120,
            },
        );
        kernel
            .request_reload(SimulationTick::new(0), riding())
            .unwrap();
        let ammo = kernel.equipped_ammo();
        let dive_id = DiveId::new(1).unwrap();
        kernel
            .begin_saddle_dive(
                dive_id,
                SimulationTick::new(1),
                WeaponId::Dustwalker,
                [0, -8_000],
                45,
            )
            .unwrap();
        assert_eq!(kernel.reload(), None);
        assert_eq!(kernel.equipped_ammo(), ammo);

        let mut dive = riding();
        dive.stance = RiderStance::SaddleDiveAirborne;
        dive.dive_id = Some(dive_id);
        let before_reload = kernel.clone();
        assert_eq!(
            kernel.request_reload(SimulationTick::new(1), dive),
            Err(ReloadStartError::Airborne)
        );
        assert_eq!(kernel, before_reload);
        assert_eq!(
            kernel.request_reload(SimulationTick::new(1), riding()),
            Err(ReloadStartError::Airborne)
        );
        assert_eq!(kernel, before_reload);
        assert!(!kernel.equip_weapon(WeaponId::Longspur));
        assert_eq!(kernel, before_reload);
        assert_eq!(
            kernel.pickup(
                WeaponPickup::world_spawn(WeaponId::Longspur, SimulationTick::new(1)),
                0,
                SimulationTick::new(1),
            ),
            Err(PickupError::Airborne)
        );
        assert_eq!(kernel, before_reload);

        let required = WeaponId::Dustwalker.stats().reload_ticks(60);
        for (launch_tick, expected_magazine) in [(required, 10), (required + 1, 30)] {
            let mut boundary =
                CombatKernel::with_weapon(60, LOBBY_SEED, player(1), WeaponId::Dustwalker).unwrap();
            boundary.set_ammo(
                WeaponId::Dustwalker,
                WeaponAmmo {
                    magazine: 10,
                    reserve: 120,
                },
            );
            boundary
                .request_reload(SimulationTick::new(0), riding())
                .unwrap();
            boundary
                .begin_saddle_dive(
                    DiveId::new(2).unwrap(),
                    SimulationTick::new(launch_tick),
                    WeaponId::Dustwalker,
                    [0, -8_000],
                    45,
                )
                .unwrap();
            assert_eq!(boundary.equipped_ammo().magazine, expected_magazine);
            assert_eq!(boundary.reload(), None);
        }
    }

    #[test]
    fn k12_same_tick_launch_fire_reload_and_landing_order_are_explicit() {
        let dive_id = DiveId::new(1).unwrap();
        let mut kernel = kernel(60, WeaponId::Dustwalker);
        kernel
            .begin_saddle_dive(
                dive_id,
                SimulationTick::new(10),
                WeaponId::Dustwalker,
                [0, -8_000],
                45,
            )
            .unwrap();
        let mut dive = riding();
        dive.stance = RiderStance::SaddleDiveAirborne;
        dive.dive_id = Some(dive_id);
        assert!(kernel
            .request_fire(SimulationTick::new(10), forward(), dive)
            .is_ok());
        assert_eq!(
            kernel.request_reload(SimulationTick::new(10), dive),
            Err(ReloadStartError::Airborne)
        );
        assert!(kernel
            .finish_saddle_dive(dive_id, SimulationTick::new(10))
            .is_ok());
        let after_first_finish = kernel.clone();
        assert!(kernel
            .finish_saddle_dive(dive_id, SimulationTick::new(10))
            .is_ok());
        assert_eq!(kernel, after_first_finish);
        let mut prone = riding();
        prone.stance = RiderStance::LandingProne;
        assert_eq!(
            kernel.request_fire(SimulationTick::new(10), forward(), prone),
            Err(ShotRejectionReason::Dismounted)
        );
    }

    #[test]
    fn launch_captures_current_prelaunch_handling_without_advancing_the_clock() {
        let mut kernel = kernel(60, WeaponId::Dustwalker);
        let mut previous = riding();
        previous.gait = CombatGait::Walk;
        previous.planar_speed_mmps = 2_000;
        previous.gait_top_speed_mmps = 14_000;
        kernel.advance_to(SimulationTick::new(9), previous).unwrap();

        let mut current = previous;
        current.gait = CombatGait::Gallop;
        current.planar_speed_mmps = 14_000;
        current.yaw_rate_millidegrees_per_second = 60_000;
        current.ads = true;
        // Equal-tick installation changes only authoritative handling. The
        // launch call still owns progression/cancellation through tick 10.
        kernel.advance_to(SimulationTick::new(9), current).unwrap();
        let dive_id = DiveId::new(44).unwrap();
        kernel
            .begin_saddle_dive(
                dive_id,
                SimulationTick::new(10),
                WeaponId::Dustwalker,
                [0, -14_000],
                45,
            )
            .unwrap();
        assert_eq!(
            kernel
                .dive_fire_context()
                .expect("dive context opened")
                .launch_handling,
            current
        );
    }

    #[test]
    fn dive_sway_scales_only_offsets_and_decays_on_the_nominal_schedule() {
        let mut launch_handling = riding();
        launch_handling.gait = CombatGait::Gallop;
        launch_handling.planar_speed_mmps = 14_000;
        launch_handling.gait_top_speed_mmps = 14_000;
        let mut kernel = kernel(60, WeaponId::Rattler);
        kernel
            .advance_to(SimulationTick::new(0), launch_handling)
            .unwrap();
        let dive_id = DiveId::new(1).unwrap();
        kernel
            .begin_saddle_dive(
                dive_id,
                SimulationTick::new(0),
                WeaponId::Rattler,
                [0, -14_000],
                45,
            )
            .unwrap();
        let mut dive = launch_handling;
        dive.stance = RiderStance::SaddleDiveAirborne;
        dive.dive_id = Some(dive_id);
        let shot = kernel
            .request_fire(SimulationTick::new(1), forward(), dive)
            .unwrap();
        let normal = deterministic_sway(SimulationTick::new(1), 60, launch_handling);
        assert_eq!(shot.sway, scale_sway(normal, 600));
        assert_eq!(
            shot.spread_millidegrees,
            effective_spread_millidegrees(*WeaponId::Rattler.stats(), launch_handling)
        );
        assert!(shot.sway.magnitude_millidegrees() <= normal.magnitude_millidegrees());
    }

    #[test]
    fn combat_authority_emits_deduplicated_dive_events_with_epoch_and_late_ledger() {
        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_650, 0);
        let mut authority = CombatAuthority::new(60, LOBBY_SEED).unwrap();
        assert!(authority.set_authority_epoch(7));
        assert!(!authority.set_authority_epoch(6));
        authority.register_shooter(shooter, WeaponId::Longspur);
        let dive_id = DiveId::new(4).unwrap();
        authority
            .begin_saddle_dive(
                shooter,
                dive_id,
                SimulationTick::new(100),
                WeaponId::Longspur,
                [0, 8_000],
                45,
            )
            .unwrap();
        let mut targets = TargetRegistry::new(60).unwrap();
        register_target(&mut targets, 9, 2, 200);
        let mut pose = target_pose(9, 100, -20_000, 1_650);
        pose.head_radius_mm = 5_000;
        targets.record_pose(pose).unwrap();
        let shot = command(
            &authority,
            shooter,
            WeaponId::Longspur,
            100,
            muzzle,
            forward(),
        );
        let mut rider = rider_snapshot(shooter, 100, muzzle);
        rider.riding.stance = RiderStance::SaddleDiveAirborne;
        rider.riding.dive_id = Some(dive_id);
        rider.riding.gait = CombatGait::Gallop;
        let resolved =
            authority.validate_shot(&shot, SimulationTick::new(100), rider, &mut targets);
        assert_eq!(resolved.result.outcome, ShotOutcome::Hit);
        assert_eq!(resolved.telemetry.stance, RiderStance::SaddleDiveAirborne);
        assert_eq!(resolved.accepted_shot.unwrap().dive_id, Some(dive_id));
        assert_eq!(
            resolved
                .gameplay_events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![
                crate::GameplayEventKind::SaddleDiveHeadshot,
                crate::GameplayEventKind::AirborneReversal,
            ]
        );
        assert!(resolved
            .gameplay_events
            .iter()
            .all(|event| event.id.authority_epoch == 7));
        let replay = authority.observe_authority_result(&resolved.result);
        assert!(replay.duplicate);
        assert!(replay.events.is_empty());
    }

    #[test]
    fn two_peers_replay_one_thousand_commands_bit_identically() {
        let shooter = player(1);
        let muzzle = QuantizedOrigin::new(0, 1_650, 0);
        let mut first = CombatAuthority::new(120, LOBBY_SEED).unwrap();
        let mut second = CombatAuthority::new(120, LOBBY_SEED).unwrap();
        first.register_shooter(shooter, WeaponId::Dustwalker);
        second.register_shooter(shooter, WeaponId::Dustwalker);
        first.shooter_kernel_mut(shooter).unwrap().set_ammo(
            WeaponId::Dustwalker,
            WeaponAmmo {
                magazine: u16::MAX,
                reserve: u16::MAX,
            },
        );
        second.shooter_kernel_mut(shooter).unwrap().set_ammo(
            WeaponId::Dustwalker,
            WeaponAmmo {
                magazine: u16::MAX,
                reserve: u16::MAX,
            },
        );
        // Capacity clamping is intentional, so this stream contains identical
        // hits, rate rejections, and empty rejections after round 30.
        let mut first_targets = TargetRegistry::new(120).unwrap();
        let mut second_targets = TargetRegistry::new(120).unwrap();
        for registry in [&mut first_targets, &mut second_targets] {
            register_target(registry, 1, 2, 10_000);
            registry
                .record_pose(target_pose(1, 1_001, -25_000, 1_650))
                .unwrap();
        }

        for index in 0..1_000_u64 {
            let tick = 1_001 + index;
            let command = command(
                &first,
                shooter,
                WeaponId::Dustwalker,
                tick,
                muzzle,
                forward(),
            );
            let first_result = first.validate_shot(
                &command,
                SimulationTick::new(tick),
                rider_snapshot(shooter, tick, muzzle),
                &mut first_targets,
            );
            let second_result = second.validate_shot(
                &command,
                SimulationTick::new(tick),
                rider_snapshot(shooter, tick, muzzle),
                &mut second_targets,
            );
            assert_eq!(first_result, second_result, "command {index}");
        }
        assert_eq!(first, second);
        assert_eq!(first_targets, second_targets);
        assert!(first_targets.health(EntityId(1)).unwrap() < 10_000);
    }
}
