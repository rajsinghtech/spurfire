//! Transactional composition of M1 rifle authority and M3 horse vitality.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ActorGameplayKernel, ActorM3TickInput, ActorM3TickOutput, AuthorityShot, CombatAuthority,
    EntityId, HorseDamageCommand, HorseDamageId, HorseTargetPoseSnapshot, HorseVitalityClass,
    HorseVitalityState, M3AuthorityBank, M3AuthorityError, M3AuthorityHorseDamage, PlayerId,
    QuantizedDirection, QuantizedOrigin, RiderSnapshot, ShotCommand, ShotOutcome, SimulationTick,
    TargetDefinition, TargetPoseSnapshot, TargetRegistry, TargetRegistryError, TeamId, WeaponId,
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
            next_horse_damage_sequence: 1,
        })
    }

    /// Reassembles a migrated authority only when combat, rider targets,
    /// horse targets, actor ownership, health, and epochs form one exact graph.
    pub fn restore_components(
        combat: CombatAuthority,
        targets: TargetRegistry,
        actors: M3AuthorityBank,
        rider_entities: BTreeMap<PlayerId, EntityId>,
        next_horse_damage_sequence: u64,
    ) -> Result<Self, M3CombatAuthorityError> {
        let actor_rows = actors.checkpoint();
        if next_horse_damage_sequence == 0
            || combat.authority_epoch() != actors.authority_epoch()
            || actor_rows.actors().len() != rider_entities.len()
            || combat.shooter_count() != actor_rows.actors().len()
            || targets.len() != actor_rows.actors().len().saturating_mul(2)
        {
            return Err(M3CombatAuthorityError::CrossKernelInvariant);
        }
        let mut seen_entities = BTreeMap::new();
        for row in actor_rows.actors() {
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
            {
                return Err(M3CombatAuthorityError::CrossKernelInvariant);
            }
        }
        Ok(Self {
            combat,
            targets,
            actors,
            rider_entities,
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

    /// Immutable actor state for one roster member.
    #[must_use]
    pub fn actor(&self, rider_player_id: PlayerId) -> Option<&ActorGameplayKernel> {
        self.actors.actor(rider_player_id)
    }

    /// Advances one actor and synchronizes horse regeneration/remount health to
    /// the rewind registry as one transaction.
    pub fn advance_actor(
        &mut self,
        rider_player_id: PlayerId,
        input: ActorM3TickInput,
    ) -> Result<ActorM3TickOutput, M3CombatAuthorityError> {
        let mut candidate = self.clone();
        let output = candidate.actors.advance_actor(rider_player_id, input)?;
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
        *self = candidate;
        Ok(M3AuthorityShot { shot, horse_damage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OnFootTickInput, QuantizedDirection, RecallState, RidingState, DIRECTION_UNITS};

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
            rider_position: QuantizedOrigin::default(),
            return_horse_position: QuantizedOrigin::default(),
            return_horse_moving: false,
        }
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
    fn migrated_components_require_one_exact_combat_actor_target_graph() {
        let authority = authority();
        let restored = M3CombatAuthority::restore_components(
            authority.combat.clone(),
            authority.targets.clone(),
            authority.actors.clone(),
            authority.rider_entities.clone(),
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
                authority.next_horse_damage_sequence,
            ),
            Err(M3CombatAuthorityError::CrossKernelInvariant)
        );
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
