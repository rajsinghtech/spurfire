//! Deterministic M3 horse-vitality, on-foot, recall, and running-mount rules.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{QuantizedDirection, QuantizedOrigin, SimulationTick, WireVersion, DIRECTION_UNITS};

/// M3 continues to use the shared 60 Hz authority clock.
pub const M3_TICK_RATE_HZ: u32 = 60;
/// Ordinary on-foot walk speed.
pub const ON_FOOT_WALK_SPEED_MMPS: u32 = 2_000;
/// Maximum on-foot sprint speed.
pub const ON_FOOT_SPRINT_SPEED_MMPS: u32 = 4_500;
/// Crouched movement speed.
pub const ON_FOOT_CROUCH_SPEED_MMPS: u32 = 1_200;
/// Full continuous sprint capacity.
pub const ON_FOOT_STAMINA_TICKS: u32 = 4 * M3_TICK_RATE_HZ;
/// Empty-to-full stamina regeneration duration.
pub const ON_FOOT_STAMINA_REGEN_TICKS: u32 = 6 * M3_TICK_RATE_HZ;
/// Tactical-roll displacement duration.
pub const TACTICAL_ROLL_TICKS: u64 = M3_TICK_RATE_HZ as u64 / 2;
/// Tactical-roll distance.
pub const TACTICAL_ROLL_DISTANCE_MM: u32 = 3_500;
/// Tactical-roll fixed movement speed.
pub const TACTICAL_ROLL_SPEED_MMPS: u32 = 7_000;
/// Tactical-roll cooldown.
pub const TACTICAL_ROLL_COOLDOWN_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64 / 2;
/// Roll-exit sway impulse in thousandths.
pub const ROLL_EXIT_SWAY_IMPULSE_MILLI: u16 = 300;
/// Roll-exit sway decay duration.
pub const ROLL_EXIT_SWAY_DECAY_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64 / 5;
/// Input buffer shared by crouch-to-roll transitions.
pub const ON_FOOT_INPUT_BUFFER_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64 / 20;
/// Base horse-recall delay.
pub const BASE_RECALL_TICKS: u64 = 20 * M3_TICK_RATE_HZ as u64;
/// Minimum horse-recall delay after earned reductions.
pub const MIN_RECALL_TICKS: u64 = 8 * M3_TICK_RATE_HZ as u64;
/// Running-mount distance threshold.
pub const RUNNING_MOUNT_RANGE_MM: u32 = 4_000;
/// Stationary remount range after the horse completes its slide stop.
pub const STATIONARY_REMOUNT_RANGE_MM: u32 = 3_000;
/// Running-mount opportunity duration once the returning horse reaches the rider.
pub const RUNNING_MOUNT_WINDOW_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64 / 2;
/// Horse bolt duration after spook.
pub const HORSE_BOLT_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64;
/// Horse regeneration delay after its most recent damage.
pub const HORSE_REGEN_DELAY_TICKS: u64 = 6 * M3_TICK_RATE_HZ as u64;
/// Horse regeneration rate after the delay.
pub const HORSE_REGEN_HEALTH_PER_SECOND: u32 = 10;
/// Rider lateral throw distance when the horse spooks.
pub const SPOOK_THROW_DISTANCE_MM: u32 = 3_000;
/// No-input rider stun duration after the throw.
pub const SPOOK_STUN_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64 / 5;

const HOOFBEAT_TICKS: u64 = 2 * M3_TICK_RATE_HZ as u64;
const DUST_REVEAL_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64 / 2;
const GALLOP_IN_TICKS: u64 = 3 * M3_TICK_RATE_HZ as u64;

/// M3 changes signed input/snapshot/checkpoint canonicalization and therefore
/// starts a new gameplay wire major. The existing 1.2 transport remains active
/// until its send/receive path is replaced atomically.
pub const M3_WIRE_VERSION: WireVersion = WireVersion::new(2, 0);

/// Wire-v2 stance namespace. Keeping it separate prevents an M3 binary from
/// changing how active wire-1.2 packets interpret stance IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum M3RiderStance {
    /// Forced lateral throw/stun after horse loss.
    SpookStunned,
    /// Ordinary standing/walking on-foot state.
    Standing,
    /// Stamina-consuming sprint.
    Sprinting,
    /// Reduced-capsule crouch.
    Crouched,
    /// Direction-locked tactical roll.
    Rolling,
}

/// M3 horse row selected by the existing M0 archetype selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorseVitalityClass {
    /// Fast, fragile horse.
    Courser,
    /// Slow, durable horse.
    Warhorse,
    /// Agile middle-weight horse.
    Mustang,
}

impl HorseVitalityClass {
    /// Locked prototype vitality row.
    #[must_use]
    pub const fn max_health(self) -> u16 {
        match self {
            Self::Courser => 200,
            Self::Warhorse => 320,
            Self::Mustang => 250,
        }
    }
}

/// Replay identity for one authority-owned horse damage aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HorseDamageId {
    /// Authority epoch producing the damage.
    pub authority_epoch: u64,
    /// Original damage tick.
    pub tick: SimulationTick,
    /// Authority-unique sequence within the tick.
    pub sequence: u64,
}

/// One authority-owned horse damage command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorseDamageCommand {
    /// Replay identity.
    pub id: HorseDamageId,
    /// Saturating health damage.
    pub amount: u16,
    /// Collision-resolved horse position at damage time.
    pub horse_position: QuantizedOrigin,
    /// Attacker/projectile position used to select the bolt-away heading.
    pub damage_source_position: QuantizedOrigin,
}

/// Horse control state after M3 vitality is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorseVitalityState {
    /// Rider may control or retrieve the horse normally.
    Available,
    /// Zero-health horse is bolting away from the last damage source.
    Bolting,
    /// Bolt completed; this horse cannot be remounted and recall may begin.
    Despawned,
}

/// Result of one unique horse damage application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorseDamageApplication {
    /// Health before this command.
    pub health_before: u16,
    /// Health after saturating subtraction.
    pub health_after: u16,
    /// True only for the command that crossed to zero.
    pub spooked: bool,
    /// Integer planar vector pointing away from the damage source.
    pub bolt_away_delta_mm: [i32; 2],
    /// Locked lateral throw distance when `spooked` is true.
    pub rider_throw_distance_mm: u32,
    /// Locked no-input stun duration when `spooked` is true.
    pub rider_stun_ticks: u64,
    /// Spook throws never author fall damage.
    pub rider_fall_damage: bool,
}

/// Exactly-once M3 event emitted on the fatal horse-damage edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorseBoltedEvent {
    /// Damage receipt that caused the event; this is its replay identity.
    pub id: HorseDamageId,
    /// Horse row that was depleted.
    pub class: HorseVitalityClass,
    /// Integer planar vector pointing away from the last damage source.
    pub bolt_away_delta_mm: [i32; 2],
    /// Locked notification value. M3 logs it; M5 alone mutates score.
    pub notification_points: u16,
}

/// Composed result of authority-owned horse damage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorseDamageEffects {
    /// Unique damage application.
    pub application: HorseDamageApplication,
    /// Present exactly once on the fatal edge.
    pub horse_bolted: Option<HorseBoltedEvent>,
}

/// Deterministic, replay-safe horse vitality and bolt timer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorseVitalityKernel {
    class: HorseVitalityClass,
    health: u16,
    state: HorseVitalityState,
    bolt_started_tick: Option<SimulationTick>,
    bolt_away_delta_mm: [i32; 2],
    last_damage_id: Option<HorseDamageId>,
    current_tick: Option<SimulationTick>,
    last_damage_tick: Option<SimulationTick>,
    regen_units: u32,
}

impl HorseVitalityKernel {
    /// Creates a full-health horse.
    #[must_use]
    pub fn new(class: HorseVitalityClass) -> Self {
        Self {
            class,
            health: class.max_health(),
            state: HorseVitalityState::Available,
            bolt_started_tick: None,
            bolt_away_delta_mm: [0, 1],
            last_damage_id: None,
            current_tick: None,
            last_damage_tick: None,
            regen_units: 0,
        }
    }

    /// Current health.
    #[must_use]
    pub const fn health(&self) -> u16 {
        self.health
    }

    /// Maximum health for the selected archetype.
    #[must_use]
    pub const fn max_health(&self) -> u16 {
        self.class.max_health()
    }

    /// Current vitality state.
    #[must_use]
    pub const fn state(&self) -> HorseVitalityState {
        self.state
    }

    /// Fixed bolt-away heading numerator for the adapter to normalize.
    #[must_use]
    pub const fn bolt_away_delta_mm(&self) -> [i32; 2] {
        self.bolt_away_delta_mm
    }

    /// Applies a unique command. Duplicates and post-spook damage do not mutate state.
    pub fn apply_damage(&mut self, command: HorseDamageCommand) -> Option<HorseDamageApplication> {
        if self.state != HorseVitalityState::Available
            || self.last_damage_id.is_some_and(|last| command.id <= last)
            || self
                .current_tick
                .is_some_and(|current| command.id.tick != current)
        {
            return None;
        }
        self.last_damage_id = Some(command.id);
        let health_before = self.health;
        self.health = self.health.saturating_sub(command.amount);
        self.last_damage_tick = Some(command.id.tick);
        self.regen_units = 0;
        let spooked = health_before > 0 && self.health == 0;
        if spooked {
            self.state = HorseVitalityState::Bolting;
            self.bolt_started_tick = Some(command.id.tick);
            let away_x = command
                .horse_position
                .x
                .saturating_sub(command.damage_source_position.x);
            let away_z = command
                .horse_position
                .z
                .saturating_sub(command.damage_source_position.z);
            self.bolt_away_delta_mm = if away_x == 0 && away_z == 0 {
                [0, 1]
            } else {
                [away_x, away_z]
            };
        }
        Some(HorseDamageApplication {
            health_before,
            health_after: self.health,
            spooked,
            bolt_away_delta_mm: self.bolt_away_delta_mm,
            rider_throw_distance_mm: if spooked { SPOOK_THROW_DISTANCE_MM } else { 0 },
            rider_stun_ticks: if spooked { SPOOK_STUN_TICKS } else { 0 },
            rider_fall_damage: false,
        })
    }

    /// Advances the absolute bolt timer and reports the exact first despawn tick.
    pub fn advance_tick(
        &mut self,
        tick: SimulationTick,
    ) -> Result<Option<SimulationTick>, M3Error> {
        if self.current_tick.is_some_and(|current| tick <= current) {
            return Err(M3Error::TickReplay);
        }
        let previous_tick = self.current_tick;
        self.current_tick = Some(tick);
        if self.state == HorseVitalityState::Available && self.health < self.class.max_health() {
            let regen_boundary = self
                .last_damage_tick
                .map(|last| last.saturating_add(HORSE_REGEN_DELAY_TICKS));
            if let Some(boundary) = regen_boundary {
                let eligible_start = previous_tick
                    .map(|previous| previous.saturating_add(1))
                    .unwrap_or(boundary)
                    .as_u64()
                    .max(boundary.as_u64());
                if tick.as_u64() >= eligible_start {
                    let eligible_ticks = tick.as_u64() - eligible_start + 1;
                    let units = u64::from(self.regen_units).saturating_add(
                        eligible_ticks.saturating_mul(u64::from(HORSE_REGEN_HEALTH_PER_SECOND)),
                    );
                    let restored = units / u64::from(M3_TICK_RATE_HZ);
                    self.regen_units = u32::try_from(units % u64::from(M3_TICK_RATE_HZ))
                        .expect("regen remainder is bounded by tick rate");
                    self.health = self
                        .health
                        .saturating_add(u16::try_from(restored).unwrap_or(u16::MAX))
                        .min(self.class.max_health());
                    if self.health == self.class.max_health() {
                        self.regen_units = 0;
                    }
                }
            }
        }
        if self.state != HorseVitalityState::Bolting {
            return Ok(None);
        }
        let Some(started) = self.bolt_started_tick else {
            return Err(M3Error::InvalidState);
        };
        if tick.checked_duration_since(started).unwrap_or(0) >= HORSE_BOLT_TICKS {
            self.state = HorseVitalityState::Despawned;
            return Ok(Some(started.saturating_add(HORSE_BOLT_TICKS)));
        }
        Ok(None)
    }

    /// Completes a Majestic Return with the same archetype at full health.
    pub fn restore_returned_horse(&mut self) -> bool {
        if self.state != HorseVitalityState::Despawned {
            return false;
        }
        self.health = self.class.max_health();
        self.state = HorseVitalityState::Available;
        self.bolt_started_tick = None;
        self.bolt_away_delta_mm = [0, 1];
        self.regen_units = 0;
        true
    }

    fn state_is_valid(&self) -> bool {
        if self.health > self.class.max_health() || self.regen_units >= M3_TICK_RATE_HZ {
            return false;
        }
        if self.last_damage_id.map(|id| id.tick) != self.last_damage_tick
            || !tick_is_not_after(self.last_damage_tick, self.current_tick)
            || !tick_is_not_after(self.bolt_started_tick, self.current_tick)
            || (self.health == self.class.max_health() && self.regen_units != 0)
        {
            return false;
        }
        match self.state {
            HorseVitalityState::Available => self.health > 0 && self.bolt_started_tick.is_none(),
            HorseVitalityState::Bolting => {
                self.health == 0
                    && self.bolt_started_tick.is_some_and(|started| {
                        self.current_tick.is_none_or(|current| {
                            current.checked_duration_since(started).unwrap_or(0) < HORSE_BOLT_TICKS
                        })
                    })
            }
            HorseVitalityState::Despawned => {
                self.health == 0
                    && self.bolt_started_tick.is_some_and(|started| {
                        self.current_tick.is_some_and(|current| {
                            current.checked_duration_since(started).unwrap_or(0) >= HORSE_BOLT_TICKS
                        })
                    })
            }
        }
    }
}

/// Deterministic on-foot locomotion state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFootState {
    /// Forced lateral throw/stun after horse loss.
    SpookStunned,
    /// Ordinary upright movement.
    Standing,
    /// Stamina-consuming sprint.
    Sprinting,
    /// Held crouch with reduced capsule and sway.
    Crouched,
    /// Fixed-duration, direction-locked tactical roll.
    Rolling,
}

impl OnFootState {
    /// Snapshot stance for combat rewind and remote presentation.
    #[must_use]
    pub const fn stance(self) -> M3RiderStance {
        match self {
            Self::SpookStunned => M3RiderStance::SpookStunned,
            Self::Standing => M3RiderStance::Standing,
            Self::Sprinting => M3RiderStance::Sprinting,
            Self::Crouched => M3RiderStance::Crouched,
            Self::Rolling => M3RiderStance::Rolling,
        }
    }
}

/// Input for one on-foot authority tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnFootTickInput {
    /// Strictly increasing shared tick.
    pub tick: SimulationTick,
    /// Normalized planar desired direction; malformed values become no movement.
    pub move_direction: Option<QuantizedDirection>,
    /// Sprint level.
    pub sprint_pressed: bool,
    /// Crouch level. A rising edge while sprinting starts a roll.
    pub crouch_pressed: bool,
    /// Whether a reload is currently active; a roll cancels it deterministically.
    pub reload_active: bool,
}

/// Output applied by the engine adapter for one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnFootTickOutput {
    /// Resulting state.
    pub state: OnFootState,
    /// Wire stance.
    pub stance: M3RiderStance,
    /// Requested planar speed.
    pub speed_mmps: u32,
    /// Direction-locked requested velocity in millimetres/second.
    pub requested_velocity_mmps: [i32; 2],
    /// Remaining stamina ticks at the four-second consumption scale.
    pub stamina_ticks: u32,
    /// Whether firing is permitted this tick.
    pub can_fire: bool,
    /// Whether roll entry started a reload pause this tick. The future combat
    /// integration owns and checkpoints the retained reload progress.
    pub reload_pause_started: bool,
    /// Base stance sway multiplier in thousandths.
    pub sway_multiplier_milli: u16,
    /// Decaying roll-exit sway impulse in thousandths.
    pub roll_exit_sway_milli: u16,
}

/// Pure on-foot stance, stamina, roll, and input-buffer state machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnFootKernel {
    current_tick: Option<SimulationTick>,
    state: OnFootState,
    stamina_units: u32,
    previous_crouch_level: bool,
    buffered_roll_until: Option<SimulationTick>,
    roll_started_tick: Option<SimulationTick>,
    roll_direction: QuantizedDirection,
    roll_cooldown_until: SimulationTick,
    roll_exit_tick: Option<SimulationTick>,
    spook_stun_until: Option<SimulationTick>,
}

impl Default for OnFootKernel {
    fn default() -> Self {
        Self {
            current_tick: None,
            state: OnFootState::Standing,
            stamina_units: ON_FOOT_STAMINA_TICKS * ON_FOOT_STAMINA_REGEN_TICKS,
            previous_crouch_level: false,
            buffered_roll_until: None,
            roll_started_tick: None,
            roll_direction: QuantizedDirection::new(0, 0, -DIRECTION_UNITS),
            roll_cooldown_until: SimulationTick::new(0),
            roll_exit_tick: None,
            spook_stun_until: None,
        }
    }
}

impl OnFootKernel {
    /// Current on-foot state.
    #[must_use]
    pub const fn state(&self) -> OnFootState {
        self.state
    }

    /// Stamina exposed on the four-second consumption scale.
    #[must_use]
    pub const fn stamina_ticks(&self) -> u32 {
        self.stamina_units / ON_FOOT_STAMINA_REGEN_TICKS
    }

    /// Enters the locked no-input spook stun. The adapter applies the separate
    /// three-metre lateral displacement command exactly once.
    pub fn begin_spook_stun(&mut self, tick: SimulationTick) -> bool {
        if self.current_tick.is_some_and(|current| tick <= current)
            || self.state == OnFootState::SpookStunned
        {
            return false;
        }
        self.state = OnFootState::SpookStunned;
        self.spook_stun_until = Some(tick.saturating_add(SPOOK_STUN_TICKS));
        self.buffered_roll_until = None;
        self.roll_started_tick = None;
        true
    }

    /// Advances one strict tick.
    pub fn advance_tick(&mut self, input: OnFootTickInput) -> Result<OnFootTickOutput, M3Error> {
        if self
            .current_tick
            .is_some_and(|current| input.tick <= current)
        {
            return Err(M3Error::TickReplay);
        }
        self.current_tick = Some(input.tick);
        let crouch_edge = input.crouch_pressed && !self.previous_crouch_level;
        self.previous_crouch_level = input.crouch_pressed;
        let direction = input
            .move_direction
            .filter(|value| value.is_normalized() && value.y.abs() <= DIRECTION_UNITS / 100);

        if self.state == OnFootState::SpookStunned {
            let stun_until = self.spook_stun_until.ok_or(M3Error::InvalidState)?;
            if input.tick < stun_until {
                return Ok(OnFootTickOutput {
                    state: self.state,
                    stance: self.state.stance(),
                    speed_mmps: 0,
                    requested_velocity_mmps: [0, 0],
                    stamina_ticks: self.stamina_ticks(),
                    can_fire: false,
                    reload_pause_started: false,
                    sway_multiplier_milli: 1_500,
                    roll_exit_sway_milli: 0,
                });
            }
            self.state = OnFootState::Standing;
            self.spook_stun_until = None;
        }

        if crouch_edge {
            self.buffered_roll_until = Some(input.tick.saturating_add(ON_FOOT_INPUT_BUFFER_TICKS));
        }

        let mut reload_pause_started = false;
        if self.state == OnFootState::Rolling {
            let started = self.roll_started_tick.ok_or(M3Error::InvalidState)?;
            if input.tick.checked_duration_since(started).unwrap_or(0) >= TACTICAL_ROLL_TICKS {
                self.state = OnFootState::Standing;
                self.roll_started_tick = None;
                self.roll_exit_tick = Some(started.saturating_add(TACTICAL_ROLL_TICKS));
            }
        }

        if self.state != OnFootState::Rolling {
            let has_stamina = self.stamina_ticks() > 0;
            let wants_sprint = input.sprint_pressed && direction.is_some() && has_stamina;
            let buffer_live = self
                .buffered_roll_until
                .is_some_and(|deadline| input.tick <= deadline);
            if wants_sprint && buffer_live && input.tick >= self.roll_cooldown_until {
                self.state = OnFootState::Rolling;
                self.roll_started_tick = Some(input.tick);
                self.roll_direction = direction.expect("sprint requires a direction");
                self.roll_cooldown_until = input.tick.saturating_add(TACTICAL_ROLL_COOLDOWN_TICKS);
                self.buffered_roll_until = None;
                reload_pause_started = input.reload_active;
            } else if input.crouch_pressed {
                self.state = OnFootState::Crouched;
            } else if wants_sprint {
                self.state = OnFootState::Sprinting;
            } else {
                self.state = OnFootState::Standing;
            }
        }

        if self.state == OnFootState::Sprinting {
            self.stamina_units = self
                .stamina_units
                .saturating_sub(ON_FOOT_STAMINA_REGEN_TICKS);
        } else {
            self.stamina_units = self
                .stamina_units
                .saturating_add(ON_FOOT_STAMINA_TICKS)
                .min(ON_FOOT_STAMINA_TICKS * ON_FOOT_STAMINA_REGEN_TICKS);
        }

        let (speed_mmps, movement_direction, can_fire, sway_multiplier_milli) = match self.state {
            OnFootState::SpookStunned => (0, None, false, 1_500),
            OnFootState::Standing => (
                ON_FOOT_WALK_SPEED_MMPS,
                direction,
                true,
                if direction.is_some() { 1_200 } else { 900 },
            ),
            OnFootState::Sprinting => (ON_FOOT_SPRINT_SPEED_MMPS, direction, true, 1_500),
            OnFootState::Crouched => (ON_FOOT_CROUCH_SPEED_MMPS, direction, true, 800),
            OnFootState::Rolling => (
                TACTICAL_ROLL_SPEED_MMPS,
                Some(self.roll_direction),
                false,
                1_200,
            ),
        };
        let requested_velocity_mmps = movement_direction
            .map(|value| scale_planar_direction(value, speed_mmps))
            .unwrap_or([0, 0]);
        let roll_exit_sway_milli = self
            .roll_exit_tick
            .and_then(|exit| input.tick.checked_duration_since(exit))
            .map_or(0, roll_exit_sway);

        Ok(OnFootTickOutput {
            state: self.state,
            stance: self.state.stance(),
            speed_mmps,
            requested_velocity_mmps,
            stamina_ticks: self.stamina_ticks(),
            can_fire,
            reload_pause_started,
            sway_multiplier_milli,
            roll_exit_sway_milli,
        })
    }

    fn state_is_valid(&self) -> bool {
        if self.stamina_units > ON_FOOT_STAMINA_TICKS * ON_FOOT_STAMINA_REGEN_TICKS {
            return false;
        }
        (match self.state {
            OnFootState::SpookStunned => {
                self.roll_started_tick.is_none()
                    && self.spook_stun_until.is_some_and(|until| {
                        self.current_tick.is_none_or(|current| current < until)
                    })
            }
            OnFootState::Rolling => {
                self.spook_stun_until.is_none()
                    && self.roll_started_tick.is_some_and(|started| {
                        self.current_tick.is_none_or(|current| {
                            started <= current
                                && current.checked_duration_since(started).unwrap_or(0)
                                    < TACTICAL_ROLL_TICKS
                        })
                    })
                    && self.roll_direction.is_normalized()
            }
            OnFootState::Standing | OnFootState::Sprinting | OnFootState::Crouched => {
                self.spook_stun_until.is_none() && self.roll_started_tick.is_none()
            }
        }) && tick_is_not_after(self.roll_exit_tick, self.current_tick)
    }
}

fn scale_planar_direction(direction: QuantizedDirection, speed_mmps: u32) -> [i32; 2] {
    let speed = i64::from(speed_mmps);
    let scale = i64::from(DIRECTION_UNITS);
    [
        (i64::from(direction.x) * speed / scale).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
        (i64::from(direction.z) * speed / scale).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
    ]
}

fn roll_exit_sway(elapsed_ticks: u64) -> u16 {
    if elapsed_ticks >= ROLL_EXIT_SWAY_DECAY_TICKS {
        return 0;
    }
    let remaining = ROLL_EXIT_SWAY_DECAY_TICKS - elapsed_ticks;
    u16::try_from(u64::from(ROLL_EXIT_SWAY_IMPULSE_MILLI) * remaining / ROLL_EXIT_SWAY_DECAY_TICKS)
        .unwrap_or(0)
}

/// Monotonic authority identity for one recall-economy credit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RecallCreditId {
    /// Authority epoch producing the credit.
    pub authority_epoch: u64,
    /// Original gameplay tick.
    pub tick: SimulationTick,
    /// Authority-unique sequence within the tick.
    pub sequence: u64,
}

/// Locked M3 recall reduction source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallCreditKind {
    /// Authority-confirmed damage dealt; each complete 25 damage earns one second.
    DamageDealt(u16),
    /// One authority-confirmed objective tick earns two seconds.
    ObjectiveTick,
}

/// Replay-safe recall credit command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallCreditCommand {
    /// Strict monotonic replay identity.
    pub id: RecallCreditId,
    /// Credit source and amount.
    pub kind: RecallCreditKind,
}

/// Majestic Return presentation/state phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallState {
    /// Original horse is still available.
    HorsePresent,
    /// Horse is absent and the earned recall delay is running.
    CoolingDown,
    /// Recall may now be requested.
    Ready,
    /// Two-second hoofbeat telegraph.
    Hoofbeats,
    /// Dust/silhouette reveal.
    DustReveal,
    /// Three-second gallop-in.
    GallopIn,
    /// Final 1.5 seconds of gallop-in; running-mount window is active.
    MountWindow,
    /// Window expired; stationary remount remains possible.
    WaitingMount,
}

/// Recall timer output for HUD/presentation and acceptance telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecallTickOutput {
    /// Current phase.
    pub state: RecallState,
    /// Earliest tick at which recall can be requested.
    pub ready_tick: Option<SimulationTick>,
    /// Remaining delay before request eligibility.
    pub cooldown_remaining_ticks: u64,
    /// Whether this tick opened the running-mount window.
    pub mount_window_opened: bool,
}

/// M3 lose-horse-to-remount acceptance row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemountTelemetryRow {
    /// Fatal spook tick; acceptance timing includes the three-second bolt.
    pub horse_lost_tick: SimulationTick,
    /// Successful remount tick, if observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remount_tick: Option<SimulationTick>,
    /// Wall-clock-equivalent tick duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lose_horse_to_remount_ticks: Option<u64>,
    /// Whether the running-mount branch succeeded.
    pub running_mount: bool,
}

/// Deterministic recall economy, return phases, and mount window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallKernel {
    state: RecallState,
    current_tick: Option<SimulationTick>,
    horse_loss_tick: Option<SimulationTick>,
    lost_tick: Option<SimulationTick>,
    phase_enter_tick: Option<SimulationTick>,
    earned_reduction_ticks: u64,
    damage_remainder: u32,
    last_credit_id: Option<RecallCreditId>,
    telemetry: Option<RemountTelemetryRow>,
}

impl Default for RecallKernel {
    fn default() -> Self {
        Self {
            state: RecallState::HorsePresent,
            current_tick: None,
            horse_loss_tick: None,
            lost_tick: None,
            phase_enter_tick: None,
            earned_reduction_ticks: 0,
            damage_remainder: 0,
            last_credit_id: None,
            telemetry: None,
        }
    }
}

impl RecallKernel {
    /// Current phase.
    #[must_use]
    pub const fn state(&self) -> RecallState {
        self.state
    }

    /// Current acceptance row.
    #[must_use]
    pub const fn telemetry(&self) -> Option<RemountTelemetryRow> {
        self.telemetry
    }

    /// Starts recall timing when the spooked horse completes its bolt.
    pub fn lose_horse(
        &mut self,
        despawn_tick: SimulationTick,
        horse_loss_tick: SimulationTick,
    ) -> Result<(), M3Error> {
        if self.state != RecallState::HorsePresent || horse_loss_tick > despawn_tick {
            return Err(M3Error::InvalidState);
        }
        self.state = RecallState::CoolingDown;
        self.current_tick = Some(despawn_tick);
        self.horse_loss_tick = Some(horse_loss_tick);
        self.lost_tick = Some(despawn_tick);
        self.phase_enter_tick = Some(despawn_tick);
        self.earned_reduction_ticks = 0;
        self.damage_remainder = 0;
        self.last_credit_id = None;
        self.telemetry = Some(RemountTelemetryRow {
            horse_lost_tick: horse_loss_tick,
            remount_tick: None,
            lose_horse_to_remount_ticks: None,
            running_mount: false,
        });
        Ok(())
    }

    /// Applies one strictly newer authority credit. The monotonic watermark is
    /// compact, checkpointed, and rejects duplicates/migration replay.
    pub fn apply_credit(&mut self, command: RecallCreditCommand) -> bool {
        if !matches!(self.state, RecallState::CoolingDown | RecallState::Ready) {
            return false;
        }
        if self.last_credit_id.is_some_and(|last| command.id <= last)
            || self.lost_tick.is_none_or(|lost| command.id.tick < lost)
            || self
                .current_tick
                .is_none_or(|current| command.id.tick > current)
        {
            return false;
        }
        self.last_credit_id = Some(command.id);
        match command.kind {
            RecallCreditKind::DamageDealt(damage) => {
                let total = self.damage_remainder.saturating_add(u32::from(damage));
                let whole = total / 25;
                self.damage_remainder = total % 25;
                self.earned_reduction_ticks = self
                    .earned_reduction_ticks
                    .saturating_add(u64::from(whole) * M3_TICK_RATE_HZ as u64)
                    .min(BASE_RECALL_TICKS - MIN_RECALL_TICKS);
            }
            RecallCreditKind::ObjectiveTick => {
                self.earned_reduction_ticks = self
                    .earned_reduction_ticks
                    .saturating_add(2 * M3_TICK_RATE_HZ as u64)
                    .min(BASE_RECALL_TICKS - MIN_RECALL_TICKS);
            }
        }
        true
    }

    /// Earliest recall request tick after reductions and the hard floor.
    #[must_use]
    pub fn ready_tick(&self) -> Option<SimulationTick> {
        self.lost_tick.map(|lost| {
            lost.saturating_add(
                BASE_RECALL_TICKS
                    .saturating_sub(self.earned_reduction_ticks)
                    .max(MIN_RECALL_TICKS),
            )
        })
    }

    /// Advances phases using the absolute shared tick.
    pub fn advance_tick(&mut self, tick: SimulationTick) -> Result<RecallTickOutput, M3Error> {
        if self.current_tick.is_some_and(|current| tick <= current) {
            return Err(M3Error::TickReplay);
        }
        self.current_tick = Some(tick);
        let mut mount_window_opened = false;
        match self.state {
            RecallState::CoolingDown => {
                if self.ready_tick().is_some_and(|ready| tick >= ready) {
                    self.state = RecallState::Ready;
                    self.phase_enter_tick = self.ready_tick();
                }
            }
            RecallState::HorsePresent
            | RecallState::Ready
            | RecallState::Hoofbeats
            | RecallState::DustReveal
            | RecallState::GallopIn
            | RecallState::MountWindow
            | RecallState::WaitingMount => {}
        }
        while let Some(entered) = self.phase_enter_tick {
            let transition = match self.state {
                RecallState::Hoofbeats => Some((HOOFBEAT_TICKS, RecallState::DustReveal)),
                RecallState::DustReveal => Some((DUST_REVEAL_TICKS, RecallState::GallopIn)),
                RecallState::GallopIn => Some((
                    GALLOP_IN_TICKS.saturating_sub(RUNNING_MOUNT_WINDOW_TICKS),
                    RecallState::MountWindow,
                )),
                RecallState::MountWindow => {
                    Some((RUNNING_MOUNT_WINDOW_TICKS, RecallState::WaitingMount))
                }
                RecallState::HorsePresent
                | RecallState::CoolingDown
                | RecallState::Ready
                | RecallState::WaitingMount => None,
            };
            let Some((duration, next)) = transition else {
                break;
            };
            let boundary = entered.saturating_add(duration);
            if tick < boundary {
                break;
            }
            self.state = next;
            self.phase_enter_tick = Some(boundary);
            if next == RecallState::MountWindow {
                mount_window_opened = true;
            }
        }
        let ready_tick = self.ready_tick();
        let cooldown_remaining_ticks = if self.state == RecallState::CoolingDown {
            ready_tick
                .and_then(|ready| ready.checked_duration_since(tick))
                .unwrap_or(0)
        } else {
            0
        };
        Ok(RecallTickOutput {
            state: self.state,
            ready_tick,
            cooldown_remaining_ticks,
            mount_window_opened,
        })
    }

    /// Starts the locked return sequence after the recall delay.
    pub fn request_recall(&mut self, tick: SimulationTick) -> bool {
        if self.state != RecallState::Ready || self.current_tick != Some(tick) {
            return false;
        }
        self.state = RecallState::Hoofbeats;
        self.phase_enter_tick = Some(tick);
        true
    }

    /// Checks range/window and records one successful mount.
    pub fn try_mount(
        &mut self,
        tick: SimulationTick,
        rider_position: QuantizedOrigin,
        horse_position: QuantizedOrigin,
        horse_moving: bool,
    ) -> bool {
        if self.current_tick != Some(tick) {
            return false;
        }
        let (range_mm, running_mount) = match self.state {
            RecallState::MountWindow if horse_moving => (RUNNING_MOUNT_RANGE_MM, true),
            RecallState::WaitingMount if !horse_moving => (STATIONARY_REMOUNT_RANGE_MM, false),
            _ => return false,
        };
        if rider_position.squared_distance_mm(horse_position) > u128::from(range_mm).pow(2) {
            return false;
        }
        let Some(mut row) = self.telemetry else {
            return false;
        };
        row.remount_tick = Some(tick);
        row.lose_horse_to_remount_ticks = tick.checked_duration_since(row.horse_lost_tick);
        row.running_mount = running_mount;
        self.telemetry = Some(row);
        self.state = RecallState::HorsePresent;
        self.phase_enter_tick = Some(tick);
        true
    }

    fn state_is_valid(&self) -> bool {
        if self.earned_reduction_ticks > BASE_RECALL_TICKS - MIN_RECALL_TICKS
            || self.damage_remainder >= 25
            || self.last_credit_id.is_some_and(|id| {
                self.lost_tick.is_none_or(|lost| id.tick < lost)
                    || self.current_tick.is_none_or(|current| id.tick > current)
            })
        {
            return false;
        }
        match self.state {
            RecallState::HorsePresent => match self.telemetry {
                None => {
                    self.horse_loss_tick.is_none()
                        && self.lost_tick.is_none()
                        && self.phase_enter_tick.is_none()
                        && self.earned_reduction_ticks == 0
                        && self.damage_remainder == 0
                        && self.last_credit_id.is_none()
                }
                Some(row) => {
                    Some(row.horse_lost_tick) == self.horse_loss_tick
                        && self.lost_tick.is_some()
                        && self.phase_enter_tick == row.remount_tick
                        && tick_is_not_after(self.lost_tick, row.remount_tick)
                        && row.lose_horse_to_remount_ticks
                            == row
                                .remount_tick
                                .and_then(|tick| tick.checked_duration_since(row.horse_lost_tick))
                }
            },
            RecallState::CoolingDown
            | RecallState::Ready
            | RecallState::Hoofbeats
            | RecallState::DustReveal
            | RecallState::GallopIn
            | RecallState::MountWindow
            | RecallState::WaitingMount => {
                self.horse_loss_tick.is_some()
                    && self.lost_tick.is_some()
                    && self.phase_enter_tick.is_some()
                    && tick_is_not_after(self.horse_loss_tick, self.lost_tick)
                    && tick_is_not_after(self.lost_tick, self.phase_enter_tick)
                    && tick_is_not_after(self.phase_enter_tick, self.current_tick)
                    && self.telemetry.is_some_and(|row| {
                        Some(row.horse_lost_tick) == self.horse_loss_tick
                            && row.remount_tick.is_none()
                            && row.lose_horse_to_remount_ticks.is_none()
                    })
            }
        }
    }
}

fn tick_is_not_after(earlier: Option<SimulationTick>, later: Option<SimulationTick>) -> bool {
    match (earlier, later) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(earlier), Some(later)) => earlier <= later,
    }
}

/// M3 deterministic ordering/state rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum M3Error {
    /// Absolute tick repeated or regressed.
    #[error("tick_replay")]
    TickReplay,
    /// State data was internally inconsistent or an operation was out of phase.
    #[error("invalid_state")]
    InvalidState,
}

/// High-level M3 actor mode derived from the composed native kernels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorM3Mode {
    /// Horse is alive and locally controllable/retrievable.
    Mounted,
    /// Rider is disabled by the fatal spook throw.
    SpookStunned,
    /// Rider has the normal on-foot kit while the horse is absent/bolting.
    OnFoot,
    /// Majestic Return presentation is in progress.
    ReturningHorse,
}

/// One composed actor tick. It deliberately excludes M0/M2 movement fields;
/// wire v2 integration will combine this with the existing actor input atomically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorM3TickInput {
    /// Strictly increasing shared authority tick.
    pub tick: SimulationTick,
    /// On-foot input sampled for this tick.
    pub on_foot: OnFootTickInput,
    /// Recall/return interaction edge.
    pub interact_pressed: bool,
    /// Collision-resolved rider position for mount range validation.
    pub rider_position: QuantizedOrigin,
    /// Collision-resolved return-horse position.
    pub return_horse_position: QuantizedOrigin,
    /// Whether the returning horse is still moving through the running-mount window.
    pub return_horse_moving: bool,
}

/// Composed actor output consumed by the future native authority bank/adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorM3TickOutput {
    /// Derived actor mode.
    pub mode: ActorM3Mode,
    /// On-foot output while the rider is detached.
    pub on_foot: Option<OnFootTickOutput>,
    /// Recall phase output once the original horse has despawned.
    pub recall: Option<RecallTickOutput>,
    /// True on the exact three-second bolt-to-despawn boundary.
    pub horse_despawned: bool,
    /// True on the exact successful running/stationary remount tick.
    pub remounted: bool,
}

/// Serializable native authority state required to migrate during a bolt,
/// spook stun, roll, recall cooldown, or return sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActorGameplayKernel {
    current_tick: Option<SimulationTick>,
    horse: HorseVitalityKernel,
    on_foot: OnFootKernel,
    recall: RecallKernel,
    horse_loss_tick: Option<SimulationTick>,
    pending_horse_loss_effects: Option<HorseDamageEffects>,
}

/// Validated wire-v2 migration checkpoint. Deserialized checkpoints must pass
/// [`ActorGameplayKernel::restore_checkpoint`] before use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActorGameplayCheckpointV2 {
    /// Exact major-version boundary for the checkpoint canonical form.
    wire_version: WireVersion,
    /// Complete actor authority state, including replay receipts and timers.
    actor: ActorGameplayKernel,
}

#[derive(Deserialize)]
struct RawActorGameplayCheckpointV2 {
    wire_version: WireVersion,
    actor: RawActorGameplayKernel,
}

#[derive(Deserialize)]
struct RawActorGameplayKernel {
    current_tick: Option<SimulationTick>,
    horse: HorseVitalityKernel,
    on_foot: OnFootKernel,
    recall: RecallKernel,
    horse_loss_tick: Option<SimulationTick>,
    pending_horse_loss_effects: Option<HorseDamageEffects>,
}

impl<'de> Deserialize<'de> for ActorGameplayCheckpointV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawActorGameplayCheckpointV2::deserialize(deserializer)?;
        let checkpoint = Self {
            wire_version: raw.wire_version,
            actor: ActorGameplayKernel {
                current_tick: raw.actor.current_tick,
                horse: raw.actor.horse,
                on_foot: raw.actor.on_foot,
                recall: raw.actor.recall,
                horse_loss_tick: raw.actor.horse_loss_tick,
                pending_horse_loss_effects: raw.actor.pending_horse_loss_effects,
            },
        };
        ActorGameplayKernel::restore_checkpoint(checkpoint.clone())
            .map_err(serde::de::Error::custom)?;
        Ok(checkpoint)
    }
}

/// Fail-closed checkpoint rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum M3CheckpointError {
    /// Checkpoint is not exactly the M3 wire-major schema.
    #[error("wire_version_mismatch")]
    WireVersionMismatch,
    /// Cross-kernel state or a bounded field is inconsistent.
    #[error("invalid_checkpoint_state")]
    InvalidState,
}

impl ActorGameplayKernel {
    /// Builds one actor from the match-start horse selection.
    #[must_use]
    pub fn new(class: HorseVitalityClass) -> Self {
        Self {
            current_tick: None,
            horse: HorseVitalityKernel::new(class),
            on_foot: OnFootKernel::default(),
            recall: RecallKernel::default(),
            horse_loss_tick: None,
            pending_horse_loss_effects: None,
        }
    }

    /// Current horse kernel for snapshots/HUD.
    #[must_use]
    pub const fn horse(&self) -> &HorseVitalityKernel {
        &self.horse
    }

    /// Current on-foot kernel for snapshots/HUD.
    #[must_use]
    pub const fn on_foot(&self) -> &OnFootKernel {
        &self.on_foot
    }

    /// Current recall kernel for snapshots/HUD.
    #[must_use]
    pub const fn recall(&self) -> &RecallKernel {
        &self.recall
    }

    /// Idempotent fatal effects awaiting native adapter/network delivery.
    #[must_use]
    pub const fn pending_horse_loss_effects(&self) -> Option<HorseDamageEffects> {
        self.pending_horse_loss_effects
    }

    /// Acknowledges the exact delivered fatal event. A mismatched or repeated
    /// acknowledgement cannot clear a newer event.
    pub fn acknowledge_horse_loss_effects(&mut self, id: HorseDamageId) -> bool {
        let Some(event) = self
            .pending_horse_loss_effects
            .and_then(|effects| effects.horse_bolted)
        else {
            return false;
        };
        if event.id != id {
            return false;
        }
        self.pending_horse_loss_effects = None;
        true
    }

    /// Captures the complete native authority state for hash/sign/replication.
    #[must_use]
    pub fn checkpoint(&self) -> ActorGameplayCheckpointV2 {
        ActorGameplayCheckpointV2 {
            wire_version: M3_WIRE_VERSION,
            actor: self.clone(),
        }
    }

    /// Validates every private kernel invariant before accepting migrated state.
    pub fn restore_checkpoint(
        checkpoint: ActorGameplayCheckpointV2,
    ) -> Result<Self, M3CheckpointError> {
        if checkpoint.wire_version != M3_WIRE_VERSION {
            return Err(M3CheckpointError::WireVersionMismatch);
        }
        let actor = checkpoint.actor;
        if !actor.horse.state_is_valid()
            || !actor.on_foot.state_is_valid()
            || !actor.recall.state_is_valid()
            || !tick_is_not_after(actor.horse.current_tick, actor.current_tick)
            || !tick_is_not_after(actor.on_foot.current_tick, actor.current_tick)
            || !tick_is_not_after(actor.recall.current_tick, actor.current_tick)
        {
            return Err(M3CheckpointError::InvalidState);
        }
        let cross_kernel_valid = match actor.horse.state() {
            HorseVitalityState::Available => {
                actor.recall.state() == RecallState::HorsePresent && actor.horse_loss_tick.is_none()
            }
            HorseVitalityState::Bolting => {
                actor.recall.state() == RecallState::HorsePresent
                    && actor.horse_loss_tick == actor.horse.bolt_started_tick
            }
            HorseVitalityState::Despawned => {
                actor.recall.state() != RecallState::HorsePresent
                    && actor.horse_loss_tick == actor.recall.horse_loss_tick
            }
        };
        let pending_valid = actor.pending_horse_loss_effects.is_none_or(|effects| {
            effects.horse_bolted.is_some_and(|event| {
                effects.application.health_before > 0
                    && effects.application.health_after == 0
                    && effects.application.spooked
                    && effects.application.bolt_away_delta_mm == actor.horse.bolt_away_delta_mm
                    && effects.application.rider_throw_distance_mm == SPOOK_THROW_DISTANCE_MM
                    && effects.application.rider_stun_ticks == SPOOK_STUN_TICKS
                    && !effects.application.rider_fall_damage
                    && Some(event.id) == actor.horse.last_damage_id
                    && event.class == actor.horse.class
                    && event.bolt_away_delta_mm == actor.horse.bolt_away_delta_mm
                    && event.notification_points == 15
                    && Some(event.id.tick) == actor.recall.horse_loss_tick.or(actor.horse_loss_tick)
            })
        });
        if !cross_kernel_valid || !pending_valid {
            return Err(M3CheckpointError::InvalidState);
        }
        Ok(actor)
    }

    /// Derived high-level mode; no parallel engine boolean owns this state.
    #[must_use]
    pub fn mode(&self) -> ActorM3Mode {
        if self.horse.state() == HorseVitalityState::Available {
            ActorM3Mode::Mounted
        } else if self.on_foot.state() == OnFootState::SpookStunned {
            ActorM3Mode::SpookStunned
        } else if matches!(
            self.recall.state(),
            RecallState::Hoofbeats
                | RecallState::DustReveal
                | RecallState::GallopIn
                | RecallState::MountWindow
                | RecallState::WaitingMount
        ) {
            ActorM3Mode::ReturningHorse
        } else {
            ActorM3Mode::OnFoot
        }
    }

    /// Applies one replay-safe authority horse-damage command. Fatal damage
    /// atomically begins the rider stun and returns one `HorseBolted` event.
    pub fn apply_horse_damage(
        &mut self,
        command: HorseDamageCommand,
    ) -> Option<HorseDamageEffects> {
        if self
            .current_tick
            .is_some_and(|current| command.id.tick != current)
        {
            return None;
        }
        let mut horse = self.horse.clone();
        let mut on_foot = self.on_foot.clone();
        let application = horse.apply_damage(command)?;
        let horse_bolted = if application.spooked {
            if !on_foot.begin_spook_stun(command.id.tick) {
                return None;
            }
            Some(HorseBoltedEvent {
                id: command.id,
                class: horse.class,
                bolt_away_delta_mm: application.bolt_away_delta_mm,
                notification_points: 15,
            })
        } else {
            None
        };
        let effects = HorseDamageEffects {
            application,
            horse_bolted,
        };
        self.horse = horse;
        self.on_foot = on_foot;
        if application.spooked {
            self.horse_loss_tick = Some(command.id.tick);
            self.pending_horse_loss_effects = Some(effects);
        }
        Some(effects)
    }

    /// Applies one replay-safe recall credit without coupling it to score mutation.
    pub fn apply_recall_credit(&mut self, command: RecallCreditCommand) -> bool {
        self.recall.apply_credit(command)
    }

    /// Advances horse, rider, recall, and remount state on one shared tick.
    pub fn advance_tick(&mut self, input: ActorM3TickInput) -> Result<ActorM3TickOutput, M3Error> {
        if input.on_foot.tick != input.tick
            || self
                .current_tick
                .is_some_and(|current| input.tick <= current)
        {
            return Err(M3Error::TickReplay);
        }
        self.current_tick = Some(input.tick);

        let horse_despawn_tick = self.horse.advance_tick(input.tick)?;
        if let Some(despawn_tick) = horse_despawn_tick {
            self.recall.lose_horse(
                despawn_tick,
                self.horse_loss_tick.ok_or(M3Error::InvalidState)?,
            )?;
        }

        let on_foot = (self.horse.state() != HorseVitalityState::Available)
            .then(|| self.on_foot.advance_tick(input.on_foot))
            .transpose()?;

        let mut recall = if let Some(despawn_tick) = horse_despawn_tick {
            if input.tick > despawn_tick {
                Some(self.recall.advance_tick(input.tick)?)
            } else {
                Some(RecallTickOutput {
                    state: self.recall.state(),
                    ready_tick: self.recall.ready_tick(),
                    cooldown_remaining_ticks: self
                        .recall
                        .ready_tick()
                        .and_then(|ready| ready.checked_duration_since(input.tick))
                        .unwrap_or(0),
                    mount_window_opened: false,
                })
            }
        } else if self.horse.state() == HorseVitalityState::Despawned {
            let output = self.recall.advance_tick(input.tick)?;
            if input.interact_pressed && output.state == RecallState::Ready {
                self.recall.request_recall(input.tick);
            }
            Some(RecallTickOutput {
                state: self.recall.state(),
                ..output
            })
        } else {
            None
        };
        if input.interact_pressed && self.recall.state() == RecallState::Ready {
            self.recall.request_recall(input.tick);
            if let Some(output) = recall.as_mut() {
                output.state = self.recall.state();
            }
        }

        let remounted = if matches!(
            self.recall.state(),
            RecallState::MountWindow | RecallState::WaitingMount
        ) && input.interact_pressed
            && self.recall.try_mount(
                input.tick,
                input.rider_position,
                input.return_horse_position,
                input.return_horse_moving,
            ) {
            let restored = self.horse.restore_returned_horse();
            debug_assert!(restored, "mount completion must restore a despawned horse");
            self.on_foot = OnFootKernel::default();
            self.horse_loss_tick = None;
            true
        } else {
            false
        };

        if remounted {
            recall = None;
        }
        Ok(ActorM3TickOutput {
            mode: self.mode(),
            on_foot: if remounted { None } else { on_foot },
            recall,
            horse_despawned: horse_despawn_tick.is_some(),
            remounted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward() -> QuantizedDirection {
        QuantizedDirection::new(0, 0, -DIRECTION_UNITS)
    }

    fn actor_input(tick: u64, interact_pressed: bool) -> ActorM3TickInput {
        let tick = SimulationTick::new(tick);
        ActorM3TickInput {
            tick,
            on_foot: OnFootTickInput {
                tick,
                move_direction: None,
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            },
            interact_pressed,
            rider_position: QuantizedOrigin::default(),
            return_horse_position: QuantizedOrigin::new(3_999, 0, 0),
            return_horse_moving: true,
        }
    }

    fn recall_credit(tick: u64, sequence: u64, kind: RecallCreditKind) -> RecallCreditCommand {
        RecallCreditCommand {
            id: RecallCreditId {
                authority_epoch: 1,
                tick: SimulationTick::new(tick),
                sequence,
            },
            kind,
        }
    }

    #[test]
    fn horse_rows_spook_once_bolt_away_and_despawn_at_three_seconds() {
        for (class, expected) in [
            (HorseVitalityClass::Courser, 200),
            (HorseVitalityClass::Warhorse, 320),
            (HorseVitalityClass::Mustang, 250),
        ] {
            let mut horse = HorseVitalityKernel::new(class);
            assert_eq!(horse.health(), expected);
            let command = HorseDamageCommand {
                id: HorseDamageId {
                    authority_epoch: 4,
                    tick: SimulationTick::new(10),
                    sequence: 1,
                },
                amount: expected,
                horse_position: QuantizedOrigin::new(10_000, 0, 5_000),
                damage_source_position: QuantizedOrigin::new(12_000, 0, 4_000),
            };
            let result = horse.apply_damage(command).unwrap();
            assert!(result.spooked);
            assert_eq!(result.bolt_away_delta_mm, [-2_000, 1_000]);
            assert_eq!(result.rider_throw_distance_mm, 3_000);
            assert_eq!(result.rider_stun_ticks, 36);
            assert!(!result.rider_fall_damage);
            assert_eq!(horse.state(), HorseVitalityState::Bolting);
            assert!(horse.apply_damage(command).is_none());
            assert_eq!(horse.advance_tick(SimulationTick::new(189)).unwrap(), None);
            assert_eq!(horse.state(), HorseVitalityState::Bolting);
            assert_eq!(
                horse.advance_tick(SimulationTick::new(190)).unwrap(),
                Some(SimulationTick::new(190))
            );
            assert_eq!(horse.state(), HorseVitalityState::Despawned);
            assert!(horse.restore_returned_horse());
            assert_eq!(horse.health(), expected);
        }
    }

    #[test]
    fn horse_regen_waits_six_seconds_then_restores_ten_health_per_second() {
        let mut horse = HorseVitalityKernel::new(HorseVitalityClass::Mustang);
        horse
            .apply_damage(HorseDamageCommand {
                id: HorseDamageId {
                    authority_epoch: 1,
                    tick: SimulationTick::new(10),
                    sequence: 1,
                },
                amount: 50,
                horse_position: QuantizedOrigin::default(),
                damage_source_position: QuantizedOrigin::new(1, 0, 0),
            })
            .unwrap();
        horse.advance_tick(SimulationTick::new(369)).unwrap();
        assert_eq!(horse.health(), 200);
        horse.advance_tick(SimulationTick::new(375)).unwrap();
        assert_eq!(horse.health(), 201);
        horse.advance_tick(SimulationTick::new(669)).unwrap();
        assert_eq!(horse.health(), 250);
    }

    #[test]
    fn damage_watermark_is_bounded_and_fatal_actor_application_is_transactional() {
        let mut horse = HorseVitalityKernel::new(HorseVitalityClass::Warhorse);
        horse.advance_tick(SimulationTick::new(10)).unwrap();
        for sequence in 1..=100 {
            assert!(horse
                .apply_damage(HorseDamageCommand {
                    id: HorseDamageId {
                        authority_epoch: 1,
                        tick: SimulationTick::new(10),
                        sequence,
                    },
                    amount: 1,
                    horse_position: QuantizedOrigin::default(),
                    damage_source_position: QuantizedOrigin::new(1, 0, 0),
                })
                .is_some());
        }
        assert_eq!(horse.last_damage_id.unwrap().sequence, 100);

        let mut actor = ActorGameplayKernel::new(HorseVitalityClass::Courser);
        actor.advance_tick(actor_input(1, false)).unwrap();
        let before = actor.clone();
        for rejected_tick in [0, 2] {
            assert!(actor
                .apply_horse_damage(HorseDamageCommand {
                    id: HorseDamageId {
                        authority_epoch: 1,
                        tick: SimulationTick::new(rejected_tick),
                        sequence: 1,
                    },
                    amount: 200,
                    horse_position: QuantizedOrigin::default(),
                    damage_source_position: QuantizedOrigin::new(1, 0, 0),
                })
                .is_none());
            assert_eq!(actor, before);
        }
        assert!(
            actor
                .apply_horse_damage(HorseDamageCommand {
                    id: HorseDamageId {
                        authority_epoch: 1,
                        tick: SimulationTick::new(1),
                        sequence: 1,
                    },
                    amount: 200,
                    horse_position: QuantizedOrigin::default(),
                    damage_source_position: QuantizedOrigin::new(1, 0, 0),
                })
                .unwrap()
                .application
                .spooked
        );
    }

    #[test]
    fn sprint_consumes_four_seconds_and_regenerates_in_six() {
        let mut kernel = OnFootKernel::default();
        for tick in 1..=ON_FOOT_STAMINA_TICKS {
            let output = kernel
                .advance_tick(OnFootTickInput {
                    tick: SimulationTick::new(u64::from(tick)),
                    move_direction: Some(forward()),
                    sprint_pressed: true,
                    crouch_pressed: false,
                    reload_active: false,
                })
                .unwrap();
            assert_eq!(output.state, OnFootState::Sprinting);
        }
        assert_eq!(kernel.stamina_ticks(), 0);
        let exhausted = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(241),
                move_direction: Some(forward()),
                sprint_pressed: true,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        assert_eq!(exhausted.state, OnFootState::Standing);
        for tick in 242..=600 {
            kernel
                .advance_tick(OnFootTickInput {
                    tick: SimulationTick::new(tick),
                    move_direction: None,
                    sprint_pressed: false,
                    crouch_pressed: false,
                    reload_active: false,
                })
                .unwrap();
        }
        assert_eq!(kernel.stamina_ticks(), ON_FOOT_STAMINA_TICKS);
    }

    #[test]
    fn roll_is_buffered_locked_no_fire_and_cancels_reload() {
        let mut kernel = OnFootKernel::default();
        let crouch = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(1),
                move_direction: Some(forward()),
                sprint_pressed: false,
                crouch_pressed: true,
                reload_active: true,
            })
            .unwrap();
        assert_eq!(crouch.state, OnFootState::Crouched);
        let roll = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(2),
                move_direction: Some(forward()),
                sprint_pressed: true,
                crouch_pressed: false,
                reload_active: true,
            })
            .unwrap();
        assert_eq!(roll.state, OnFootState::Rolling);
        assert!(!roll.can_fire);
        assert!(roll.reload_pause_started);
        assert_eq!(roll.requested_velocity_mmps, [0, -7_000]);
        for tick in 3..32 {
            let output = kernel
                .advance_tick(OnFootTickInput {
                    tick: SimulationTick::new(tick),
                    move_direction: Some(QuantizedDirection::new(DIRECTION_UNITS, 0, 0)),
                    sprint_pressed: false,
                    crouch_pressed: false,
                    reload_active: false,
                })
                .unwrap();
            assert_eq!(output.state, OnFootState::Rolling);
            assert_eq!(output.requested_velocity_mmps, [0, -7_000]);
        }
        let exit = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(32),
                move_direction: None,
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        assert_eq!(exit.state, OnFootState::Standing);
        assert_eq!(exit.roll_exit_sway_milli, ROLL_EXIT_SWAY_IMPULSE_MILLI);
    }

    #[test]
    fn moving_standing_sway_uses_the_locked_one_point_two_multiplier() {
        let mut kernel = OnFootKernel::default();
        let output = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(1),
                move_direction: Some(forward()),
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        assert_eq!(output.state, OnFootState::Standing);
        assert_eq!(output.sway_multiplier_milli, 1_200);
    }

    #[test]
    fn spook_stun_blocks_input_for_exactly_point_six_seconds() {
        let mut kernel = OnFootKernel::default();
        assert!(kernel.begin_spook_stun(SimulationTick::new(10)));
        for tick in 10..46 {
            let output = kernel
                .advance_tick(OnFootTickInput {
                    tick: SimulationTick::new(tick),
                    move_direction: Some(forward()),
                    sprint_pressed: true,
                    crouch_pressed: true,
                    reload_active: true,
                })
                .unwrap();
            assert_eq!(output.state, OnFootState::SpookStunned);
            assert_eq!(output.requested_velocity_mmps, [0, 0]);
            assert!(!output.can_fire);
        }
        let released = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(46),
                move_direction: None,
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        assert_eq!(released.state, OnFootState::Standing);
    }

    #[test]
    fn migration_serialization_retains_damage_receipts_and_active_timers() {
        let mut horse = HorseVitalityKernel::new(HorseVitalityClass::Warhorse);
        let command = HorseDamageCommand {
            id: HorseDamageId {
                authority_epoch: 9,
                tick: SimulationTick::new(40),
                sequence: 2,
            },
            amount: 320,
            horse_position: QuantizedOrigin::new(5_000, 0, 0),
            damage_source_position: QuantizedOrigin::default(),
        };
        horse.apply_damage(command).unwrap();
        horse.advance_tick(SimulationTick::new(100)).unwrap();
        let encoded = serde_json::to_vec(&horse).unwrap();
        let mut restored: HorseVitalityKernel = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, horse);
        assert!(restored.apply_damage(command).is_none());
        assert_eq!(
            restored.advance_tick(SimulationTick::new(220)).unwrap(),
            Some(SimulationTick::new(220))
        );

        let mut on_foot = OnFootKernel::default();
        on_foot.begin_spook_stun(SimulationTick::new(30));
        on_foot
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(31),
                move_direction: None,
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        let restored: OnFootKernel =
            serde_json::from_slice(&serde_json::to_vec(&on_foot).unwrap()).unwrap();
        assert_eq!(restored, on_foot);
    }

    #[test]
    fn composed_actor_spooks_logs_once_survives_migration_and_remounts() {
        assert_eq!(M3_WIRE_VERSION, WireVersion::new(2, 0));
        let mut actor = ActorGameplayKernel::new(HorseVitalityClass::Courser);
        let fatal = HorseDamageCommand {
            id: HorseDamageId {
                authority_epoch: 2,
                tick: SimulationTick::new(1),
                sequence: 7,
            },
            amount: 200,
            horse_position: QuantizedOrigin::new(8_000, 0, 0),
            damage_source_position: QuantizedOrigin::default(),
        };
        let effects = actor.apply_horse_damage(fatal).unwrap();
        assert_eq!(effects.horse_bolted.unwrap().notification_points, 15);
        assert_eq!(actor.pending_horse_loss_effects(), Some(effects));
        assert!(actor.apply_horse_damage(fatal).is_none());
        let stunned = actor.advance_tick(actor_input(1, false)).unwrap();
        assert_eq!(stunned.mode, ActorM3Mode::SpookStunned);

        let encoded = serde_json::to_vec(&actor.checkpoint()).unwrap();
        let checkpoint: ActorGameplayCheckpointV2 = serde_json::from_slice(&encoded).unwrap();
        let mut actor = ActorGameplayKernel::restore_checkpoint(checkpoint).unwrap();
        assert_eq!(actor.pending_horse_loss_effects(), Some(effects));
        assert!(!actor.acknowledge_horse_loss_effects(HorseDamageId {
            sequence: 8,
            ..fatal.id
        }));
        assert!(actor.acknowledge_horse_loss_effects(fatal.id));
        assert!(!actor.acknowledge_horse_loss_effects(fatal.id));
        assert!(actor.apply_horse_damage(fatal).is_none());
        let despawned = actor.advance_tick(actor_input(181, false)).unwrap();
        assert!(despawned.horse_despawned);
        assert_eq!(despawned.mode, ActorM3Mode::OnFoot);

        for sequence in 1..=6 {
            assert!(actor.apply_recall_credit(recall_credit(
                181,
                sequence,
                RecallCreditKind::ObjectiveTick,
            )));
        }
        actor.advance_tick(actor_input(661, true)).unwrap();
        assert_eq!(actor.recall().state(), RecallState::Hoofbeats);
        actor.advance_tick(actor_input(781, false)).unwrap();
        actor.advance_tick(actor_input(871, false)).unwrap();
        let mounted = actor.advance_tick(actor_input(961, true)).unwrap();
        assert!(mounted.remounted);
        assert_eq!(mounted.mode, ActorM3Mode::Mounted);
        assert!(mounted.on_foot.is_none());
        assert!(mounted.recall.is_none());
        assert_eq!(actor.horse().health(), 200);
        assert!(actor.recall().telemetry().unwrap().running_mount);
    }

    #[test]
    fn recall_credit_watermark_survives_checkpoint_and_rejects_replay() {
        let mut actor = ActorGameplayKernel::new(HorseVitalityClass::Courser);
        actor
            .apply_horse_damage(HorseDamageCommand {
                id: HorseDamageId {
                    authority_epoch: 1,
                    tick: SimulationTick::new(1),
                    sequence: 1,
                },
                amount: 200,
                horse_position: QuantizedOrigin::default(),
                damage_source_position: QuantizedOrigin::new(1, 0, 0),
            })
            .unwrap();
        actor.advance_tick(actor_input(181, false)).unwrap();
        let accepted = recall_credit(181, 5, RecallCreditKind::ObjectiveTick);
        assert!(actor.apply_recall_credit(accepted));

        let encoded = serde_json::to_vec(&actor.checkpoint()).unwrap();
        let checkpoint: ActorGameplayCheckpointV2 = serde_json::from_slice(&encoded).unwrap();
        let mut restored = ActorGameplayKernel::restore_checkpoint(checkpoint).unwrap();
        assert!(!restored.apply_recall_credit(accepted));
        assert!(!restored.apply_recall_credit(recall_credit(
            181,
            4,
            RecallCreditKind::ObjectiveTick,
        )));
        assert!(restored.apply_recall_credit(recall_credit(
            181,
            6,
            RecallCreditKind::ObjectiveTick,
        )));
        assert!(!restored.apply_recall_credit(recall_credit(
            182,
            7,
            RecallCreditKind::ObjectiveTick,
        )));
    }

    #[test]
    fn absolute_tick_jumps_cross_bolt_roll_and_return_boundaries_without_delay() {
        let mut actor = ActorGameplayKernel::new(HorseVitalityClass::Courser);
        actor
            .apply_horse_damage(HorseDamageCommand {
                id: HorseDamageId {
                    authority_epoch: 1,
                    tick: SimulationTick::new(1),
                    sequence: 1,
                },
                amount: 200,
                horse_position: QuantizedOrigin::default(),
                damage_source_position: QuantizedOrigin::new(1, 0, 0),
            })
            .unwrap();
        let jumped = actor.advance_tick(actor_input(1_400, true)).unwrap();
        assert!(jumped.horse_despawned);
        assert_eq!(
            actor.recall().telemetry().unwrap().horse_lost_tick,
            SimulationTick::new(1)
        );
        assert_eq!(actor.recall().state(), RecallState::Hoofbeats);

        let mut on_foot = OnFootKernel::default();
        on_foot
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(1),
                move_direction: Some(forward()),
                sprint_pressed: true,
                crouch_pressed: true,
                reload_active: false,
            })
            .unwrap();
        let after_decay = on_foot
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(100),
                move_direction: None,
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        assert_eq!(after_decay.state, OnFootState::Standing);
        assert_eq!(after_decay.roll_exit_sway_milli, 0);

        let mut recall = RecallKernel::default();
        recall
            .lose_horse(SimulationTick::new(0), SimulationTick::new(0))
            .unwrap();
        recall.advance_tick(SimulationTick::new(1_200)).unwrap();
        assert!(recall.request_recall(SimulationTick::new(1_200)));
        let crossed = recall.advance_tick(SimulationTick::new(1_500)).unwrap();
        assert_eq!(crossed.state, RecallState::MountWindow);
        assert!(crossed.mount_window_opened);
    }

    #[test]
    fn m3_checkpoint_rejects_wrong_wire_and_inconsistent_private_state() {
        let actor = ActorGameplayKernel::new(HorseVitalityClass::Mustang);
        let mut wrong_wire = actor.checkpoint();
        wrong_wire.wire_version = WireVersion::new(1, 2);
        assert_eq!(
            ActorGameplayKernel::restore_checkpoint(wrong_wire),
            Err(M3CheckpointError::WireVersionMismatch)
        );

        let mut impossible_health = actor.checkpoint();
        impossible_health.actor.horse.health = 251;
        assert_eq!(
            ActorGameplayKernel::restore_checkpoint(impossible_health),
            Err(M3CheckpointError::InvalidState)
        );

        let mut impossible_recall = actor.checkpoint();
        impossible_recall.actor.recall.state = RecallState::Ready;
        assert_eq!(
            ActorGameplayKernel::restore_checkpoint(impossible_recall),
            Err(M3CheckpointError::InvalidState)
        );

        let mut impossible_timer = actor.checkpoint();
        impossible_timer.actor.on_foot.roll_exit_tick = Some(SimulationTick::new(1));
        assert_eq!(
            ActorGameplayKernel::restore_checkpoint(impossible_timer),
            Err(M3CheckpointError::InvalidState)
        );

        let mut malformed_json = serde_json::to_value(actor.checkpoint()).unwrap();
        malformed_json["actor"]["horse"]["health"] = serde_json::json!(999);
        assert!(serde_json::from_value::<ActorGameplayCheckpointV2>(malformed_json).is_err());
    }

    #[test]
    fn recall_reductions_floor_phases_and_running_mount_are_exact() {
        let mut recall = RecallKernel::default();
        recall
            .lose_horse(SimulationTick::new(100), SimulationTick::new(100))
            .unwrap();
        assert!(recall.apply_credit(recall_credit(100, 1, RecallCreditKind::DamageDealt(24))));
        assert!(!recall.apply_credit(recall_credit(100, 1, RecallCreditKind::DamageDealt(24))));
        assert!(recall.apply_credit(recall_credit(100, 2, RecallCreditKind::DamageDealt(26))));
        for sequence in 3..=10 {
            assert!(recall.apply_credit(recall_credit(
                100,
                sequence,
                RecallCreditKind::ObjectiveTick,
            )));
        }
        assert_eq!(recall.ready_tick(), Some(SimulationTick::new(580)));
        recall.advance_tick(SimulationTick::new(579)).unwrap();
        assert_eq!(recall.state(), RecallState::CoolingDown);
        recall.advance_tick(SimulationTick::new(580)).unwrap();
        assert_eq!(recall.state(), RecallState::Ready);
        assert!(recall.request_recall(SimulationTick::new(580)));
        recall.advance_tick(SimulationTick::new(700)).unwrap();
        assert_eq!(recall.state(), RecallState::DustReveal);
        recall.advance_tick(SimulationTick::new(790)).unwrap();
        assert_eq!(recall.state(), RecallState::GallopIn);
        let arrived = recall.advance_tick(SimulationTick::new(880)).unwrap();
        assert!(arrived.mount_window_opened);
        assert_eq!(recall.state(), RecallState::MountWindow);
        assert!(recall.try_mount(
            SimulationTick::new(880),
            QuantizedOrigin::new(0, 0, 0),
            QuantizedOrigin::new(4_000, 0, 0),
            true,
        ));
        let telemetry = recall.telemetry().unwrap();
        assert_eq!(telemetry.lose_horse_to_remount_ticks, Some(780));
        assert!(telemetry.running_mount);
    }

    #[test]
    fn running_window_is_half_open_and_stationary_mount_uses_three_metres() {
        let mut recall = RecallKernel::default();
        recall
            .lose_horse(SimulationTick::new(100), SimulationTick::new(50))
            .unwrap();
        recall.advance_tick(SimulationTick::new(1_300)).unwrap();
        assert!(recall.request_recall(SimulationTick::new(1_300)));
        recall.advance_tick(SimulationTick::new(1_600)).unwrap();
        assert_eq!(recall.state(), RecallState::MountWindow);

        let mut running = recall.clone();
        running.advance_tick(SimulationTick::new(1_689)).unwrap();
        assert_eq!(running.state(), RecallState::MountWindow);
        assert!(!running.try_mount(
            SimulationTick::new(1_689),
            QuantizedOrigin::default(),
            QuantizedOrigin::new(3_000, 0, 0),
            false,
        ));
        assert!(running.try_mount(
            SimulationTick::new(1_689),
            QuantizedOrigin::default(),
            QuantizedOrigin::new(4_000, 0, 0),
            true,
        ));

        recall.advance_tick(SimulationTick::new(1_690)).unwrap();
        assert_eq!(recall.state(), RecallState::WaitingMount);
        assert!(!recall.try_mount(
            SimulationTick::new(1_690),
            QuantizedOrigin::default(),
            QuantizedOrigin::new(3_001, 0, 0),
            false,
        ));
        assert!(recall.try_mount(
            SimulationTick::new(1_690),
            QuantizedOrigin::default(),
            QuantizedOrigin::new(3_000, 0, 0),
            false,
        ));
        assert_eq!(
            recall.telemetry().unwrap().lose_horse_to_remount_ticks,
            Some(1_640)
        );
    }

    #[test]
    fn malformed_directions_and_tick_replays_fail_closed() {
        let mut kernel = OnFootKernel::default();
        let output = kernel
            .advance_tick(OnFootTickInput {
                tick: SimulationTick::new(1),
                move_direction: Some(QuantizedDirection::new(1, 2, 3)),
                sprint_pressed: true,
                crouch_pressed: false,
                reload_active: false,
            })
            .unwrap();
        assert_eq!(output.state, OnFootState::Standing);
        assert_eq!(output.requested_velocity_mmps, [0, 0]);
        assert_eq!(
            kernel.advance_tick(OnFootTickInput {
                tick: SimulationTick::new(1),
                move_direction: None,
                sprint_pressed: false,
                crouch_pressed: false,
                reload_active: false,
            }),
            Err(M3Error::TickReplay)
        );
    }
}
