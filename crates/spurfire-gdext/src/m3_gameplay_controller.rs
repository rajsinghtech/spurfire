//! Godot-facing adapter for the deterministic M3 authority actor bank.

use godot::classes::{INode, Node};
use godot::prelude::*;
use spurfire_protocol::{
    ActorM3Mode, ActorM3TickInput, EntityId, HorseDamageCommand, HorseDamageId, HorseVitalityClass,
    HorseVitalityState, M3AuthorityBank, M3AuthorityCheckpointV2, M3RiderStance, OnFootState,
    OnFootTickInput, PlayerId, QuantizedDirection, QuantizedOrigin, RecallCreditCommand,
    RecallCreditId, RecallCreditKind, RecallState, SimulationTick, DIRECTION_UNITS,
};

/// One native authority owner for M3 horse loss, on-foot play, and return.
/// Presentation consumes properties/signals; gameplay state remains in Rust.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct M3GameplayController {
    #[base]
    base: Base<Node>,

    #[var(no_set)]
    current_tick: i64,
    #[var(no_set)]
    authority_epoch: i64,
    #[var(no_set)]
    mode_id: i64,
    #[var(no_set)]
    horse_health: i64,
    #[var(no_set)]
    horse_max_health: i64,
    #[var(no_set)]
    horse_state_id: i64,
    #[var(no_set)]
    on_foot_state_id: i64,
    #[var(no_set)]
    stamina_ticks: i64,
    #[var(no_set)]
    recall_state_id: i64,

    bank: M3AuthorityBank,
    rider_player_id: Option<PlayerId>,
    horse_entity_id: Option<EntityId>,
}

#[godot_api]
impl M3GameplayController {
    #[signal]
    fn mode_changed(previous_id: i64, current_id: i64, tick: i64);

    #[signal]
    fn horse_damaged(health_before: i64, health_after: i64, tick: i64);

    #[signal]
    fn horse_bolted(tick: i64, throw_distance_m: f64, stun_seconds: f64);

    #[signal]
    fn horse_despawned(tick: i64);

    #[signal]
    fn remounted(tick: i64, running_mount: bool);

    /// Binds one local/proxy actor to an exact authority epoch and horse target.
    /// Archetype IDs match `HorseController`: 0 Courser, 1 Warhorse, 2 Mustang.
    #[func]
    fn configure(
        &mut self,
        authority_epoch: i64,
        rider_player_id: GString,
        horse_entity_id: i64,
        archetype_id: i64,
    ) -> bool {
        let (Ok(authority_epoch), Ok(rider_player_id), Ok(horse_entity_id), Some(class)) = (
            u64::try_from(authority_epoch),
            PlayerId::parse(&rider_player_id.to_string()),
            u64::try_from(horse_entity_id).map(EntityId),
            horse_class(archetype_id),
        ) else {
            return false;
        };
        let mut bank = M3AuthorityBank::new(authority_epoch);
        if !bank.register_actor(rider_player_id, horse_entity_id, class) {
            return false;
        }
        self.bank = bank;
        self.rider_player_id = Some(rider_player_id);
        self.horse_entity_id = Some(horse_entity_id);
        self.current_tick = -1;
        self.authority_epoch = i64::try_from(authority_epoch).unwrap_or(i64::MAX);
        self.refresh_properties();
        true
    }

    /// Advances one strict 60 Hz M3 actor tick and returns presentation output.
    #[func]
    #[allow(clippy::too_many_arguments)]
    fn advance_tick(
        &mut self,
        tick: i64,
        move_input: Vector2,
        sprint_pressed: bool,
        crouch_pressed: bool,
        reload_active: bool,
        interact_pressed: bool,
        rider_position: Vector3,
        return_horse_position: Vector3,
        return_horse_moving: bool,
    ) -> VarDictionary {
        let Some(rider_player_id) = self.rider_player_id else {
            return VarDictionary::new();
        };
        let (Ok(tick_value), Some(rider_position), Some(return_horse_position)) = (
            u64::try_from(tick),
            quantized_origin(rider_position),
            quantized_origin(return_horse_position),
        ) else {
            return VarDictionary::new();
        };
        let tick = SimulationTick::new(tick_value);
        let input = ActorM3TickInput {
            tick,
            on_foot: OnFootTickInput {
                tick,
                move_direction: quantized_move_input(move_input),
                sprint_pressed,
                crouch_pressed,
                reload_active,
            },
            interact_pressed,
            rider_position,
            return_horse_position,
            return_horse_moving,
        };
        let previous_mode = self.mode_id;
        let Ok(output) = self.bank.advance_actor(rider_player_id, input) else {
            return VarDictionary::new();
        };
        self.current_tick = i64::try_from(tick_value).unwrap_or(i64::MAX);
        self.refresh_properties();
        let current_mode = self.mode_id;
        let current_tick = self.current_tick;
        if previous_mode >= 0 && previous_mode != current_mode {
            self.signals()
                .mode_changed()
                .emit(previous_mode, current_mode, current_tick);
        }
        if output.horse_despawned {
            self.signals().horse_despawned().emit(current_tick);
        }
        if output.remounted {
            let running = self
                .bank
                .actor(rider_player_id)
                .and_then(|actor| actor.recall().telemetry())
                .is_some_and(|row| row.running_mount);
            self.signals().remounted().emit(current_tick, running);
        }
        actor_output_dictionary(output)
    }

    /// Applies one authority-owned horse damage aggregate at the current actor
    /// tick. Duplicate/stale commands return a non-mutating empty dictionary.
    #[func]
    fn apply_horse_damage(
        &mut self,
        tick: i64,
        sequence: i64,
        amount: i64,
        horse_position: Vector3,
        damage_source_position: Vector3,
    ) -> VarDictionary {
        let Some(horse_entity_id) = self.horse_entity_id else {
            return VarDictionary::new();
        };
        let (Ok(tick), Ok(sequence), Ok(amount), Some(horse_position), Some(source_position)) = (
            u64::try_from(tick),
            u64::try_from(sequence),
            u16::try_from(amount),
            quantized_origin(horse_position),
            quantized_origin(damage_source_position),
        ) else {
            return VarDictionary::new();
        };
        let command = HorseDamageCommand {
            id: HorseDamageId {
                authority_epoch: self.bank.authority_epoch(),
                tick: SimulationTick::new(tick),
                sequence,
            },
            amount,
            horse_position,
            damage_source_position: source_position,
        };
        let Ok(Some(routed)) = self.bank.apply_horse_damage(horse_entity_id, command) else {
            return VarDictionary::new();
        };
        self.refresh_properties();
        let application = routed.effects.application;
        self.signals().horse_damaged().emit(
            i64::from(application.health_before),
            i64::from(application.health_after),
            i64::try_from(tick).unwrap_or(i64::MAX),
        );
        if application.spooked {
            self.signals().horse_bolted().emit(
                i64::try_from(tick).unwrap_or(i64::MAX),
                f64::from(application.rider_throw_distance_mm) / 1_000.0,
                application.rider_stun_ticks as f64 / 60.0,
            );
        }
        let mut result = VarDictionary::new();
        result.set("health_before", i64::from(application.health_before));
        result.set("health_after", i64::from(application.health_after));
        result.set("spooked", application.spooked);
        result.set(
            "authority_epoch",
            i64::try_from(command.id.authority_epoch).unwrap_or(i64::MAX),
        );
        result.set(
            "damage_tick",
            i64::try_from(command.id.tick.as_u64()).unwrap_or(i64::MAX),
        );
        result.set(
            "damage_sequence",
            i64::try_from(command.id.sequence).unwrap_or(i64::MAX),
        );
        result.set(
            "throw_distance_m",
            f64::from(application.rider_throw_distance_mm) / 1_000.0,
        );
        result.set("stun_seconds", application.rider_stun_ticks as f64 / 60.0);
        result.set("fall_damage", application.rider_fall_damage);
        result
    }

    /// Returns the persisted fatal-effect outbox row awaiting adapter/network
    /// delivery. Empty means no unacknowledged event.
    #[func]
    fn pending_horse_loss_effects(&self) -> VarDictionary {
        let Some(rider_player_id) = self.rider_player_id else {
            return VarDictionary::new();
        };
        let Some(effects) = self
            .bank
            .actor(rider_player_id)
            .and_then(|actor| actor.pending_horse_loss_effects())
        else {
            return VarDictionary::new();
        };
        let Some(event) = effects.horse_bolted else {
            return VarDictionary::new();
        };
        let mut row = VarDictionary::new();
        row.set(
            "authority_epoch",
            i64::try_from(event.id.authority_epoch).unwrap_or(i64::MAX),
        );
        row.set(
            "damage_tick",
            i64::try_from(event.id.tick.as_u64()).unwrap_or(i64::MAX),
        );
        row.set(
            "damage_sequence",
            i64::try_from(event.id.sequence).unwrap_or(i64::MAX),
        );
        row.set("notification_points", i64::from(event.notification_points));
        row.set(
            "bolt_away_delta_m",
            Vector2::new(
                event.bolt_away_delta_mm[0] as f32 / 1_000.0,
                event.bolt_away_delta_mm[1] as f32 / 1_000.0,
            ),
        );
        row
    }

    /// Clears only the exact fatal event confirmed delivered. A stale or
    /// mismatched acknowledgement is mutation-free.
    #[func]
    fn acknowledge_horse_loss_effects(
        &mut self,
        authority_epoch: i64,
        damage_tick: i64,
        damage_sequence: i64,
    ) -> bool {
        let Some(rider_player_id) = self.rider_player_id else {
            return false;
        };
        let (Ok(authority_epoch), Ok(damage_tick), Ok(damage_sequence)) = (
            u64::try_from(authority_epoch),
            u64::try_from(damage_tick),
            u64::try_from(damage_sequence),
        ) else {
            return false;
        };
        self.bank
            .acknowledge_horse_loss_effects(
                rider_player_id,
                HorseDamageId {
                    authority_epoch,
                    tick: SimulationTick::new(damage_tick),
                    sequence: damage_sequence,
                },
            )
            .unwrap_or(false)
    }

    /// Applies an authority-confirmed recall reduction. Kind 0 is damage dealt
    /// (amount used); kind 1 is one objective tick (amount ignored).
    #[func]
    fn apply_recall_credit(&mut self, tick: i64, sequence: i64, kind: i64, amount: i64) -> bool {
        let Some(rider_player_id) = self.rider_player_id else {
            return false;
        };
        let (Ok(tick), Ok(sequence), Some(kind)) = (
            u64::try_from(tick),
            u64::try_from(sequence),
            recall_credit_kind(kind, amount),
        ) else {
            return false;
        };
        let command = RecallCreditCommand {
            id: RecallCreditId {
                authority_epoch: self.bank.authority_epoch(),
                tick: SimulationTick::new(tick),
                sequence,
            },
            kind,
        };
        self.bank
            .apply_recall_credit(rider_player_id, command)
            .unwrap_or(false)
    }

    /// Canonical validated wire-v2 authority checkpoint JSON.
    #[func]
    fn checkpoint_json(&self) -> GString {
        GString::from(&serde_json::to_string(&self.bank.checkpoint()).unwrap_or_default())
    }

    /// Installs a validated checkpoint for exactly the next authority epoch.
    #[func]
    fn restore_checkpoint_json(&mut self, checkpoint_json: GString, next_epoch: i64) -> bool {
        let (Ok(checkpoint), Ok(next_epoch), Some(rider_player_id), Some(horse_entity_id)) = (
            serde_json::from_str::<M3AuthorityCheckpointV2>(&checkpoint_json.to_string()),
            u64::try_from(next_epoch),
            self.rider_player_id,
            self.horse_entity_id,
        ) else {
            return false;
        };
        let Ok(bank) = M3AuthorityBank::restore_checkpoint(checkpoint, next_epoch) else {
            return false;
        };
        if bank.horse_entity_id(rider_player_id) != Some(horse_entity_id) {
            return false;
        }
        self.bank = bank;
        self.authority_epoch = i64::try_from(next_epoch).unwrap_or(i64::MAX);
        self.refresh_properties();
        true
    }
}

#[godot_api]
impl INode for M3GameplayController {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            current_tick: -1,
            authority_epoch: 0,
            mode_id: -1,
            horse_health: 0,
            horse_max_health: 0,
            horse_state_id: -1,
            on_foot_state_id: -1,
            stamina_ticks: 0,
            recall_state_id: -1,
            bank: M3AuthorityBank::new(0),
            rider_player_id: None,
            horse_entity_id: None,
        }
    }
}

impl M3GameplayController {
    fn refresh_properties(&mut self) {
        let Some(rider_player_id) = self.rider_player_id else {
            return;
        };
        let Some(actor) = self.bank.actor(rider_player_id) else {
            return;
        };
        self.mode_id = mode_id(actor.mode());
        self.horse_health = i64::from(actor.horse().health());
        self.horse_max_health = i64::from(actor.horse().max_health());
        self.horse_state_id = horse_state_id(actor.horse().state());
        self.on_foot_state_id = on_foot_state_id(actor.on_foot().state());
        self.stamina_ticks = i64::from(actor.on_foot().stamina_ticks());
        self.recall_state_id = recall_state_id(actor.recall().state());
    }
}

fn horse_class(id: i64) -> Option<HorseVitalityClass> {
    match id {
        0 => Some(HorseVitalityClass::Courser),
        1 => Some(HorseVitalityClass::Warhorse),
        2 => Some(HorseVitalityClass::Mustang),
        _ => None,
    }
}

fn quantized_origin(value: Vector3) -> Option<QuantizedOrigin> {
    QuantizedOrigin::from_meters(f64::from(value.x), f64::from(value.y), f64::from(value.z)).ok()
}

fn quantized_move_input(value: Vector2) -> Option<QuantizedDirection> {
    if !value.is_finite() || value.length_squared() <= f32::EPSILON {
        return None;
    }
    let normalized = value.normalized();
    Some(QuantizedDirection::new(
        (normalized.x * DIRECTION_UNITS as f32).round() as i32,
        0,
        (normalized.y * DIRECTION_UNITS as f32).round() as i32,
    ))
    .filter(|direction| direction.is_normalized())
}

fn recall_credit_kind(kind: i64, amount: i64) -> Option<RecallCreditKind> {
    match kind {
        0 => u16::try_from(amount)
            .ok()
            .map(RecallCreditKind::DamageDealt),
        1 => Some(RecallCreditKind::ObjectiveTick),
        _ => None,
    }
}

const fn mode_id(mode: ActorM3Mode) -> i64 {
    match mode {
        ActorM3Mode::Mounted => 0,
        ActorM3Mode::SpookStunned => 1,
        ActorM3Mode::OnFoot => 2,
        ActorM3Mode::ReturningHorse => 3,
    }
}

const fn horse_state_id(state: HorseVitalityState) -> i64 {
    match state {
        HorseVitalityState::Available => 0,
        HorseVitalityState::Bolting => 1,
        HorseVitalityState::Despawned => 2,
    }
}

const fn on_foot_state_id(state: OnFootState) -> i64 {
    match state {
        OnFootState::SpookStunned => 0,
        OnFootState::Standing => 1,
        OnFootState::Sprinting => 2,
        OnFootState::Crouched => 3,
        OnFootState::Rolling => 4,
    }
}

const fn recall_state_id(state: RecallState) -> i64 {
    match state {
        RecallState::HorsePresent => 0,
        RecallState::CoolingDown => 1,
        RecallState::Ready => 2,
        RecallState::Hoofbeats => 3,
        RecallState::DustReveal => 4,
        RecallState::GallopIn => 5,
        RecallState::MountWindow => 6,
        RecallState::WaitingMount => 7,
    }
}

fn m3_stance_id(stance: M3RiderStance) -> i64 {
    match stance {
        M3RiderStance::SpookStunned => 0,
        M3RiderStance::Standing => 1,
        M3RiderStance::Sprinting => 2,
        M3RiderStance::Crouched => 3,
        M3RiderStance::Rolling => 4,
    }
}

fn actor_output_dictionary(output: spurfire_protocol::ActorM3TickOutput) -> VarDictionary {
    let mut row = VarDictionary::new();
    row.set("mode_id", mode_id(output.mode));
    row.set("horse_despawned", output.horse_despawned);
    row.set("remounted", output.remounted);
    if let Some(on_foot) = output.on_foot {
        let mut movement = VarDictionary::new();
        movement.set("state_id", on_foot_state_id(on_foot.state));
        movement.set("stance_id", m3_stance_id(on_foot.stance));
        movement.set("speed_mps", f64::from(on_foot.speed_mmps) / 1_000.0);
        movement.set(
            "velocity_mps",
            Vector2::new(
                on_foot.requested_velocity_mmps[0] as f32 / 1_000.0,
                on_foot.requested_velocity_mmps[1] as f32 / 1_000.0,
            ),
        );
        movement.set("stamina_ticks", i64::from(on_foot.stamina_ticks));
        movement.set("can_fire", on_foot.can_fire);
        movement.set("reload_pause_started", on_foot.reload_pause_started);
        movement.set(
            "sway_multiplier",
            f64::from(on_foot.sway_multiplier_milli) / 1_000.0,
        );
        movement.set(
            "roll_exit_sway",
            f64::from(on_foot.roll_exit_sway_milli) / 1_000.0,
        );
        row.set("on_foot", &movement);
    }
    if let Some(recall) = output.recall {
        let mut recall_row = VarDictionary::new();
        recall_row.set("state_id", recall_state_id(recall.state));
        recall_row.set(
            "ready_tick",
            recall
                .ready_tick
                .map_or(-1, |tick| i64::try_from(tick.as_u64()).unwrap_or(i64::MAX)),
        );
        recall_row.set(
            "cooldown_remaining_ticks",
            i64::try_from(recall.cooldown_remaining_ticks).unwrap_or(i64::MAX),
        );
        recall_row.set("mount_window_opened", recall.mount_window_opened);
        row.set("recall", &recall_row);
    }
    row
}
