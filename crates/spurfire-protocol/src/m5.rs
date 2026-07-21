//! Deterministic M5 Bounty Run clock, scoring, reveals, objectives, and respawns.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{PlayerId, SimulationTick, MAX_M3_AUTHORITY_ACTORS};

/// Shared authority rate used by every M5 duration.
pub const M5_TICK_RATE_HZ: u64 = 60;
/// Locked Alpha match duration: fifteen minutes.
pub const BOUNTY_RUN_DURATION_TICKS: u64 = 15 * 60 * M5_TICK_RATE_HZ;
/// Rider respawn delay.
pub const RESPAWN_DELAY_TICKS: u64 = 5 * M5_TICK_RATE_HZ;
/// Post-respawn movement boost duration.
pub const RESPAWN_SPEED_BUFF_TICKS: u64 = 10 * M5_TICK_RATE_HZ;
/// Most Wanted selection cadence.
pub const MOST_WANTED_CADENCE_TICKS: u64 = 60 * M5_TICK_RATE_HZ;
/// Most Wanted reveal duration.
pub const MOST_WANTED_REVEAL_TICKS: u64 = 10 * M5_TICK_RATE_HZ;
/// Dynamic objective cadence.
pub const OBJECTIVE_CADENCE_TICKS: u64 = 90 * M5_TICK_RATE_HZ;
/// Dynamic objective lifetime.
pub const OBJECTIVE_LIFETIME_TICKS: u64 = 60 * M5_TICK_RATE_HZ;
/// Assist damage must have been dealt less than five seconds before elimination.
pub const ASSIST_WINDOW_TICKS: u64 = 5 * M5_TICK_RATE_HZ;
/// Minimum recent damage for an assist.
pub const ASSIST_DAMAGE_THRESHOLD: u16 = 30;
/// Long-hit threshold is strictly greater than sixty metres.
pub const LONG_HIT_DISTANCE_MM: u32 = 60_000;
/// Per-player long-hit bonus cap.
pub const LONG_HIT_BONUS_CAP: u16 = 50;
/// Horse station buff duration.
pub const HORSE_STATION_BUFF_TICKS: u64 = 60 * M5_TICK_RATE_HZ;
/// Signal tower pays once per ten seconds held.
pub const SIGNAL_TOWER_INTERVAL_TICKS: u64 = 10 * M5_TICK_RATE_HZ;
/// Maximum bounded score accepted in a checkpoint.
pub const MAX_BOUNTY_SCORE: u32 = 1_000_000;

/// Stable M5 score categories for HUD and telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BountyScoreCategory {
    Elimination,
    Assist,
    HorseBolt,
    SaddleDiveBonus,
    MountedLongHit,
    Objective,
    MostWantedElimination,
    MostWantedSurvival,
}

impl BountyScoreCategory {
    /// Locked score value for non-objective rows.
    #[must_use]
    pub const fn points(self) -> u16 {
        match self {
            Self::Elimination => 100,
            Self::Assist => 50,
            Self::HorseBolt => 15,
            Self::SaddleDiveBonus => 25,
            Self::MountedLongHit => 10,
            Self::Objective => 0,
            Self::MostWantedElimination => 75,
            Self::MostWantedSurvival => 10,
        }
    }
}

/// One authority score mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BountyAward {
    pub player_id: PlayerId,
    pub category: BountyScoreCategory,
    pub points: u16,
    pub total_score: u32,
}

/// Per-category score totals retained through migration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BountyScoreBreakdown {
    pub elimination: u32,
    pub assist: u32,
    pub horse_bolt: u32,
    pub saddle_dive_bonus: u32,
    pub mounted_long_hit: u32,
    pub objective: u32,
    pub most_wanted_elimination: u32,
    pub most_wanted_survival: u32,
}

impl BountyScoreBreakdown {
    #[must_use]
    pub const fn total(self) -> u32 {
        self.elimination
            .saturating_add(self.assist)
            .saturating_add(self.horse_bolt)
            .saturating_add(self.saddle_dive_bonus)
            .saturating_add(self.mounted_long_hit)
            .saturating_add(self.objective)
            .saturating_add(self.most_wanted_elimination)
            .saturating_add(self.most_wanted_survival)
    }

    fn add(&mut self, category: BountyScoreCategory, points: u16) {
        let target = match category {
            BountyScoreCategory::Elimination => &mut self.elimination,
            BountyScoreCategory::Assist => &mut self.assist,
            BountyScoreCategory::HorseBolt => &mut self.horse_bolt,
            BountyScoreCategory::SaddleDiveBonus => &mut self.saddle_dive_bonus,
            BountyScoreCategory::MountedLongHit => &mut self.mounted_long_hit,
            BountyScoreCategory::Objective => &mut self.objective,
            BountyScoreCategory::MostWantedElimination => &mut self.most_wanted_elimination,
            BountyScoreCategory::MostWantedSurvival => &mut self.most_wanted_survival,
        };
        *target = target
            .saturating_add(u32::from(points))
            .min(MAX_BOUNTY_SCORE);
    }
}

/// Authority-owned scoreboard row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BountyPlayerState {
    pub player_id: PlayerId,
    pub score: BountyScoreBreakdown,
    pub eliminations: u16,
    pub assists: u16,
    pub deaths: u16,
    pub alive: bool,
    pub respawn_at_tick: Option<SimulationTick>,
    pub respawn_speed_buff_end_tick: Option<SimulationTick>,
    pub horse_buff_end_tick: Option<SimulationTick>,
    pub mounted_long_hit_bonus: u16,
}

impl BountyPlayerState {
    fn new(player_id: PlayerId) -> Self {
        Self {
            player_id,
            score: BountyScoreBreakdown::default(),
            eliminations: 0,
            assists: 0,
            deaths: 0,
            alive: true,
            respawn_at_tick: None,
            respawn_speed_buff_end_tick: None,
            horse_buff_end_tick: None,
            mounted_long_hit_bonus: 0,
        }
    }

    #[must_use]
    pub const fn total_score(&self) -> u32 {
        self.score.total()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DamageContribution {
    damage: u16,
    last_tick: SimulationTick,
}

/// Active Most Wanted reveal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MostWantedReveal {
    pub player_id: PlayerId,
    pub started_tick: SimulationTick,
    pub end_tick: SimulationTick,
    eliminated: bool,
}

/// Locked objective cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicObjectiveKind {
    MovingBounty,
    SupplyHerd,
    AmmoWagon,
    SignalTower,
    HorseBuffStation,
}

impl DynamicObjectiveKind {
    #[must_use]
    pub const fn completion_points(self) -> u16 {
        match self {
            Self::MovingBounty => 150,
            Self::SupplyHerd => 100,
            Self::AmmoWagon => 80,
            Self::SignalTower => 50,
            Self::HorseBuffStation => 50,
        }
    }

    const fn from_index(index: u64) -> Self {
        match index % 5 {
            0 => Self::MovingBounty,
            1 => Self::SupplyHerd,
            2 => Self::AmmoWagon,
            3 => Self::SignalTower,
            _ => Self::HorseBuffStation,
        }
    }
}

/// One deterministic objective window. Placement is derived by the game layer
/// from `objective_id` and the active-territory seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveDynamicObjective {
    pub objective_id: u64,
    pub kind: DynamicObjectiveKind,
    pub started_tick: SimulationTick,
    pub end_tick: SimulationTick,
    pub completed: bool,
    signal_paid_intervals: u8,
}

/// Absolute-tick match output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BountyMatchTickOutput {
    pub respawned_players: Vec<PlayerId>,
    pub reveal_started: Option<MostWantedReveal>,
    pub reveal_ended: Option<PlayerId>,
    pub objective_started: Option<ActiveDynamicObjective>,
    pub objective_ended: Option<u64>,
    pub survival_award: Option<BountyAward>,
    pub match_ended: bool,
    pub winner: Option<PlayerId>,
}

/// Fail-closed M5 operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum BountyMatchError {
    #[error("invalid_roster")]
    InvalidRoster,
    #[error("tick_replay")]
    TickReplay,
    #[error("tick_mismatch")]
    TickMismatch,
    #[error("unknown_player")]
    UnknownPlayer,
    #[error("invalid_event")]
    InvalidEvent,
    #[error("match_finished")]
    MatchFinished,
    #[error("invalid_checkpoint")]
    InvalidCheckpoint,
    #[error("authority_epoch")]
    AuthorityEpoch,
}

/// Checkpoint-ready authority kernel for one Bounty Run match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BountyMatchKernel {
    authority_epoch: u64,
    lobby_seed: u64,
    start_tick: SimulationTick,
    current_tick: SimulationTick,
    end_tick: SimulationTick,
    players: BTreeMap<PlayerId, BountyPlayerState>,
    damage: BTreeMap<PlayerId, BTreeMap<PlayerId, DamageContribution>>,
    next_reveal_tick: SimulationTick,
    active_reveal: Option<MostWantedReveal>,
    next_objective_tick: SimulationTick,
    active_objective: Option<ActiveDynamicObjective>,
    objective_sequence: u64,
    finished: bool,
}

/// Canonical migration payload for M5 state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BountyMatchCheckpointV2 {
    pub source_authority_epoch: u64,
    match_state: BountyMatchKernel,
}

impl BountyMatchCheckpointV2 {
    #[must_use]
    pub const fn match_state(&self) -> &BountyMatchKernel {
        &self.match_state
    }
}

impl BountyMatchKernel {
    pub fn new(
        authority_epoch: u64,
        lobby_seed: u64,
        start_tick: SimulationTick,
        mut roster: Vec<PlayerId>,
    ) -> Result<Self, BountyMatchError> {
        let submitted_roster_len = roster.len();
        roster.sort_unstable();
        roster.dedup();
        if roster.is_empty()
            || roster.len() != submitted_roster_len
            || roster.len() > MAX_M3_AUTHORITY_ACTORS
        {
            return Err(BountyMatchError::InvalidRoster);
        }
        let players = roster
            .into_iter()
            .map(|player| (player, BountyPlayerState::new(player)))
            .collect();
        Ok(Self {
            authority_epoch,
            lobby_seed,
            start_tick,
            current_tick: start_tick,
            end_tick: start_tick.saturating_add(BOUNTY_RUN_DURATION_TICKS),
            players,
            damage: BTreeMap::new(),
            next_reveal_tick: start_tick.saturating_add(MOST_WANTED_CADENCE_TICKS),
            active_reveal: None,
            next_objective_tick: start_tick.saturating_add(OBJECTIVE_CADENCE_TICKS),
            active_objective: None,
            objective_sequence: 0,
            finished: false,
        })
    }

    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    #[must_use]
    pub const fn current_tick(&self) -> SimulationTick {
        self.current_tick
    }

    #[must_use]
    pub const fn end_tick(&self) -> SimulationTick {
        self.end_tick
    }

    #[must_use]
    pub const fn finished(&self) -> bool {
        self.finished
    }

    #[must_use]
    pub fn player(&self, player: PlayerId) -> Option<&BountyPlayerState> {
        self.players.get(&player)
    }

    pub fn players(&self) -> impl Iterator<Item = &BountyPlayerState> {
        self.players.values()
    }

    #[must_use]
    pub const fn active_reveal(&self) -> Option<MostWantedReveal> {
        self.active_reveal
    }

    #[must_use]
    pub const fn active_objective(&self) -> Option<ActiveDynamicObjective> {
        self.active_objective
    }

    #[must_use]
    pub fn winner(&self) -> Option<PlayerId> {
        self.players
            .values()
            .max_by(|left, right| {
                left.total_score()
                    .cmp(&right.total_score())
                    .then_with(|| right.player_id.cmp(&left.player_id))
            })
            .map(|row| row.player_id)
    }

    pub fn advance_tick(
        &mut self,
        tick: SimulationTick,
    ) -> Result<BountyMatchTickOutput, BountyMatchError> {
        if tick <= self.current_tick {
            return Err(BountyMatchError::TickReplay);
        }
        let mut output = BountyMatchTickOutput::default();
        let effective_tick = SimulationTick::new(tick.as_u64().min(self.end_tick.as_u64()));
        self.current_tick = tick;

        for row in self.players.values_mut() {
            if !row.alive && row.respawn_at_tick.is_some_and(|due| due <= effective_tick) {
                let respawn_tick = row.respawn_at_tick.expect("checked respawn tick");
                row.alive = true;
                row.respawn_at_tick = None;
                let buff_end = respawn_tick.saturating_add(RESPAWN_SPEED_BUFF_TICKS);
                row.respawn_speed_buff_end_tick = (buff_end > effective_tick).then_some(buff_end);
                output.respawned_players.push(row.player_id);
            }
            if row
                .respawn_speed_buff_end_tick
                .is_some_and(|end| end <= effective_tick)
            {
                row.respawn_speed_buff_end_tick = None;
            }
            if row
                .horse_buff_end_tick
                .is_some_and(|end| end <= effective_tick)
            {
                row.horse_buff_end_tick = None;
            }
        }

        self.advance_reveals(effective_tick, &mut output);
        self.advance_objectives(effective_tick, &mut output);

        if tick >= self.end_tick && !self.finished {
            self.finished = true;
            self.active_reveal = None;
            self.active_objective = None;
            output.match_ended = true;
            output.winner = self.winner();
        }
        output.respawned_players.sort_unstable();
        Ok(output)
    }

    pub fn record_damage(
        &mut self,
        tick: SimulationTick,
        attacker: PlayerId,
        target: PlayerId,
        damage: u16,
    ) -> Result<(), BountyMatchError> {
        self.require_live_tick(tick)?;
        if damage == 0 || attacker == target || !self.players.contains_key(&attacker) {
            return Err(BountyMatchError::InvalidEvent);
        }
        let Some(target_state) = self.players.get(&target) else {
            return Err(BountyMatchError::UnknownPlayer);
        };
        if !target_state.alive {
            return Err(BountyMatchError::InvalidEvent);
        }
        let contribution = self
            .damage
            .entry(target)
            .or_default()
            .entry(attacker)
            .or_insert(DamageContribution {
                damage: 0,
                last_tick: tick,
            });
        contribution.damage = contribution.damage.saturating_add(damage);
        contribution.last_tick = tick;
        Ok(())
    }

    pub fn record_elimination(
        &mut self,
        tick: SimulationTick,
        killer: PlayerId,
        target: PlayerId,
        saddle_dive: bool,
    ) -> Result<Vec<BountyAward>, BountyMatchError> {
        self.require_live_tick(tick)?;
        if killer == target || !self.players.contains_key(&killer) {
            return Err(BountyMatchError::InvalidEvent);
        }
        if !self.players.get(&target).is_some_and(|row| row.alive) {
            return Err(BountyMatchError::UnknownPlayer);
        }

        let most_wanted = self.active_reveal.is_some_and(|reveal| {
            reveal.player_id == target && tick >= reveal.started_tick && tick < reveal.end_tick
        });
        if most_wanted {
            if let Some(reveal) = self.active_reveal.as_mut() {
                reveal.eliminated = true;
            }
        }

        let mut awards = Vec::new();
        awards.push(self.award(killer, BountyScoreCategory::Elimination, 100)?);
        if saddle_dive {
            awards.push(self.award(killer, BountyScoreCategory::SaddleDiveBonus, 25)?);
        }
        if most_wanted {
            awards.push(self.award(killer, BountyScoreCategory::MostWantedElimination, 75)?);
        }
        let killer_state = self.players.get_mut(&killer).expect("validated killer");
        killer_state.eliminations = killer_state.eliminations.saturating_add(1);

        if let Some(contributors) = self.damage.remove(&target) {
            for (attacker, contribution) in contributors {
                if attacker != killer
                    && contribution.damage >= ASSIST_DAMAGE_THRESHOLD
                    && tick
                        .as_u64()
                        .saturating_sub(contribution.last_tick.as_u64())
                        < ASSIST_WINDOW_TICKS
                {
                    awards.push(self.award(attacker, BountyScoreCategory::Assist, 50)?);
                    let row = self
                        .players
                        .get_mut(&attacker)
                        .expect("contributor is rostered");
                    row.assists = row.assists.saturating_add(1);
                }
            }
        }
        let target_state = self.players.get_mut(&target).expect("validated target");
        target_state.alive = false;
        target_state.deaths = target_state.deaths.saturating_add(1);
        target_state.respawn_at_tick = Some(tick.saturating_add(RESPAWN_DELAY_TICKS));
        target_state.respawn_speed_buff_end_tick = None;
        Ok(awards)
    }

    pub fn record_horse_bolt(
        &mut self,
        tick: SimulationTick,
        attacker: PlayerId,
        target: PlayerId,
    ) -> Result<BountyAward, BountyMatchError> {
        self.require_live_tick(tick)?;
        if attacker == target || !self.players.contains_key(&target) {
            return Err(BountyMatchError::InvalidEvent);
        }
        self.award(attacker, BountyScoreCategory::HorseBolt, 15)
    }

    pub fn record_mounted_long_hit(
        &mut self,
        tick: SimulationTick,
        attacker: PlayerId,
        distance_mm: u32,
    ) -> Result<Option<BountyAward>, BountyMatchError> {
        self.require_live_tick(tick)?;
        if distance_mm <= LONG_HIT_DISTANCE_MM {
            return Ok(None);
        }
        let row = self
            .players
            .get_mut(&attacker)
            .ok_or(BountyMatchError::UnknownPlayer)?;
        if row.mounted_long_hit_bonus >= LONG_HIT_BONUS_CAP {
            return Ok(None);
        }
        row.mounted_long_hit_bonus += 10;
        self.award(attacker, BountyScoreCategory::MountedLongHit, 10)
            .map(Some)
    }

    pub fn complete_objective(
        &mut self,
        tick: SimulationTick,
        player: PlayerId,
        objective_id: u64,
    ) -> Result<BountyAward, BountyMatchError> {
        self.require_live_tick(tick)?;
        if !self.players.contains_key(&player) {
            return Err(BountyMatchError::UnknownPlayer);
        }
        let Some(objective) = self.active_objective.as_mut() else {
            return Err(BountyMatchError::InvalidEvent);
        };
        if objective.objective_id != objective_id
            || objective.completed
            || objective.kind == DynamicObjectiveKind::SignalTower
            || tick >= objective.end_tick
        {
            return Err(BountyMatchError::InvalidEvent);
        }
        let kind = objective.kind;
        objective.completed = true;
        if kind == DynamicObjectiveKind::HorseBuffStation {
            self.players
                .get_mut(&player)
                .expect("validated objective player")
                .horse_buff_end_tick = Some(tick.saturating_add(HORSE_STATION_BUFF_TICKS));
        }
        self.award(
            player,
            BountyScoreCategory::Objective,
            kind.completion_points(),
        )
    }

    pub fn record_signal_tower_hold(
        &mut self,
        tick: SimulationTick,
        player: PlayerId,
        objective_id: u64,
        total_hold_ticks: u64,
    ) -> Result<Option<BountyAward>, BountyMatchError> {
        self.require_live_tick(tick)?;
        if !self.players.contains_key(&player) {
            return Err(BountyMatchError::UnknownPlayer);
        }
        let Some(objective) = self.active_objective.as_mut() else {
            return Err(BountyMatchError::InvalidEvent);
        };
        if objective.objective_id != objective_id
            || objective.kind != DynamicObjectiveKind::SignalTower
            || tick >= objective.end_tick
        {
            return Err(BountyMatchError::InvalidEvent);
        }
        if objective.completed || objective.signal_paid_intervals >= 3 {
            return Ok(None);
        }
        let earned = (total_hold_ticks / SIGNAL_TOWER_INTERVAL_TICKS).min(3) as u8;
        if earned <= objective.signal_paid_intervals {
            return Ok(None);
        }
        objective.signal_paid_intervals += 1;
        if objective.signal_paid_intervals == 3 {
            objective.completed = true;
        }
        self.award(player, BountyScoreCategory::Objective, 50)
            .map(Some)
    }

    #[must_use]
    pub fn checkpoint(&self) -> BountyMatchCheckpointV2 {
        BountyMatchCheckpointV2 {
            source_authority_epoch: self.authority_epoch,
            match_state: self.clone(),
        }
    }

    pub fn restore_checkpoint(
        mut checkpoint: BountyMatchCheckpointV2,
        next_authority_epoch: u64,
    ) -> Result<Self, BountyMatchError> {
        if checkpoint.source_authority_epoch.checked_add(1) != Some(next_authority_epoch)
            || checkpoint.match_state.authority_epoch != checkpoint.source_authority_epoch
            || !checkpoint.match_state.state_is_valid()
        {
            return Err(BountyMatchError::InvalidCheckpoint);
        }
        checkpoint.match_state.authority_epoch = next_authority_epoch;
        Ok(checkpoint.match_state)
    }

    #[must_use]
    pub fn state_is_valid(&self) -> bool {
        if self.players.is_empty()
            || self.players.len() > MAX_M3_AUTHORITY_ACTORS
            || self.current_tick < self.start_tick
            || self.end_tick != self.start_tick.saturating_add(BOUNTY_RUN_DURATION_TICKS)
            || self.finished != (self.current_tick >= self.end_tick)
            || (self.finished && (self.active_reveal.is_some() || self.active_objective.is_some()))
            || self.objective_sequence == u64::MAX
            || self.players.iter().any(|(id, row)| {
                *id != row.player_id
                    || row.total_score() > MAX_BOUNTY_SCORE
                    || row.mounted_long_hit_bonus > LONG_HIT_BONUS_CAP
                    || row.score.mounted_long_hit != u32::from(row.mounted_long_hit_bonus)
                    || row.alive == row.respawn_at_tick.is_some()
                    || row
                        .respawn_at_tick
                        .is_some_and(|respawn| !self.finished && respawn <= self.current_tick)
                    || row
                        .respawn_speed_buff_end_tick
                        .is_some_and(|end| end <= self.current_tick)
                    || row
                        .horse_buff_end_tick
                        .is_some_and(|end| end <= self.current_tick)
            })
            || self.damage.iter().any(|(target, contributors)| {
                !self.players.contains_key(target)
                    || contributors.len() > self.players.len()
                    || contributors.iter().any(|(attacker, contribution)| {
                        attacker == target
                            || !self.players.contains_key(attacker)
                            || contribution.damage == 0
                            || contribution.last_tick > self.current_tick
                    })
            })
        {
            return false;
        }
        if let Some(reveal) = self.active_reveal {
            if !self.players.contains_key(&reveal.player_id)
                || reveal.end_tick != reveal.started_tick.saturating_add(MOST_WANTED_REVEAL_TICKS)
                || self.current_tick >= reveal.end_tick
            {
                return false;
            }
        }
        self.active_objective.is_none_or(|objective| {
            objective.objective_id > 0
                && objective.end_tick
                    == objective
                        .started_tick
                        .saturating_add(OBJECTIVE_LIFETIME_TICKS)
                && objective.signal_paid_intervals <= 3
                && self.current_tick < objective.end_tick
        })
    }

    fn require_live_tick(&self, tick: SimulationTick) -> Result<(), BountyMatchError> {
        if self.finished {
            Err(BountyMatchError::MatchFinished)
        } else if tick != self.current_tick {
            Err(BountyMatchError::TickMismatch)
        } else {
            Ok(())
        }
    }

    fn award(
        &mut self,
        player: PlayerId,
        category: BountyScoreCategory,
        points: u16,
    ) -> Result<BountyAward, BountyMatchError> {
        let row = self
            .players
            .get_mut(&player)
            .ok_or(BountyMatchError::UnknownPlayer)?;
        row.score.add(category, points);
        Ok(BountyAward {
            player_id: player,
            category,
            points,
            total_score: row.total_score(),
        })
    }

    fn advance_reveals(&mut self, tick: SimulationTick, output: &mut BountyMatchTickOutput) {
        while self.next_reveal_tick <= tick && self.next_reveal_tick < self.end_tick {
            self.close_reveal(self.next_reveal_tick, output);
            let leader = self.winner();
            if let Some(player_id) = leader {
                let reveal = MostWantedReveal {
                    player_id,
                    started_tick: self.next_reveal_tick,
                    end_tick: self
                        .next_reveal_tick
                        .saturating_add(MOST_WANTED_REVEAL_TICKS),
                    eliminated: false,
                };
                self.active_reveal = Some(reveal);
                output.reveal_started = Some(reveal);
            }
            self.next_reveal_tick = self
                .next_reveal_tick
                .saturating_add(MOST_WANTED_CADENCE_TICKS);
        }
        self.close_reveal(tick, output);
    }

    fn close_reveal(&mut self, tick: SimulationTick, output: &mut BountyMatchTickOutput) {
        let Some(reveal) = self.active_reveal.filter(|reveal| reveal.end_tick <= tick) else {
            return;
        };
        self.active_reveal = None;
        output.reveal_ended = Some(reveal.player_id);
        if !reveal.eliminated {
            output.survival_award = self
                .award(
                    reveal.player_id,
                    BountyScoreCategory::MostWantedSurvival,
                    10,
                )
                .ok();
        }
    }

    fn advance_objectives(&mut self, tick: SimulationTick, output: &mut BountyMatchTickOutput) {
        if self
            .active_objective
            .is_some_and(|objective| objective.end_tick <= tick)
        {
            output.objective_ended = self.active_objective.map(|row| row.objective_id);
            self.active_objective = None;
        }
        while self.next_objective_tick <= tick && self.next_objective_tick < self.end_tick {
            if self
                .active_objective
                .is_some_and(|objective| objective.end_tick <= self.next_objective_tick)
            {
                output.objective_ended = self.active_objective.map(|row| row.objective_id);
                self.active_objective = None;
            }
            self.objective_sequence = self.objective_sequence.saturating_add(1);
            let objective = ActiveDynamicObjective {
                objective_id: self.objective_sequence,
                kind: DynamicObjectiveKind::from_index(
                    self.lobby_seed
                        .wrapping_add(self.objective_sequence.saturating_sub(1)),
                ),
                started_tick: self.next_objective_tick,
                end_tick: self
                    .next_objective_tick
                    .saturating_add(OBJECTIVE_LIFETIME_TICKS),
                completed: false,
                signal_paid_intervals: 0,
            };
            self.active_objective = Some(objective);
            output.objective_started = Some(objective);
            self.next_objective_tick = self
                .next_objective_tick
                .saturating_add(OBJECTIVE_CADENCE_TICKS);
        }
        if self
            .active_objective
            .is_some_and(|objective| objective.end_tick <= tick)
        {
            output.objective_ended = self.active_objective.map(|row| row.objective_id);
            self.active_objective = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(value: u8) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{value:012}")).unwrap()
    }

    fn kernel() -> BountyMatchKernel {
        BountyMatchKernel::new(
            4,
            9,
            SimulationTick::new(0),
            vec![player(3), player(1), player(2)],
        )
        .unwrap()
    }

    #[test]
    fn elimination_assist_respawn_and_locked_score_rows_are_exact() {
        let mut match_state = kernel();
        match_state.advance_tick(SimulationTick::new(1)).unwrap();
        match_state
            .record_damage(SimulationTick::new(1), player(2), player(3), 30)
            .unwrap();
        match_state.advance_tick(SimulationTick::new(299)).unwrap();
        let awards = match_state
            .record_elimination(SimulationTick::new(299), player(1), player(3), true)
            .unwrap();
        assert_eq!(awards.len(), 3);
        assert_eq!(match_state.player(player(1)).unwrap().total_score(), 125);
        assert_eq!(match_state.player(player(2)).unwrap().total_score(), 50);
        assert!(!match_state.player(player(3)).unwrap().alive);

        let output = match_state.advance_tick(SimulationTick::new(599)).unwrap();
        assert_eq!(output.respawned_players, vec![player(3)]);
        assert_eq!(
            match_state
                .player(player(3))
                .unwrap()
                .respawn_speed_buff_end_tick,
            Some(SimulationTick::new(1_199))
        );
    }

    #[test]
    fn assist_window_is_strict_and_long_hits_cap_at_fifty() {
        let mut match_state = kernel();
        match_state.advance_tick(SimulationTick::new(1)).unwrap();
        match_state
            .record_damage(SimulationTick::new(1), player(2), player(3), 99)
            .unwrap();
        match_state.advance_tick(SimulationTick::new(301)).unwrap();
        let awards = match_state
            .record_elimination(SimulationTick::new(301), player(1), player(3), false)
            .unwrap();
        assert_eq!(awards.len(), 1);

        for tick in 302..=307 {
            match_state.advance_tick(SimulationTick::new(tick)).unwrap();
            let award = match_state
                .record_mounted_long_hit(SimulationTick::new(tick), player(1), 60_001)
                .unwrap();
            assert_eq!(award.is_some(), tick <= 306);
        }
        assert_eq!(
            match_state
                .player(player(1))
                .unwrap()
                .mounted_long_hit_bonus,
            50
        );
    }

    #[test]
    fn most_wanted_ties_reveals_survival_and_bounty_are_deterministic() {
        let mut match_state = kernel();
        let first = match_state
            .advance_tick(SimulationTick::new(MOST_WANTED_CADENCE_TICKS))
            .unwrap();
        assert_eq!(first.reveal_started.unwrap().player_id, player(1));
        let survived = match_state
            .advance_tick(SimulationTick::new(
                MOST_WANTED_CADENCE_TICKS + MOST_WANTED_REVEAL_TICKS,
            ))
            .unwrap();
        assert_eq!(survived.survival_award.unwrap().points, 10);

        match_state
            .advance_tick(SimulationTick::new(2 * MOST_WANTED_CADENCE_TICKS))
            .unwrap();
        let awards = match_state
            .record_elimination(
                SimulationTick::new(2 * MOST_WANTED_CADENCE_TICKS),
                player(2),
                player(1),
                false,
            )
            .unwrap();
        assert_eq!(awards.iter().map(|row| row.points).sum::<u16>(), 175);
    }

    #[test]
    fn objectives_cycle_pay_exact_rows_and_signal_caps_at_one_fifty() {
        let mut match_state =
            BountyMatchKernel::new(1, 0, SimulationTick::new(0), vec![player(1), player(2)])
                .unwrap();
        let output = match_state
            .advance_tick(SimulationTick::new(OBJECTIVE_CADENCE_TICKS))
            .unwrap();
        let objective = output.objective_started.unwrap();
        let award = match_state
            .complete_objective(
                SimulationTick::new(OBJECTIVE_CADENCE_TICKS),
                player(1),
                objective.objective_id,
            )
            .unwrap();
        assert_eq!(award.points, objective.kind.completion_points());

        match_state
            .advance_tick(SimulationTick::new(4 * OBJECTIVE_CADENCE_TICKS))
            .unwrap();
        let tower = match_state.active_objective().unwrap();
        assert_eq!(tower.kind, DynamicObjectiveKind::SignalTower);
        for intervals in 1..=4 {
            let award = match_state
                .record_signal_tower_hold(
                    SimulationTick::new(4 * OBJECTIVE_CADENCE_TICKS),
                    player(2),
                    tower.objective_id,
                    intervals * SIGNAL_TOWER_INTERVAL_TICKS,
                )
                .unwrap();
            assert_eq!(award.is_some(), intervals <= 3);
        }
        assert_eq!(match_state.player(player(2)).unwrap().score.objective, 150);
    }

    #[test]
    fn match_end_and_checkpoint_epoch_are_fail_closed() {
        let mut match_state = kernel();
        match_state.advance_tick(SimulationTick::new(10)).unwrap();
        match_state
            .record_horse_bolt(SimulationTick::new(10), player(2), player(1))
            .unwrap();
        let checkpoint = match_state.checkpoint();
        let mut inconsistent_score = checkpoint.clone();
        inconsistent_score
            .match_state
            .players
            .get_mut(&player(1))
            .unwrap()
            .score
            .mounted_long_hit = 10;
        assert_eq!(
            BountyMatchKernel::restore_checkpoint(inconsistent_score, 5),
            Err(BountyMatchError::InvalidCheckpoint)
        );
        let encoded = serde_json::to_vec(&checkpoint).unwrap();
        let decoded = serde_json::from_slice(&encoded).unwrap();
        let mut restored = BountyMatchKernel::restore_checkpoint(decoded, 5).unwrap();
        assert_eq!(restored.player(player(2)).unwrap().total_score(), 15);
        assert_eq!(
            BountyMatchKernel::restore_checkpoint(checkpoint, 6),
            Err(BountyMatchError::InvalidCheckpoint)
        );

        let output = restored
            .advance_tick(SimulationTick::new(BOUNTY_RUN_DURATION_TICKS))
            .unwrap();
        assert!(output.match_ended);
        assert_eq!(output.winner, Some(player(2)));
        assert_eq!(
            restored.record_horse_bolt(
                SimulationTick::new(BOUNTY_RUN_DURATION_TICKS),
                player(1),
                player(2)
            ),
            Err(BountyMatchError::MatchFinished)
        );

        assert_eq!(
            BountyMatchKernel::new(1, 0, SimulationTick::new(0), vec![player(1), player(1)]),
            Err(BountyMatchError::InvalidRoster)
        );
    }
}
