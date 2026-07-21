//! Deterministic M4 Spur economy and Majestic Charge timing.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SimulationTick;

/// Full Spur meter value and the only spend threshold.
pub const SPUR_METER_MAX: u8 = 100;
/// Jump/clean-landing lifetime cap. Eighteen is the largest even total strictly below 20%.
pub const MOVEMENT_STYLE_SPUR_CAP: u8 = 18;
/// A hostile projectile must pass within this distance to earn a near miss.
pub const NEAR_MISS_DISTANCE_MM: u16 = 1_500;
/// A clean landing requires a sway impulse strictly below 0.2.
pub const CLEAN_LANDING_SWAY_LIMIT_MILLI: u16 = 200;
/// Majestic Charge lasts exactly six seconds at the shared 60 Hz authority rate.
pub const MAJESTIC_CHARGE_TICKS: u64 = 6 * 60;
/// Charge acceleration multiplier (2.0x).
pub const CHARGE_ACCEL_MULTIPLIER_MILLI: u16 = 2_000;
/// Charge turn-rate multiplier (+30%).
pub const CHARGE_TURN_MULTIPLIER_MILLI: u16 = 1_300;
/// Charge weapon-sway multiplier (0.7x).
pub const CHARGE_SWAY_MULTIPLIER_MILLI: u16 = 700;
/// Charge forces the terrain handling factor to 1.0.
pub const CHARGE_TERRAIN_MULTIPLIER_MILLI: u16 = 1_000;

/// Monotonic authority identity for one Spur award.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SpurCreditId {
    /// Authority epoch that accepted the source event.
    #[serde(rename = "e", alias = "authority_epoch")]
    pub authority_epoch: u64,
    /// Original event tick.
    #[serde(rename = "t", alias = "tick")]
    pub tick: SimulationTick,
    /// Authority-unique sequence within the epoch and tick.
    #[serde(rename = "s", alias = "sequence")]
    pub sequence: u64,
}

/// One of the four locked M4 fill sources, with evidence needed for validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpurCreditKind {
    /// Authority-observed mounted jump.
    Jump,
    /// Collision-free landing with its authority-computed sway impulse.
    CleanLanding {
        /// False when any landing collision invalidated the style award.
        collision_free: bool,
        /// Landing sway impulse in thousandths.
        sway_impulse_milli: u16,
    },
    /// Closest approach from a hostile projectile.
    NearMiss {
        /// Near misses from self/friendly fire never earn Spur.
        hostile: bool,
        /// Authority-computed closest approach in millimetres.
        closest_distance_mm: u16,
    },
    /// Accepted hit while the shooter is mounted.
    MountedHit,
    /// Rider elimination while the shooter is mounted.
    MountedElimination,
    /// Rider elimination from a live Saddle Dive.
    SaddleDiveElimination,
}

impl SpurCreditKind {
    const fn raw_points(self) -> Option<u8> {
        match self {
            Self::Jump => Some(4),
            Self::CleanLanding {
                collision_free: true,
                sway_impulse_milli,
            } if sway_impulse_milli < CLEAN_LANDING_SWAY_LIMIT_MILLI => Some(2),
            Self::NearMiss {
                hostile: true,
                closest_distance_mm,
            } if closest_distance_mm <= NEAR_MISS_DISTANCE_MM => Some(3),
            Self::MountedHit => Some(2),
            Self::MountedElimination => Some(6),
            Self::SaddleDiveElimination => Some(8),
            Self::CleanLanding { .. } | Self::NearMiss { .. } => None,
        }
    }

    const fn is_movement_style(self) -> bool {
        matches!(self, Self::Jump | Self::CleanLanding { .. })
    }
}

/// Replay-safe authority Spur award.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurCreditCommand {
    /// Strictly increasing credit identity.
    #[serde(rename = "i", alias = "id")]
    pub id: SpurCreditId,
    /// Locked source and validation evidence.
    #[serde(rename = "k", alias = "kind")]
    pub kind: SpurCreditKind,
}

/// Context selected from authority-owned horse state for the Q edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpurSpendContext {
    /// Rider is attached to an available horse.
    Mounted,
    /// Horse is absent and recall has not already begun its return presentation.
    HorseAbsent,
    /// Dive, stun, or an already-running return makes spending ineligible.
    #[default]
    Ineligible,
}

/// Accepted contextual result of spending a full meter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpurSpendOutcome {
    /// Begin a six-second mounted Majestic Charge.
    MajesticCharge,
    /// Bypass the remaining horse-recall cooldown and begin Majestic Return.
    InstantMajesticReturn,
}

/// Output from one absolute authority tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpurTickOutput {
    /// Accepted spend on this exact tick.
    pub spend: Option<SpurSpendOutcome>,
    /// Charge is active for this tick after spend/expiry processing.
    pub charge_active: bool,
    /// True on the exact transition out of charge.
    pub charge_ended: bool,
}

/// M4 replay/order rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum SpurError {
    /// Absolute tick repeated or regressed.
    #[error("tick_replay")]
    TickReplay,
}

/// Checkpoint-ready authority owner for one player's Spur economy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurKernel {
    #[serde(rename = "t", alias = "current_tick")]
    current_tick: Option<SimulationTick>,
    #[serde(rename = "m", alias = "meter")]
    meter: u8,
    #[serde(rename = "p", alias = "movement_style_points")]
    movement_style_points: u8,
    #[serde(rename = "i", alias = "last_credit_id")]
    last_credit_id: Option<SpurCreditId>,
    #[serde(rename = "n", alias = "next_credit_sequence")]
    next_credit_sequence: u64,
    #[serde(rename = "h", alias = "spend_held")]
    spend_held: bool,
    #[serde(rename = "b", alias = "charge_started_tick")]
    charge_started_tick: Option<SimulationTick>,
    #[serde(rename = "e", alias = "charge_end_tick")]
    charge_end_tick: Option<SimulationTick>,
}

impl Default for SpurKernel {
    fn default() -> Self {
        Self {
            current_tick: None,
            meter: 0,
            movement_style_points: 0,
            last_credit_id: None,
            next_credit_sequence: 1,
            spend_held: false,
            charge_started_tick: None,
            charge_end_tick: None,
        }
    }
}

impl SpurKernel {
    /// Last shared authority tick applied to the meter/spend clock.
    #[must_use]
    pub const fn current_tick(&self) -> Option<SimulationTick> {
        self.current_tick
    }

    /// Current meter value.
    #[must_use]
    pub const fn meter(&self) -> u8 {
        self.meter
    }

    /// Lifetime movement-style contribution, capped below 20 points.
    #[must_use]
    pub const fn movement_style_points(&self) -> u8 {
        self.movement_style_points
    }

    /// Exact charge start tick when active.
    #[must_use]
    pub const fn charge_started_tick(&self) -> Option<SimulationTick> {
        self.charge_started_tick
    }

    /// Exclusive charge-end tick when active.
    #[must_use]
    pub const fn charge_end_tick(&self) -> Option<SimulationTick> {
        self.charge_end_tick
    }

    /// Whether charge is active at the kernel's current tick.
    #[must_use]
    pub fn charge_active(&self) -> bool {
        matches!((self.current_tick, self.charge_end_tick), (Some(now), Some(end)) if now < end)
    }

    /// Applies one strictly newer authority-confirmed award and returns points actually added.
    pub fn apply_credit(&mut self, command: SpurCreditCommand) -> Option<u8> {
        let raw_points = command.kind.raw_points()?;
        let next_sequence = command.id.sequence.checked_add(1)?;
        if command.id.sequence == 0
            || self
                .current_tick
                .is_none_or(|current| command.id.tick > current)
            || self.last_credit_id.is_some_and(|last| command.id <= last)
        {
            return None;
        }
        let source_points = if command.kind.is_movement_style() {
            raw_points.min(MOVEMENT_STYLE_SPUR_CAP.saturating_sub(self.movement_style_points))
        } else {
            raw_points
        };
        let points = source_points.min(SPUR_METER_MAX.saturating_sub(self.meter));
        self.last_credit_id = Some(command.id);
        self.next_credit_sequence = self.next_credit_sequence.max(next_sequence);
        if points == 0 {
            return Some(0);
        }
        self.meter = self.meter.saturating_add(points).min(SPUR_METER_MAX);
        if command.kind.is_movement_style() {
            self.movement_style_points += points;
        }
        Some(points)
    }

    /// Issues a credit from trusted authority evidence using checkpointed sequence state.
    pub fn issue_credit(
        &mut self,
        authority_epoch: u64,
        tick: SimulationTick,
        kind: SpurCreditKind,
    ) -> Option<u8> {
        self.apply_credit(SpurCreditCommand {
            id: SpurCreditId {
                authority_epoch,
                tick,
                sequence: self.next_credit_sequence,
            },
            kind,
        })
    }

    /// Advances the no-decay meter and contextual Q spend on one absolute tick.
    pub fn advance_tick(
        &mut self,
        tick: SimulationTick,
        spend_pressed: bool,
        context: SpurSpendContext,
    ) -> Result<SpurTickOutput, SpurError> {
        if self.current_tick.is_some_and(|current| tick <= current) {
            return Err(SpurError::TickReplay);
        }
        self.current_tick = Some(tick);
        let charge_ended = self.charge_end_tick.is_some_and(|end| tick >= end);
        if charge_ended {
            self.charge_started_tick = None;
            self.charge_end_tick = None;
        }

        let rising_edge = spend_pressed && !self.spend_held;
        self.spend_held = spend_pressed;
        let spend = if rising_edge && self.meter == SPUR_METER_MAX && !self.charge_active() {
            match context {
                SpurSpendContext::Mounted => {
                    self.meter = 0;
                    self.charge_started_tick = Some(tick);
                    self.charge_end_tick = Some(tick.saturating_add(MAJESTIC_CHARGE_TICKS));
                    Some(SpurSpendOutcome::MajesticCharge)
                }
                SpurSpendContext::HorseAbsent => {
                    self.meter = 0;
                    Some(SpurSpendOutcome::InstantMajesticReturn)
                }
                SpurSpendContext::Ineligible => None,
            }
        } else {
            None
        };
        Ok(SpurTickOutput {
            spend,
            charge_active: self.charge_active(),
            charge_ended,
        })
    }

    /// Validates private invariants before a containing migration checkpoint is installed.
    #[must_use]
    pub fn state_is_valid(&self) -> bool {
        self.meter <= SPUR_METER_MAX
            && self.movement_style_points <= MOVEMENT_STYLE_SPUR_CAP
            && self.next_credit_sequence > 0
            && self.last_credit_id.is_none_or(|id| {
                id.sequence < self.next_credit_sequence
                    && self.current_tick.is_some_and(|current| id.tick <= current)
            })
            && match (self.charge_started_tick, self.charge_end_tick) {
                (None, None) => true,
                (Some(start), Some(end)) => {
                    end == start.saturating_add(MAJESTIC_CHARGE_TICKS)
                        && self
                            .current_tick
                            .is_some_and(|current| start <= current && current < end)
                }
                _ => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credit(sequence: u64, tick: u64, kind: SpurCreditKind) -> SpurCreditCommand {
        SpurCreditCommand {
            id: SpurCreditId {
                authority_epoch: 1,
                tick: SimulationTick::new(tick),
                sequence,
            },
            kind,
        }
    }

    fn advance(kernel: &mut SpurKernel, tick: u64) {
        kernel
            .advance_tick(
                SimulationTick::new(tick),
                false,
                SpurSpendContext::Ineligible,
            )
            .expect("fresh tick");
    }

    #[test]
    fn exact_sources_validate_and_movement_style_cannot_fill_twenty_percent() {
        let mut kernel = SpurKernel::default();
        advance(&mut kernel, 1);
        assert_eq!(
            kernel.apply_credit(credit(1, 1, SpurCreditKind::Jump)),
            Some(4)
        );
        assert_eq!(
            kernel.apply_credit(credit(
                2,
                1,
                SpurCreditKind::CleanLanding {
                    collision_free: true,
                    sway_impulse_milli: 199,
                },
            )),
            Some(2)
        );
        assert_eq!(
            kernel.apply_credit(credit(
                3,
                1,
                SpurCreditKind::CleanLanding {
                    collision_free: true,
                    sway_impulse_milli: 200,
                },
            )),
            None
        );
        assert_eq!(
            kernel.apply_credit(credit(
                3,
                1,
                SpurCreditKind::NearMiss {
                    hostile: false,
                    closest_distance_mm: 1,
                },
            )),
            None
        );
        assert_eq!(
            kernel.apply_credit(credit(
                3,
                1,
                SpurCreditKind::NearMiss {
                    hostile: true,
                    closest_distance_mm: 1_500,
                },
            )),
            Some(3)
        );
        for sequence in 4..=8 {
            let _ = kernel.apply_credit(credit(sequence, 1, SpurCreditKind::Jump));
        }
        assert_eq!(kernel.movement_style_points(), MOVEMENT_STYLE_SPUR_CAP);
        assert_eq!(kernel.meter(), 21);
    }

    #[test]
    fn credits_are_monotonic_capped_and_never_decay() {
        let mut kernel = SpurKernel::default();
        advance(&mut kernel, 10);
        let command = credit(1, 10, SpurCreditKind::SaddleDiveElimination);
        assert_eq!(kernel.apply_credit(command), Some(8));
        assert_eq!(kernel.apply_credit(command), None);
        assert_eq!(
            kernel.apply_credit(credit(2, 11, SpurCreditKind::MountedHit)),
            None
        );
        for sequence in 2..=20 {
            assert!(kernel
                .apply_credit(credit(sequence, 10, SpurCreditKind::SaddleDiveElimination))
                .is_some());
        }
        assert_eq!(kernel.meter(), SPUR_METER_MAX);
        advance(&mut kernel, 600);
        assert_eq!(kernel.meter(), SPUR_METER_MAX);
        assert!(kernel.state_is_valid());
    }

    #[test]
    fn mounted_spend_is_rising_edge_and_charge_has_exact_half_open_duration() {
        let mut kernel = SpurKernel::default();
        advance(&mut kernel, 1);
        for sequence in 1..=13 {
            let _ = kernel.apply_credit(credit(sequence, 1, SpurCreditKind::SaddleDiveElimination));
        }
        let start = kernel
            .advance_tick(SimulationTick::new(2), true, SpurSpendContext::Mounted)
            .expect("spend");
        assert_eq!(start.spend, Some(SpurSpendOutcome::MajesticCharge));
        assert!(start.charge_active);
        assert_eq!(kernel.meter(), 0);
        assert_eq!(kernel.charge_end_tick(), Some(SimulationTick::new(362)));
        let held = kernel
            .advance_tick(SimulationTick::new(361), true, SpurSpendContext::Mounted)
            .expect("held");
        assert!(held.charge_active);
        assert_eq!(held.spend, None);
        let end = kernel
            .advance_tick(SimulationTick::new(362), true, SpurSpendContext::Mounted)
            .expect("end");
        assert!(!end.charge_active);
        assert!(end.charge_ended);
        assert!(kernel.state_is_valid());
    }

    #[test]
    fn full_horseless_spend_returns_without_starting_charge() {
        let mut kernel = SpurKernel::default();
        advance(&mut kernel, 1);
        for sequence in 1..=13 {
            let _ = kernel.apply_credit(credit(sequence, 1, SpurCreditKind::SaddleDiveElimination));
        }
        let output = kernel
            .advance_tick(SimulationTick::new(2), true, SpurSpendContext::HorseAbsent)
            .expect("instant return");
        assert_eq!(output.spend, Some(SpurSpendOutcome::InstantMajesticReturn));
        assert!(!output.charge_active);
        assert_eq!(kernel.meter(), 0);
    }
}
