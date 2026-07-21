//! Transactional composition of M1 rifle authority and M3 horse vitality.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ActorGameplayKernel, ActorM3TickInput, ActorM3TickOutput, AuthorityShot, CombatAuthority,
    EntityId, HorseDamageCommand, HorseDamageId, HorseTargetPoseSnapshot, HorseVitalityClass,
    HorseVitalityState, M3AuthorityBank, M3AuthorityError, M3AuthorityHorseDamage, PlayerId,
    QuantizedDirection, QuantizedOrigin, ReloadSnapshot, RiderSnapshot, RiderStance, ShotCommand,
    ShotOutcome, SimulationTick, SpurCreditKind, TargetDefinition, TargetPoseSnapshot,
    TargetRegistry, TargetRegistryError, TeamId, WeaponId,
};

/// Prototype rider health registered alongside each M3 horse target.
pub const M3_RIDER_MAX_HEALTH: u16 = 100;
/// Locked initial horse body capsule inner half-length.
pub const M3_HORSE_BODY_HALF_LENGTH_MM: u16 = 900;
/// Locked initial horse body capsule radius.
pub const M3_HORSE_BODY_RADIUS_MM: u16 = 650;
/// Locked initial horse head hit sphere radius.
pub const M3_HORSE_HEAD_RADIUS_MM: u16 = 350;

/// Authority-observed horse geometry for one rewind tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3HorseTargetPose {
    /// Exact simulation tick represented by the geometry.
    pub tick: SimulationTick,
    /// Roster actor owning the horse.
    pub rider_player_id: PlayerId,
    /// Center of the horse body capsule.
    pub body_center: QuantizedOrigin,
    /// Normalized planar nose-forward direction.
    pub body_forward: QuantizedDirection,
    /// Center of the horse head sphere.
    pub head_center: QuantizedOrigin,
}

/// Accepted/rejected rifle resolution plus optional routed horse effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3AuthorityShot {
    /// Existing deterministic rifle authority result and telemetry.
    pub shot: AuthorityShot,
    /// Present exactly when an accepted hit damaged a registered horse.
    pub horse_damage: Option<M3AuthorityHorseDamage>,
}

/// Authority-owned reload state bound into a wire-v2 migration checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3ReloadCheckpointV2 {
    /// Roster shooter owning this clock.
    #[serde(rename = "p", alias = "rider_player_id")]
    pub rider_player_id: PlayerId,
    /// Last actor tick applied to reload progress.
    #[serde(
        default,
        rename = "t",
        alias = "current_tick",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_tick: Option<SimulationTick>,
    /// Previous R-button level for deterministic rising-edge admission.
    #[serde(rename = "h", alias = "reload_held")]
    pub reload_held: bool,
    /// Retained progress, including a roll-paused reload.
    #[serde(
        default,
        rename = "r",
        alias = "reload",
        skip_serializing_if = "Option::is_none"
    )]
    pub reload: Option<ReloadSnapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct M3ReloadClock {
    current_tick: Option<SimulationTick>,
    reload_held: bool,
}

/// Fail-closed composed M3 combat operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum M3CombatAuthorityError {
    /// Tick rate was zero.
    #[error("invalid_tick_rate")]
    InvalidTickRate,
    /// Roster actor or target identity was not registered.
    #[error("unknown_actor_or_target")]
    UnknownActorOrTarget,
    /// Actor bank rejected damage or tick mutation.
    #[error("actor_state: {0}")]
    ActorState(#[from] M3AuthorityError),
    /// Target history/health rejected a mutation.
    #[error("target_state: {0}")]
    TargetState(#[from] TargetRegistryError),
    /// Combat and M3 health/effect state did not agree; nothing committed.
    #[error("cross_kernel_invariant")]
    CrossKernelInvariant,
    /// Damage receipt sequence was exhausted.
    #[error("damage_sequence_exhausted")]
    DamageSequenceExhausted,
}

/// One composed elected-authority owner for rifles, rider targets, horse
/// targets, and M3 actor state.
///
/// Every public multi-kernel mutation prepares a clone and commits only after
/// target health, actor vitality, fatal-spook semantics, and replay state agree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3CombatAuthority {
    combat: CombatAuthority,
    targets: TargetRegistry,
    actors: M3AuthorityBank,
    rider_entities: BTreeMap<PlayerId, EntityId>,
    reload_clocks: BTreeMap<PlayerId, M3ReloadClock>,
    next_horse_damage_sequence: u64,
}

impl M3CombatAuthority {
    /// Creates an empty composed authority for one epoch.
    pub fn new(
        tick_rate: u32,
        lobby_seed: u64,
        authority_epoch: u64,
    ) -> Result<Self, M3CombatAuthorityError> {
        let mut combat = CombatAuthority::new(tick_rate, lobby_seed)
            .map_err(|_| M3CombatAuthorityError::InvalidTickRate)?;
        if !combat.set_authority_epoch(authority_epoch) {
            return Err(M3CombatAuthorityError::CrossKernelInvariant);
        }
        let targets =
            TargetRegistry::new(tick_rate).map_err(|_| M3CombatAuthorityError::InvalidTickRate)?;
        Ok(Self {
            combat,
            targets,
            actors: M3AuthorityBank::new(authority_epoch),
            rider_entities: BTreeMap::new(),
            reload_clocks: BTreeMap::new(),
            next_horse_damage_sequence: 1,
        })
    }

    /// Reassembles a migrated authority only when combat, rider targets,
    /// horse targets, actor ownership, health, and epochs form one exact graph.
    pub fn restore_components(
        mut combat: CombatAuthority,
        targets: TargetRegistry,
        actors: M3AuthorityBank,
        rider_entities: BTreeMap<PlayerId, EntityId>,
        reloads: Vec<M3ReloadCheckpointV2>,
        next_horse_damage_sequence: u64,
    ) -> Result<Self, M3CombatAuthorityError> {
        let actor_rows = actors.checkpoint();
        if next_horse_damage_sequence == 0
            || combat.authority_epoch() != actors.authority_epoch()
            || actor_rows.actors().len() != rider_entities.len()
            || actor_rows.actors().len() != reloads.len()
            || combat.shooter_count() != actor_rows.actors().len()
            || targets.len() != actor_rows.actors().len().saturating_mul(2)
        {
            return Err(M3CombatAuthorityError::CrossKernelInvariant);
        }
        let mut seen_entities = BTreeMap::new();
        let mut reload_clocks = BTreeMap::new();
        for (row, reload) in actor_rows.actors().iter().zip(reloads) {
            let player = row.rider_player_id;
            let Some(rider_entity) = rider_entities.get(&player).copied() else {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            };
            let horse_entity = row.horse_entity_id;
            let Some(actor) = actors.actor(player) else {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            };
            let Some(rider_definition) = targets.definition(rider_entity) else {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            };
            let Some(horse_definition) = targets.definition(horse_entity) else {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            };
            if rider_entity == horse_entity
                || seen_entities.insert(rider_entity, player).is_some()
                || seen_entities.insert(horse_entity, player).is_some()
                || combat.shooter_kernel(player).is_none()
                || rider_definition.owner_peer_id != Some(player)
                || rider_definition.max_health != M3_RIDER_MAX_HEALTH
                || horse_definition.owner_peer_id != Some(player)
                || horse_definition.team_id != rider_definition.team_id
                || horse_definition.max_health != actor.horse().max_health()
                || targets.health(horse_entity) != Some(actor.horse().health())
                || reload.rider_player_id != player
                || actor.current_tick() != reload.current_tick
            {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            }
            let Some(kernel) = combat.shooter_kernel_mut(player) else {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            };
            if !kernel.restore_m3_reload(reload.reload) {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            }
            reload_clocks.insert(
                player,
                M3ReloadClock {
                    current_tick: reload.current_tick,
                    reload_held: reload.reload_held,
                },
            );
        }
        Ok(Self {
            combat,
            targets,
            actors,
            rider_entities,
            reload_clocks,
            next_horse_damage_sequence,
        })
    }

    /// Registers one shooter, rider target, horse target, and actor atomically.
    #[allow(clippy::too_many_arguments)]
    pub fn register_actor(
        &mut self,
        rider_player_id: PlayerId,
        rider_entity_id: EntityId,
        horse_entity_id: EntityId,
        horse_class: HorseVitalityClass,
        weapon_id: WeaponId,
        target_team: TeamId,
    ) -> bool {
        if rider_entity_id == horse_entity_id || self.rider_entities.contains_key(&rider_player_id)
        {
            return false;
        }
        let mut candidate = self.clone();
        if !candidate
            .actors
            .register_actor(rider_player_id, horse_entity_id, horse_class)
            || !candidate
                .combat
                .register_shooter(rider_player_id, weapon_id)
            || candidate
                .targets
                .register(TargetDefinition {
                    entity_id: rider_entity_id,
                    owner_peer_id: Some(rider_player_id),
                    team_id: target_team,
                    max_health: M3_RIDER_MAX_HEALTH,
                })
                .is_err()
            || candidate
                .targets
                .register(TargetDefinition {
                    entity_id: horse_entity_id,
                    owner_peer_id: Some(rider_player_id),
                    team_id: target_team,
                    max_health: horse_class.max_health(),
                })
                .is_err()
        {
            return false;
        }
        candidate
            .rider_entities
            .insert(rider_player_id, rider_entity_id);
        candidate
            .reload_clocks
            .insert(rider_player_id, M3ReloadClock::default());
        *self = candidate;
        true
    }

    /// Immutable rifle authority for checkpoint/snapshot inspection.
    #[must_use]
    pub const fn combat(&self) -> &CombatAuthority {
        &self.combat
    }

    /// Immutable target registry for health/snapshot inspection.
    #[must_use]
    pub const fn targets(&self) -> &TargetRegistry {
        &self.targets
    }

    /// Immutable M3 actor bank for HUD/checkpoint inspection.
    #[must_use]
    pub const fn actors(&self) -> &M3AuthorityBank {
        &self.actors
    }

    /// Next authority-global horse damage receipt sequence for checkpoints.
    #[must_use]
    pub const fn next_horse_damage_sequence(&self) -> u64 {
        self.next_horse_damage_sequence
    }

    /// Current retained reload state for HUD and authority tests.
    #[must_use]
    pub fn reload(&self, rider_player_id: PlayerId) -> Option<ReloadSnapshot> {
        self.combat
            .shooter_kernel(rider_player_id)
            .and_then(crate::CombatKernel::reload)
    }

    /// Canonical player-sorted reload rows for the combined wire-v2 handoff.
    #[must_use]
    pub fn reload_checkpoints(&self) -> Vec<M3ReloadCheckpointV2> {
        self.actors
            .checkpoint()
            .actors()
            .iter()
            .map(|row| {
                let clock = self
                    .reload_clocks
                    .get(&row.rider_player_id)
                    .copied()
                    .unwrap_or_default();
                M3ReloadCheckpointV2 {
                    rider_player_id: row.rider_player_id,
                    current_tick: clock.current_tick,
                    reload_held: clock.reload_held,
                    reload: self.reload(row.rider_player_id),
                }
            })
            .collect()
    }

    /// Immutable actor state for one roster member.
    #[must_use]
    pub fn actor(&self, rider_player_id: PlayerId) -> Option<&ActorGameplayKernel> {
        self.actors.actor(rider_player_id)
    }

    /// Roster rider owning one registered rider target entity.
    #[must_use]
    pub fn rider_owner(&self, entity_id: EntityId) -> Option<PlayerId> {
        self.rider_entities
            .iter()
            .find_map(|(player, rider)| (*rider == entity_id).then_some(*player))
    }

    /// Advances one actor and synchronizes horse regeneration/remount health to
    /// the rewind registry as one transaction.
    pub fn advance_actor(
        &mut self,
        rider_player_id: PlayerId,
        mut input: ActorM3TickInput,
    ) -> Result<ActorM3TickOutput, M3CombatAuthorityError> {
        let mut candidate = self.clone();
        let reload_pressed = input.on_foot.reload_active;
        let reload_was_active = candidate.reload(rider_player_id).is_some();
        input.on_foot.reload_active = reload_was_active;
        let output = candidate.actors.advance_actor(rider_player_id, input)?;
        let clock = candidate
            .reload_clocks
            .get(&rider_player_id)
            .copied()
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
        let elapsed = clock.current_tick.map_or(0, |tick| {
            input.tick.checked_duration_since(tick).unwrap_or(0)
        });
        let on_foot = output.on_foot;
        let paused = on_foot.is_some_and(|state| !state.can_fire);
        let reload_outcome = candidate
            .combat
            .shooter_kernel_mut(rider_player_id)
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?
            .advance_m3_reload(elapsed, paused);
        let reload_edge = reload_pressed && !clock.reload_held;
        if reload_edge && !paused && !reload_outcome.reload_completed {
            let kernel = candidate
                .combat
                .shooter_kernel_mut(rider_player_id)
                .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
            if kernel.reload().is_none() {
                let _ = kernel.request_m3_reload();
            }
        }
        candidate.reload_clocks.insert(
            rider_player_id,
            M3ReloadClock {
                current_tick: Some(input.tick),
                reload_held: reload_pressed,
            },
        );
        let horse_entity_id = candidate
            .actors
            .horse_entity_id(rider_player_id)
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
        let health = candidate
            .actors
            .actor(rider_player_id)
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?
            .horse()
            .health();
        candidate
            .targets
            .synchronize_health(horse_entity_id, health)?;
        *self = candidate;
        Ok(output)
    }

    /// Issues one validated M4 award through the composed authority owner.
    pub fn issue_spur_credit(
        &mut self,
        rider_player_id: PlayerId,
        tick: SimulationTick,
        kind: SpurCreditKind,
    ) -> Result<Option<u8>, M3CombatAuthorityError> {
        self.actors
            .issue_spur_credit(rider_player_id, tick, kind)
            .map_err(Into::into)
    }

    /// Restores one eliminated rider to full health on the M5 respawn edge.
    pub fn respawn_rider(
        &mut self,
        rider_player_id: PlayerId,
    ) -> Result<(), M3CombatAuthorityError> {
        let rider_entity = self
            .rider_entities
            .get(&rider_player_id)
            .copied()
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
        let mut candidate = self.clone();
        candidate
            .targets
            .synchronize_health(rider_entity, M3_RIDER_MAX_HEALTH)?;
        *self = candidate;
        Ok(())
    }

    /// Records locked horse hit geometry for rewind. Hittability derives from
    /// authority vitality; callers cannot keep a spooked/despawned horse active.
    pub fn record_horse_pose(
        &mut self,
        pose: M3HorseTargetPose,
    ) -> Result<(), M3CombatAuthorityError> {
        let horse_entity_id = self
            .actors
            .horse_entity_id(pose.rider_player_id)
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
        let actor = self
            .actors
            .actor(pose.rider_player_id)
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
        if actor.current_tick() != Some(pose.tick) {
            return Err(M3CombatAuthorityError::CrossKernelInvariant);
        }
        self.targets.record_horse_pose(HorseTargetPoseSnapshot {
            tick: pose.tick,
            entity_id: horse_entity_id,
            body_center: pose.body_center,
            body_forward: pose.body_forward,
            body_half_length_mm: M3_HORSE_BODY_HALF_LENGTH_MM,
            body_radius_mm: M3_HORSE_BODY_RADIUS_MM,
            head_center: pose.head_center,
            head_radius_mm: M3_HORSE_HEAD_RADIUS_MM,
            active: actor.horse().state() == HorseVitalityState::Available
                && actor.horse().health() > 0,
        })?;
        Ok(())
    }

    /// Records one rider target pose through the same bounded rewind registry.
    pub fn record_rider_pose(
        &mut self,
        rider_player_id: PlayerId,
        pose: TargetPoseSnapshot,
    ) -> Result<(), M3CombatAuthorityError> {
        let expected = self
            .rider_entities
            .get(&rider_player_id)
            .copied()
            .ok_or(M3CombatAuthorityError::UnknownActorOrTarget)?;
        if pose.entity_id != expected {
            return Err(M3CombatAuthorityError::UnknownActorOrTarget);
        }
        if self
            .actors
            .actor(rider_player_id)
            .is_none_or(|actor| actor.current_tick() != Some(pose.tick))
        {
            return Err(M3CombatAuthorityError::CrossKernelInvariant);
        }
        self.targets.record_pose(pose)?;
        Ok(())
    }

    /// Resolves one rifle command and routes horse damage transactionally.
    /// `authority_tick` is the damage/spook time; `command.tick` remains the
    /// historical rewind time.
    pub fn resolve_shot(
        &mut self,
        command: &ShotCommand,
        authority_tick: SimulationTick,
        rider: RiderSnapshot,
    ) -> Result<M3AuthorityShot, M3CombatAuthorityError> {
        let mut candidate = self.clone();
        let mut shot =
            candidate
                .combat
                .validate_shot(command, authority_tick, rider, &mut candidate.targets);
        let mut horse_damage = None;
        if shot.result.outcome == ShotOutcome::Hit {
            if let Some(target_id) = shot.result.target_id {
                if candidate.actors.horse_owner(target_id).is_some() {
                    let pose = candidate
                        .targets
                        .horse_pose_at(target_id, command.tick)
                        .ok_or(M3CombatAuthorityError::CrossKernelInvariant)?;
                    let sequence = candidate.next_horse_damage_sequence;
                    candidate.next_horse_damage_sequence = sequence
                        .checked_add(1)
                        .ok_or(M3CombatAuthorityError::DamageSequenceExhausted)?;
                    let routed = candidate
                        .actors
                        .apply_horse_damage(
                            target_id,
                            HorseDamageCommand {
                                id: HorseDamageId {
                                    authority_epoch: candidate.actors.authority_epoch(),
                                    tick: authority_tick,
                                    sequence,
                                },
                                amount: shot.result.damage,
                                horse_position: pose.body_center,
                                damage_source_position: rider.muzzle_origin,
                            },
                        )?
                        .ok_or(M3CombatAuthorityError::CrossKernelInvariant)?;
                    let actor_health = candidate
                        .actors
                        .actor(routed.rider_player_id)
                        .ok_or(M3CombatAuthorityError::CrossKernelInvariant)?
                        .horse()
                        .health();
                    let target_health = candidate
                        .targets
                        .health(target_id)
                        .ok_or(M3CombatAuthorityError::CrossKernelInvariant)?;
                    if actor_health != target_health
                        || routed.effects.application.health_after != target_health
                        || routed.effects.application.spooked != shot.result.eliminated
                    {
                        return Err(M3CombatAuthorityError::CrossKernelInvariant);
                    }
                    // A depleted horse spooks; it is never a rider elimination.
                    shot.result.eliminated = false;
                    horse_damage = Some(routed);
                }
            }
        }
        let spur_kind = match (
            rider.riding.stance,
            shot.result.outcome,
            shot.result.eliminated,
        ) {
            (RiderStance::SaddleDiveAirborne, ShotOutcome::Hit, true) => {
                Some(SpurCreditKind::SaddleDiveElimination)
            }
            (RiderStance::Mounted, ShotOutcome::Hit, true) => {
                Some(SpurCreditKind::MountedElimination)
            }
            (RiderStance::Mounted, ShotOutcome::Hit, false) => Some(SpurCreditKind::MountedHit),
            _ => None,
        };
        if let Some(kind) = spur_kind {
            candidate
                .actors
                .issue_spur_credit(command.shooter_peer_id, authority_tick, kind)?
                .ok_or(M3CombatAuthorityError::CrossKernelInvariant)?;
        }
        *self = candidate;
        Ok(M3AuthorityShot { shot, horse_damage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        OnFootTickInput, QuantizedDirection, RecallState, RidingState, WeaponAmmo, DIRECTION_UNITS,
    };

    fn player(number: u64) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{number:012x}")).unwrap()
    }

    fn actor_input(tick: u64) -> ActorM3TickInput {
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
            interact_pressed: false,
            spur_pressed: false,
            mounted_for_spur: true,
            rider_position: QuantizedOrigin::default(),
            return_horse_position: QuantizedOrigin::default(),
            return_horse_moving: false,
        }
    }

    fn on_foot_input(tick: u64, sprint: bool, crouch: bool, reload: bool) -> ActorM3TickInput {
        let mut input = actor_input(tick);
        input.on_foot.move_direction = Some(QuantizedDirection::new(0, 0, -DIRECTION_UNITS));
        input.on_foot.sprint_pressed = sprint;
        input.on_foot.crouch_pressed = crouch;
        input.on_foot.reload_active = reload;
        input
    }

    fn authority() -> M3CombatAuthority {
        let mut authority = M3CombatAuthority::new(60, 0x1234, 7).unwrap();
        assert!(authority.register_actor(
            player(1),
            EntityId(101),
            EntityId(201),
            HorseVitalityClass::Courser,
            WeaponId::Dustwalker,
            TeamId(1),
        ));
        assert!(authority.register_actor(
            player(2),
            EntityId(102),
            EntityId(202),
            HorseVitalityClass::Warhorse,
            WeaponId::Dustwalker,
            TeamId(1),
        ));
        authority
    }

    fn record_target_horse(authority: &mut M3CombatAuthority, tick: u64) {
        authority
            .record_horse_pose(M3HorseTargetPose {
                tick: SimulationTick::new(tick),
                rider_player_id: player(2),
                body_center: QuantizedOrigin::new(0, 1_000, -10_000),
                body_forward: QuantizedDirection::new(0, 0, -DIRECTION_UNITS),
                head_center: QuantizedOrigin::new(0, 2_500, -10_500),
            })
            .unwrap();
    }

    fn shot_command(authority: &M3CombatAuthority, tick: u64) -> ShotCommand {
        ShotCommand {
            tick: SimulationTick::new(tick),
            shooter_peer_id: player(1),
            weapon_id: WeaponId::Dustwalker,
            origin: QuantizedOrigin::new(0, 1_000, 0),
            direction: QuantizedDirection::new(0, 0, -DIRECTION_UNITS),
            spread_seed: authority.combat().expected_spread_seed(player(1)).unwrap(),
            claimed_target: None,
        }
    }

    fn rider_snapshot(tick: u64) -> RiderSnapshot {
        RiderSnapshot {
            tick: SimulationTick::new(tick),
            shooter_peer_id: player(1),
            muzzle_origin: QuantizedOrigin::new(0, 1_000, 0),
            team_id: TeamId(0),
            riding: RidingState::default(),
        }
    }

    #[test]
    fn registration_is_atomic_across_shooter_rider_horse_and_actor() {
        let mut authority = authority();
        let before = authority.clone();
        assert!(!authority.register_actor(
            player(3),
            EntityId(103),
            EntityId(202),
            HorseVitalityClass::Mustang,
            WeaponId::Longspur,
            TeamId(1),
        ));
        assert_eq!(authority, before);
        assert!(!authority.register_actor(
            player(1),
            EntityId(301),
            EntityId(401),
            HorseVitalityClass::Mustang,
            WeaponId::Longspur,
            TeamId(1),
        ));
        assert_eq!(authority, before);
    }

    #[test]
    fn m5_respawn_restores_only_the_rostered_rider_target() {
        let mut authority = authority();
        authority
            .targets
            .synchronize_health(EntityId(102), 0)
            .unwrap();
        authority.respawn_rider(player(2)).unwrap();
        assert_eq!(
            authority.targets.health(EntityId(102)),
            Some(M3_RIDER_MAX_HEALTH)
        );
        assert_eq!(authority.targets.health(EntityId(202)), Some(320));

        let before = authority.clone();
        assert_eq!(
            authority.respawn_rider(player(3)),
            Err(M3CombatAuthorityError::UnknownActorOrTarget)
        );
        assert_eq!(authority, before);
    }

    #[test]
    fn migrated_components_require_one_exact_combat_actor_target_graph() {
        let authority = authority();
        let restored = M3CombatAuthority::restore_components(
            authority.combat.clone(),
            authority.targets.clone(),
            authority.actors.clone(),
            authority.rider_entities.clone(),
            authority.reload_checkpoints(),
            authority.next_horse_damage_sequence,
        )
        .unwrap();
        assert_eq!(restored, authority);

        let mut wrong_health = authority.targets.clone();
        wrong_health.synchronize_health(EntityId(202), 1).unwrap();
        assert_eq!(
            M3CombatAuthority::restore_components(
                authority.combat.clone(),
                wrong_health,
                authority.actors.clone(),
                authority.rider_entities.clone(),
                authority.reload_checkpoints(),
                authority.next_horse_damage_sequence,
            ),
            Err(M3CombatAuthorityError::CrossKernelInvariant)
        );

        let mut missing_rider = authority.rider_entities.clone();
        missing_rider.remove(&player(2));
        assert_eq!(
            M3CombatAuthority::restore_components(
                authority.combat.clone(),
                authority.targets.clone(),
                authority.actors.clone(),
                missing_rider,
                authority.reload_checkpoints(),
                authority.next_horse_damage_sequence,
            ),
            Err(M3CombatAuthorityError::CrossKernelInvariant)
        );
    }

    #[test]
    fn roll_pauses_reload_and_migration_retains_exact_progress() {
        let mut authority = authority();
        assert!(authority
            .combat
            .shooter_kernel_mut(player(1))
            .unwrap()
            .set_ammo(
                WeaponId::Dustwalker,
                WeaponAmmo {
                    magazine: 5,
                    reserve: 40,
                },
            ));
        authority.advance_actor(player(1), actor_input(1)).unwrap();
        let fatal = authority
            .actors
            .apply_horse_damage(
                EntityId(201),
                HorseDamageCommand {
                    id: HorseDamageId {
                        authority_epoch: 7,
                        tick: SimulationTick::new(1),
                        sequence: 1,
                    },
                    amount: 200,
                    horse_position: QuantizedOrigin::default(),
                    damage_source_position: QuantizedOrigin::new(1_000, 0, 0),
                },
            )
            .unwrap()
            .unwrap();
        authority
            .targets
            .synchronize_health(EntityId(201), fatal.effects.application.health_after)
            .unwrap();

        authority
            .advance_actor(player(1), on_foot_input(37, false, false, false))
            .unwrap();
        authority
            .advance_actor(player(1), on_foot_input(38, false, false, true))
            .unwrap();
        authority
            .advance_actor(player(1), on_foot_input(48, false, false, false))
            .unwrap();
        assert_eq!(authority.reload(player(1)).unwrap().active_ticks, 10);

        let roll = authority
            .advance_actor(player(1), on_foot_input(49, true, true, false))
            .unwrap();
        assert!(roll.on_foot.unwrap().reload_pause_started);
        authority
            .advance_actor(player(1), on_foot_input(60, false, false, false))
            .unwrap();
        assert_eq!(authority.reload(player(1)).unwrap().active_ticks, 10);

        let reloads = authority.reload_checkpoints();
        let restored = M3CombatAuthority::restore_components(
            authority.combat.clone(),
            authority.targets.clone(),
            authority.actors.clone(),
            authority.rider_entities.clone(),
            reloads.clone(),
            authority.next_horse_damage_sequence,
        )
        .unwrap();
        assert_eq!(restored.reload(player(1)).unwrap().active_ticks, 10);

        let mut forged = reloads;
        let row = forged
            .iter_mut()
            .find(|row| row.rider_player_id == player(1))
            .unwrap();
        let required_ticks = row.reload.unwrap().required_ticks;
        row.reload.as_mut().unwrap().active_ticks = required_ticks;
        assert_eq!(
            M3CombatAuthority::restore_components(
                authority.combat.clone(),
                authority.targets.clone(),
                authority.actors.clone(),
                authority.rider_entities.clone(),
                forged,
                authority.next_horse_damage_sequence,
            ),
            Err(M3CombatAuthorityError::CrossKernelInvariant)
        );

        for tick in 61..=79 {
            authority
                .advance_actor(player(1), on_foot_input(tick, false, false, false))
                .unwrap();
        }
        assert_eq!(authority.reload(player(1)).unwrap().active_ticks, 11);
    }

    #[test]
    fn horse_hit_commits_ammo_target_health_and_actor_vitality_once() {
        let mut authority = authority();
        authority.advance_actor(player(1), actor_input(10)).unwrap();
        authority.advance_actor(player(2), actor_input(10)).unwrap();
        record_target_horse(&mut authority, 10);
        let command = shot_command(&authority, 10);
        let ammo_before = authority
            .combat()
            .shooter_kernel(player(1))
            .unwrap()
            .equipped_ammo()
            .magazine;
        let health_before = authority.actor(player(2)).unwrap().horse().health();

        let resolved = authority
            .resolve_shot(&command, SimulationTick::new(10), rider_snapshot(10))
            .unwrap();
        assert_eq!(resolved.shot.result.outcome, ShotOutcome::Hit);
        assert_eq!(resolved.shot.result.target_id, Some(EntityId(202)));
        assert!(!resolved.shot.result.eliminated);
        let routed = resolved.horse_damage.unwrap();
        assert_eq!(routed.rider_player_id, player(2));
        assert_eq!(routed.effects.application.health_before, health_before);
        assert_eq!(
            authority.actor(player(2)).unwrap().horse().health(),
            authority.targets().health(EntityId(202)).unwrap()
        );
        assert_eq!(
            authority
                .combat()
                .shooter_kernel(player(1))
                .unwrap()
                .equipped_ammo()
                .magazine,
            ammo_before - 1
        );

        let health_after = authority.actor(player(2)).unwrap().horse().health();
        let duplicate = authority
            .resolve_shot(&command, SimulationTick::new(10), rider_snapshot(10))
            .unwrap();
        assert_eq!(duplicate.shot.result.outcome, ShotOutcome::Reject);
        assert!(duplicate.horse_damage.is_none());
        assert_eq!(
            authority.actor(player(2)).unwrap().horse().health(),
            health_after
        );
    }

    #[test]
    fn fatal_horse_hit_spooks_without_reporting_a_rider_elimination() {
        let mut authority = authority();
        let mut fatal = None;
        for index in 0..30_u64 {
            let tick = 10 + index * 10;
            authority
                .advance_actor(player(1), actor_input(tick))
                .unwrap();
            authority
                .advance_actor(player(2), actor_input(tick))
                .unwrap();
            record_target_horse(&mut authority, tick);
            let command = shot_command(&authority, tick);
            let resolved = authority
                .resolve_shot(&command, SimulationTick::new(tick), rider_snapshot(tick))
                .unwrap();
            assert_eq!(resolved.shot.result.outcome, ShotOutcome::Hit);
            if resolved
                .horse_damage
                .is_some_and(|damage| damage.effects.application.spooked)
            {
                fatal = Some(resolved);
                break;
            }
        }
        let fatal = fatal.expect("a full Dustwalker magazine must spook a Warhorse");
        assert!(!fatal.shot.result.eliminated);
        assert!(fatal.horse_damage.unwrap().effects.horse_bolted.is_some());
        assert_eq!(
            authority.actor(player(2)).unwrap().horse().state(),
            HorseVitalityState::Bolting
        );
        assert_eq!(authority.targets().health(EntityId(202)), Some(0));

        let next_tick = fatal.shot.result.tick.as_u64() + 10;
        authority
            .advance_actor(player(1), actor_input(next_tick))
            .unwrap();
        authority
            .advance_actor(player(2), actor_input(next_tick))
            .unwrap();
        record_target_horse(&mut authority, next_tick);
        let command = shot_command(&authority, next_tick);
        let after_spook = authority
            .resolve_shot(
                &command,
                SimulationTick::new(next_tick),
                rider_snapshot(next_tick),
            )
            .unwrap();
        assert_eq!(after_spook.shot.result.outcome, ShotOutcome::Miss);
        assert!(after_spook.horse_damage.is_none());
    }

    #[test]
    fn actor_tick_mismatch_rolls_back_target_damage_ammo_and_receipts() {
        let mut authority = authority();
        authority.advance_actor(player(1), actor_input(10)).unwrap();
        authority.advance_actor(player(2), actor_input(9)).unwrap();
        record_target_horse(&mut authority, 9);
        let command = shot_command(&authority, 9);
        let before = authority.clone();
        assert_eq!(
            authority.resolve_shot(&command, SimulationTick::new(10), rider_snapshot(9),),
            Err(M3CombatAuthorityError::CrossKernelInvariant)
        );
        assert_eq!(authority, before);

        authority.advance_actor(player(2), actor_input(10)).unwrap();
        assert!(authority
            .resolve_shot(&command, SimulationTick::new(10), rider_snapshot(9))
            .unwrap()
            .horse_damage
            .is_some());
    }

    #[test]
    fn actor_tick_health_sync_restores_regen_and_remount_values_to_targets() {
        let mut authority = authority();
        authority.advance_actor(player(1), actor_input(10)).unwrap();
        authority.advance_actor(player(2), actor_input(10)).unwrap();
        record_target_horse(&mut authority, 10);
        let command = shot_command(&authority, 10);
        authority
            .resolve_shot(&command, SimulationTick::new(10), rider_snapshot(10))
            .unwrap();
        let damaged = authority.actor(player(2)).unwrap().horse().health();
        authority
            .advance_actor(player(2), actor_input(400))
            .unwrap();
        assert!(authority.actor(player(2)).unwrap().horse().health() > damaged);
        assert_eq!(
            authority.actor(player(2)).unwrap().horse().health(),
            authority.targets().health(EntityId(202)).unwrap()
        );
        assert_eq!(
            authority.actor(player(2)).unwrap().recall().state(),
            RecallState::HorsePresent
        );
    }
}
