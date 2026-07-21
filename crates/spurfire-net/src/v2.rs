//! Live wire-v2 actor replication contract for M3 lobbies.
//!
//! This module remains separate from the retained wire-1.2 M2 proof codec so
//! its types, validation, and canonical signing bytes switch atomically.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use serde::{ser::SerializeTuple, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use spurfire_protocol::{
    bounty_objective_world_point, canonical_envelope_digest, canonical_manifest_digest,
    BountyMatchSnapshot, BountyObjectiveSnapshot, BountyRevealSnapshot, BountyWorldPoint,
    DynamicObjectiveKind, EntityId, HorseVitalityClass, HorseVitalityState, LobbyId, M3ActorStance,
    NodeKey, PlayerId, QuantizedDirection, RecallState, RosterHash, RosterManifest, SessionBinding,
    SessionIdentityError, SessionPublicKey, SessionSignature, ShotCommand, ShotResult,
    SimulationTick, WeaponId, WireVersion, DIRECTION_UNITS, M3_WIRE_VERSION, MAJESTIC_CHARGE_TICKS,
    MAX_BOUNTY_SCORE, MAX_M3_AUTHORITY_ACTORS, MOST_WANTED_REVEAL_TICKS, OBJECTIVE_LIFETIME_TICKS,
    ON_FOOT_STAMINA_TICKS, SPUR_METER_MAX,
};

use crate::{
    AcceptOutcome, CodecError, M3MatchCheckpointV2, SessionState, HEARTBEAT_TIMEOUT_MS,
    MAX_DATAGRAM_BYTES,
};

/// Decoded checkpoint bytes carried by one migration datagram.
pub const M3_CHECKPOINT_FRAGMENT_BYTES: usize = 384;
/// Hard cap keeps reassembly at or below 72 KiB before canonical validation.
pub const MAX_M3_CHECKPOINT_FRAGMENTS: usize = 192;

/// Mounted jump edge.
pub const M3_INPUT_JUMP_PRESSED: u16 = 1 << 0;
/// Dismount/remount/recall interaction edge.
pub const M3_INPUT_INTERACT_PRESSED: u16 = 1 << 1;
/// Held on-foot sprint.
pub const M3_INPUT_SPRINT_PRESSED: u16 = 1 << 2;
/// Held/tapped crouch and tactical-roll input.
pub const M3_INPUT_CROUCH_PRESSED: u16 = 1 << 3;
/// Reload edge/hold consumed by combat authority.
pub const M3_INPUT_RELOAD_PRESSED: u16 = 1 << 4;
/// Aim-down-sights hold.
pub const M3_INPUT_ADS_PRESSED: u16 = 1 << 5;
/// Reserved M4 Spur-spend edge so M4 does not require another wire major.
pub const M3_INPUT_SPUR_PRESSED: u16 = 1 << 6;
/// Every unassigned v2 input bit must remain zero.
pub const M3_INPUT_RESERVED_MASK: u16 = !(M3_INPUT_JUMP_PRESSED
    | M3_INPUT_INTERACT_PRESSED
    | M3_INPUT_SPRINT_PRESSED
    | M3_INPUT_CROUCH_PRESSED
    | M3_INPUT_RELOAD_PRESSED
    | M3_INPUT_ADS_PRESSED
    | M3_INPUT_SPUR_PRESSED);

/// Authority-locked actor loadout announced once per session generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3ActorLoadout {
    /// Match-start horse archetype.
    #[serde(rename = "h", alias = "horse_class")]
    pub horse_class: HorseVitalityClass,
    /// Match-start rifle.
    #[serde(rename = "w", alias = "weapon_id")]
    pub weapon_id: WeaponId,
}

/// Fixed-tick player intent for both mounted and on-foot states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3ActorInput {
    /// Mounted forward/reverse intent in [-1000, 1000].
    #[serde(rename = "t", alias = "throttle_milli")]
    pub throttle_milli: i16,
    /// Mounted turn intent in [-1000, 1000].
    #[serde(rename = "s", alias = "steer_milli")]
    pub steer_milli: i16,
    /// On-foot planar X intent in [-1000, 1000].
    #[serde(rename = "x", alias = "move_x_milli")]
    pub move_x_milli: i16,
    /// On-foot planar Z intent in [-1000, 1000].
    #[serde(rename = "z", alias = "move_z_milli")]
    pub move_z_milli: i16,
    /// Assigned edge/level flags; reserved bits are zero.
    #[serde(rename = "b", alias = "buttons")]
    pub buttons: u16,
}

impl M3ActorInput {
    /// Rejects out-of-range, diagonal-overdrive, and future reserved bits.
    #[must_use]
    pub fn is_canonical(self) -> bool {
        let bounded = |value: i16| (-1_000..=1_000).contains(&value);
        let x = i64::from(self.move_x_milli);
        let z = i64::from(self.move_z_milli);
        bounded(self.throttle_milli)
            && bounded(self.steer_milli)
            && bounded(self.move_x_milli)
            && bounded(self.move_z_milli)
            && x * x + z * z <= 1_000_000
            && self.buttons & M3_INPUT_RESERVED_MASK == 0
    }
}

/// Authority-owned horse state carried with each v2 actor snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3HorseSnapshot {
    /// Stable target entity.
    #[serde(rename = "i", alias = "entity_id")]
    pub entity_id: EntityId,
    /// Immutable match-start archetype.
    #[serde(rename = "c", alias = "class")]
    pub class: HorseVitalityClass,
    /// Available/bolting/despawned lifecycle.
    #[serde(rename = "s", alias = "state")]
    pub state: HorseVitalityState,
    /// Collision-resolved position in millimetres.
    #[serde(rename = "p", alias = "position_mm")]
    pub position_mm: [i32; 3],
    /// Authority velocity in millimetres per second.
    #[serde(rename = "v", alias = "velocity_mmps")]
    pub velocity_mmps: [i32; 3],
    /// Yaw in thousandths of a degree.
    #[serde(rename = "y", alias = "yaw_millidegrees")]
    pub yaw_millidegrees: i32,
    /// Current M3 vitality.
    #[serde(rename = "h", alias = "health")]
    pub health: u16,
    /// Planar bolt-away direction while spooked.
    #[serde(rename = "d", alias = "bolt_away_direction")]
    pub bolt_away_direction: QuantizedDirection,
}

impl M3HorseSnapshot {
    #[must_use]
    fn is_canonical(self) -> bool {
        self.health <= self.class.max_health()
            && self.bolt_away_direction.is_normalized()
            && self.bolt_away_direction.y.abs() <= DIRECTION_UNITS / 100
            && match self.state {
                HorseVitalityState::Available => self.health > 0,
                HorseVitalityState::Bolting | HorseVitalityState::Despawned => self.health == 0,
            }
    }
}

/// Complete authority snapshot used for local reconciliation and remote presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3ActorSnapshot {
    /// Roster subject represented by the authority.
    #[serde(rename = "i", alias = "rider_player_id")]
    pub rider_player_id: PlayerId,
    /// Collision-resolved rider position.
    #[serde(rename = "p", alias = "rider_position_mm")]
    pub rider_position_mm: [i32; 3],
    /// Rider velocity.
    #[serde(rename = "v", alias = "rider_velocity_mmps")]
    pub rider_velocity_mmps: [i32; 3],
    /// Rider yaw in thousandths of a degree.
    #[serde(rename = "y", alias = "rider_yaw_millidegrees")]
    pub rider_yaw_millidegrees: i32,
    /// Complete wire-v2 logical stance.
    #[serde(rename = "s", alias = "stance")]
    pub stance: M3ActorStance,
    /// Authority-owned rider health.
    #[serde(rename = "h", alias = "rider_health")]
    pub rider_health: u16,
    /// Remaining on-foot sprint capacity.
    #[serde(rename = "a", alias = "stamina_ticks")]
    pub stamina_ticks: u32,
    /// Complete horse state, including absent/return phases.
    #[serde(rename = "o", alias = "horse")]
    pub horse: M3HorseSnapshot,
    /// Majestic Return phase.
    #[serde(rename = "r", alias = "recall_state")]
    pub recall_state: RecallState,
    /// Earliest recall request tick while cooling down.
    #[serde(
        default,
        rename = "q",
        alias = "recall_ready_tick",
        skip_serializing_if = "Option::is_none"
    )]
    pub recall_ready_tick: Option<SimulationTick>,
    /// Authority-owned M4 Spur meter.
    #[serde(default, rename = "u", alias = "spur_meter")]
    pub spur_meter: u8,
    /// Exact charge start tick when Majestic Charge is active.
    #[serde(
        default,
        rename = "b",
        alias = "charge_started_tick",
        skip_serializing_if = "Option::is_none"
    )]
    pub charge_started_tick: Option<SimulationTick>,
    /// Exclusive charge end tick when Majestic Charge is active.
    #[serde(
        default,
        rename = "e",
        alias = "charge_end_tick",
        skip_serializing_if = "Option::is_none"
    )]
    pub charge_end_tick: Option<SimulationTick>,
}

/// Base64-on-JSON fragment so large migration checkpoints remain MTU-safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3CheckpointFragment(Vec<u8>);

impl M3CheckpointFragment {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for M3CheckpointFragment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD_NO_PAD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for M3CheckpointFragment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD_NO_PAD
            .decode(encoded)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl M3ActorSnapshot {
    /// Cross-field validation for bounded actor/horse/recall truth.
    #[must_use]
    pub fn is_canonical(self) -> bool {
        if self.rider_health > 100
            || self.stamina_ticks > ON_FOOT_STAMINA_TICKS
            || self.spur_meter > SPUR_METER_MAX
            || !self.horse.is_canonical()
            || !match (self.charge_started_tick, self.charge_end_tick) {
                (None, None) => true,
                (Some(start), Some(end)) => end == start.saturating_add(MAJESTIC_CHARGE_TICKS),
                _ => false,
            }
        {
            return false;
        }
        match self.recall_state {
            RecallState::HorsePresent => {
                matches!(
                    self.horse.state,
                    HorseVitalityState::Available | HorseVitalityState::Bolting
                ) && self.recall_ready_tick.is_none()
            }
            RecallState::CoolingDown => {
                self.horse.state == HorseVitalityState::Despawned
                    && self.recall_ready_tick.is_some()
            }
            RecallState::Ready
            | RecallState::Hoofbeats
            | RecallState::DustReveal
            | RecallState::GallopIn
            | RecallState::MountWindow
            | RecallState::WaitingMount => {
                self.horse.state == HorseVitalityState::Despawned
                    && self.recall_ready_tick.is_some()
            }
        }
    }
}

/// Compact public score row kept below the live UDP MTU for eight-player Alpha matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5ScoreRowV2 {
    pub player_id: PlayerId,
    pub score: u32,
    pub eliminations: u16,
    pub assists: u16,
    pub deaths: u16,
    pub alive: bool,
    pub respawn_at_tick: Option<SimulationTick>,
    pub respawn_speed_buff_end_tick: Option<SimulationTick>,
    pub horse_buff_end_tick: Option<SimulationTick>,
    /// Elimination, assist, horse-bolt, dive, long-hit, objective,
    /// Most-Wanted-elimination, and Most-Wanted-survival points.
    pub score_breakdown: Option<[u32; 8]>,
}

impl Serialize for M5ScoreRowV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut row = serializer.serialize_tuple(if self.score_breakdown.is_some() {
            10
        } else {
            9
        })?;
        row.serialize_element(&self.player_id)?;
        row.serialize_element(&self.score)?;
        row.serialize_element(&self.eliminations)?;
        row.serialize_element(&self.assists)?;
        row.serialize_element(&self.deaths)?;
        row.serialize_element(&self.alive)?;
        row.serialize_element(&self.respawn_at_tick)?;
        row.serialize_element(&self.respawn_speed_buff_end_tick)?;
        row.serialize_element(&self.horse_buff_end_tick)?;
        if let Some(breakdown) = self.score_breakdown {
            row.serialize_element(&breakdown)?;
        }
        row.end()
    }
}

impl<'de> Deserialize<'de> for M5ScoreRowV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        type ActiveRow = (
            PlayerId,
            u32,
            u16,
            u16,
            u16,
            bool,
            Option<SimulationTick>,
            Option<SimulationTick>,
            Option<SimulationTick>,
        );
        type FinalRow = (
            PlayerId,
            u32,
            u16,
            u16,
            u16,
            bool,
            Option<SimulationTick>,
            Option<SimulationTick>,
            Option<SimulationTick>,
            [u32; 8],
        );
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireRow {
            Final(FinalRow),
            Active(ActiveRow),
        }
        let (active, score_breakdown) = match WireRow::deserialize(deserializer)? {
            WireRow::Active(row) => (row, None),
            WireRow::Final((a, b, c, d, e, f, g, h, i, breakdown)) => {
                ((a, b, c, d, e, f, g, h, i), Some(breakdown))
            }
        };
        let (
            player_id,
            score,
            eliminations,
            assists,
            deaths,
            alive,
            respawn_at_tick,
            respawn_speed_buff_end_tick,
            horse_buff_end_tick,
        ) = active;
        Ok(Self {
            player_id,
            score,
            eliminations,
            assists,
            deaths,
            alive,
            respawn_at_tick,
            respawn_speed_buff_end_tick,
            horse_buff_end_tick,
            score_breakdown,
        })
    }
}

/// Compact reveal row for a routine MatchState keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M5RevealV2 {
    #[serde(rename = "i", alias = "player_id")]
    pub player_id: PlayerId,
    #[serde(rename = "s", alias = "started_tick")]
    pub started_tick: SimulationTick,
    #[serde(rename = "e", alias = "end_tick")]
    pub end_tick: SimulationTick,
}

/// Compact objective row for a routine MatchState keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M5ObjectiveV2 {
    #[serde(rename = "i", alias = "objective_id")]
    pub objective_id: u64,
    #[serde(rename = "k", alias = "kind")]
    pub kind: DynamicObjectiveKind,
    #[serde(rename = "s", alias = "started_tick")]
    pub started_tick: SimulationTick,
    #[serde(rename = "e", alias = "end_tick")]
    pub end_tick: SimulationTick,
    #[serde(rename = "c", alias = "completed")]
    pub completed: bool,
    #[serde(rename = "x", alias = "x_mm")]
    pub x_mm: i32,
    #[serde(rename = "z", alias = "z_mm")]
    pub z_mm: i32,
}

/// Signed two-hertz M5 presentation keyframe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M5MatchStateV2 {
    #[serde(rename = "e", alias = "authority_epoch")]
    pub authority_epoch: u64,
    #[serde(rename = "g", alias = "lobby_seed")]
    pub lobby_seed: u64,
    #[serde(rename = "t", alias = "current_tick")]
    pub current_tick: SimulationTick,
    #[serde(rename = "n", alias = "end_tick")]
    pub end_tick: SimulationTick,
    #[serde(rename = "p", alias = "players")]
    pub players: Vec<M5ScoreRowV2>,
    #[serde(
        default,
        rename = "w",
        alias = "active_reveal",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_reveal: Option<M5RevealV2>,
    #[serde(
        default,
        rename = "o",
        alias = "active_objective",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_objective: Option<M5ObjectiveV2>,
    #[serde(rename = "f", alias = "finished")]
    pub finished: bool,
    #[serde(
        default,
        rename = "x",
        alias = "winner",
        skip_serializing_if = "Option::is_none"
    )]
    pub winner: Option<PlayerId>,
}

impl M5MatchStateV2 {
    #[must_use]
    pub fn from_snapshot(snapshot: &BountyMatchSnapshot) -> Self {
        Self {
            authority_epoch: snapshot.authority_epoch,
            lobby_seed: snapshot.lobby_seed,
            current_tick: snapshot.current_tick,
            end_tick: snapshot.end_tick,
            players: snapshot
                .players
                .iter()
                .map(|row| M5ScoreRowV2 {
                    player_id: row.player_id,
                    score: row.total_score(),
                    eliminations: row.eliminations,
                    assists: row.assists,
                    deaths: row.deaths,
                    alive: row.alive,
                    respawn_at_tick: row.respawn_at_tick,
                    respawn_speed_buff_end_tick: row.respawn_speed_buff_end_tick,
                    horse_buff_end_tick: row.horse_buff_end_tick,
                    score_breakdown: snapshot.finished.then_some([
                        row.score.elimination,
                        row.score.assist,
                        row.score.horse_bolt,
                        row.score.saddle_dive_bonus,
                        row.score.mounted_long_hit,
                        row.score.objective,
                        row.score.most_wanted_elimination,
                        row.score.most_wanted_survival,
                    ]),
                })
                .collect(),
            active_reveal: snapshot.active_reveal.map(
                |BountyRevealSnapshot {
                     player_id,
                     started_tick,
                     end_tick,
                 }| M5RevealV2 {
                    player_id,
                    started_tick,
                    end_tick,
                },
            ),
            active_objective: snapshot.active_objective.map(
                |BountyObjectiveSnapshot {
                     objective_id,
                     kind,
                     started_tick,
                     end_tick,
                     completed,
                     world_point,
                 }| M5ObjectiveV2 {
                    objective_id,
                    kind,
                    started_tick,
                    end_tick,
                    completed,
                    x_mm: world_point.x_mm,
                    z_mm: world_point.z_mm,
                },
            ),
            finished: snapshot.finished,
            winner: snapshot.winner,
        }
    }

    #[must_use]
    pub fn is_canonical(&self) -> bool {
        if self.players.is_empty()
            || self.players.len() > MAX_M3_AUTHORITY_ACTORS
            || self.finished != (self.current_tick >= self.end_tick)
            || self
                .players
                .windows(2)
                .any(|rows| rows[0].player_id >= rows[1].player_id)
        {
            return false;
        }
        let effective_tick =
            SimulationTick::new(self.current_tick.as_u64().min(self.end_tick.as_u64()));
        if self.players.iter().any(|row| {
            row.score > MAX_BOUNTY_SCORE
                || row.score_breakdown.is_some() != self.finished
                || row.score_breakdown.is_some_and(|breakdown| {
                    row.score
                        != breakdown
                            .iter()
                            .copied()
                            .fold(0_u32, u32::saturating_add)
                            .min(MAX_BOUNTY_SCORE)
                })
                || row.alive == row.respawn_at_tick.is_some()
                || row
                    .respawn_at_tick
                    .is_some_and(|respawn| !self.finished && respawn <= effective_tick)
                || row
                    .respawn_speed_buff_end_tick
                    .is_some_and(|end| end <= effective_tick)
                || row
                    .horse_buff_end_tick
                    .is_some_and(|end| end <= effective_tick)
        }) {
            return false;
        }
        let contains_player = |player| self.players.iter().any(|row| row.player_id == player);
        if self.active_reveal.is_some_and(|reveal| {
            self.finished
                || !contains_player(reveal.player_id)
                || reveal.end_tick != reveal.started_tick.saturating_add(MOST_WANTED_REVEAL_TICKS)
                || effective_tick >= reveal.end_tick
        }) || self.active_objective.is_some_and(|objective| {
            self.finished
                || objective.objective_id == 0
                || objective.end_tick
                    != objective
                        .started_tick
                        .saturating_add(OBJECTIVE_LIFETIME_TICKS)
                || effective_tick >= objective.end_tick
                || BountyWorldPoint {
                    x_mm: objective.x_mm,
                    z_mm: objective.z_mm,
                } != bounty_objective_world_point(
                    self.lobby_seed,
                    objective.objective_id,
                    u32::try_from(self.players.len()).unwrap_or(u32::MAX),
                )
        }) {
            return false;
        }
        let expected_winner = self.players.iter().max_by(|left, right| {
            left.score
                .cmp(&right.score)
                .then_with(|| right.player_id.cmp(&left.player_id))
        });
        match (self.finished, self.winner, expected_winner) {
            (false, None, Some(_)) => true,
            (true, Some(winner), Some(expected)) => winner == expected.player_id,
            _ => false,
        }
    }
}

/// Candidate v2 gameplay payload set. No v1 stance/input interpretation is reused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum M3PeerPayloadV2 {
    Hello {
        #[serde(rename = "h", alias = "hostname")]
        hostname: String,
    },
    Heartbeat,
    Probe {
        #[serde(rename = "n", alias = "nonce")]
        nonce: u64,
        #[serde(rename = "r", alias = "reply")]
        reply: bool,
    },
    ActorLoadout {
        #[serde(rename = "l", alias = "loadout")]
        loadout: M3ActorLoadout,
    },
    ActorInput {
        #[serde(rename = "i", alias = "input")]
        input: M3ActorInput,
    },
    ActorSnapshot {
        #[serde(rename = "s", alias = "snapshot")]
        snapshot: M3ActorSnapshot,
    },
    ShotCommand {
        #[serde(rename = "c", alias = "command")]
        command: ShotCommand,
    },
    ShotResult {
        #[serde(rename = "r", alias = "result")]
        result: ShotResult,
    },
    MatchState {
        #[serde(rename = "m", alias = "state")]
        state: M5MatchStateV2,
    },
    Authority {
        #[serde(rename = "a", alias = "authority")]
        authority: PlayerId,
        #[serde(rename = "e", alias = "epoch")]
        epoch: u64,
    },
    MigrationFragment {
        #[serde(rename = "a", alias = "authority")]
        authority: PlayerId,
        #[serde(rename = "e", alias = "epoch")]
        epoch: u64,
        #[serde(rename = "h", alias = "state_hash")]
        state_hash: [u8; 32],
        #[serde(rename = "i", alias = "fragment_index")]
        fragment_index: u16,
        #[serde(rename = "n", alias = "fragment_count")]
        fragment_count: u16,
        #[serde(rename = "f", alias = "fragment")]
        fragment: M3CheckpointFragment,
    },
    Leave,
}

/// Signed candidate v2 datagram. Activation replaces the v1 envelope atomically.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3EnvelopeV2 {
    #[serde(rename = "v", alias = "wire_version")]
    pub wire_version: WireVersion,
    #[serde(rename = "l", alias = "lobby_id")]
    pub lobby_id: LobbyId,
    #[serde(rename = "p", alias = "sender")]
    pub sender: PlayerId,
    #[serde(rename = "q", alias = "sequence")]
    pub sequence: u64,
    #[serde(rename = "e", alias = "authority_epoch")]
    pub authority_epoch: u64,
    #[serde(rename = "t", alias = "simulation_tick")]
    pub simulation_tick: u64,
    #[serde(rename = "d", alias = "payload")]
    pub payload: M3PeerPayloadV2,
    #[serde(
        default,
        rename = "s",
        alias = "session",
        skip_serializing_if = "Option::is_none"
    )]
    pub session: Option<SessionBinding>,
}

impl M3EnvelopeV2 {
    /// Canonical signature digest; JSON representation is never signed.
    #[must_use]
    pub fn signing_digest(&self) -> Option<[u8; 32]> {
        let session = self.session.as_ref()?;
        Some(canonical_envelope_digest(
            self.wire_version,
            self.lobby_id,
            session.network_generation,
            session.session_generation,
            session.roster_hash,
            self.sender,
            self.authority_epoch,
            self.sequence,
            self.simulation_tick,
            &canonical_m3_payload_bytes(&self.payload),
        ))
    }
}

#[derive(Clone, Debug)]
struct PendingMigration {
    authority: PlayerId,
    epoch: u64,
    state_hash: [u8; 32],
    fragment_count: u16,
    started_ms: u64,
    envelopes: BTreeMap<u16, M3EnvelopeV2>,
}

/// Exact-roster signing, source admission, replay, and atomic M3 migration.
#[derive(Clone, Debug)]
pub struct M3SecureSession {
    manifest: RosterManifest,
    roster_hash: RosterHash,
    state: SessionState,
    pending_migration: Option<PendingMigration>,
    installed_checkpoint: Option<M3MatchCheckpointV2>,
}

impl M3SecureSession {
    pub fn new(
        manifest: RosterManifest,
        manifest_public_key: SessionPublicKey,
        manifest_signature: SessionSignature,
        state: SessionState,
    ) -> Result<Self, SessionIdentityError> {
        manifest.validate()?;
        manifest_public_key.verify_digest(
            &canonical_manifest_digest(manifest_public_key, &manifest),
            manifest_signature,
        )?;
        let roster_hash = manifest.hash();
        Ok(Self {
            manifest,
            roster_hash,
            state,
            pending_migration: None,
            installed_checkpoint: None,
        })
    }

    #[must_use]
    pub const fn roster_hash(&self) -> RosterHash {
        self.roster_hash
    }

    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    #[must_use]
    pub fn installed_checkpoint(&self) -> Option<&M3MatchCheckpointV2> {
        self.installed_checkpoint.as_ref()
    }

    pub fn take_installed_checkpoint(&mut self) -> Option<M3MatchCheckpointV2> {
        self.installed_checkpoint.take()
    }

    pub fn envelope(
        &mut self,
        tick: u64,
        payload: M3PeerPayloadV2,
        signing_key: &SigningKey,
    ) -> Result<M3EnvelopeV2, SessionIdentityError> {
        validate_payload(&payload).map_err(|_| SessionIdentityError::BadSignature)?;
        let local = self.state.local_player;
        let authority = self.state.authority;
        let subject_is_valid = match &payload {
            M3PeerPayloadV2::ActorSnapshot { snapshot } => {
                local == authority && self.state.peers.contains_key(&snapshot.rider_player_id)
            }
            M3PeerPayloadV2::MatchState { state } => {
                local == authority
                    && state.authority_epoch == self.state.authority_epoch
                    && state.current_tick.as_u64() == tick
            }
            M3PeerPayloadV2::ShotResult { result } => {
                local == authority
                    && result.tick.as_u64() == tick
                    && self.state.peers.contains_key(&result.shooter_peer_id)
            }
            M3PeerPayloadV2::ShotCommand { command } => {
                command.shooter_peer_id == local && command.tick.as_u64() == tick
            }
            M3PeerPayloadV2::Authority {
                authority: claimed,
                epoch,
            }
            | M3PeerPayloadV2::MigrationFragment {
                authority: claimed,
                epoch,
                ..
            } => *claimed == local && *epoch == self.state.authority_epoch,
            M3PeerPayloadV2::Hello { .. }
            | M3PeerPayloadV2::Heartbeat
            | M3PeerPayloadV2::Probe { .. }
            | M3PeerPayloadV2::ActorLoadout { .. }
            | M3PeerPayloadV2::ActorInput { .. }
            | M3PeerPayloadV2::Leave => true,
        };
        if !subject_is_valid {
            return Err(SessionIdentityError::BadSignature);
        }
        let sequence = self.state.next_sequence;
        let mut envelope = M3EnvelopeV2 {
            wire_version: M3_WIRE_VERSION,
            lobby_id: self.state.lobby_id,
            sender: local,
            sequence,
            authority_epoch: self.state.authority_epoch,
            simulation_tick: tick,
            payload,
            session: None,
        };
        self.sign(&mut envelope, signing_key)?;
        self.state.next_sequence = self.state.next_sequence.saturating_add(1);
        Ok(envelope)
    }

    pub fn sign(
        &self,
        envelope: &mut M3EnvelopeV2,
        signing_key: &SigningKey,
    ) -> Result<(), SessionIdentityError> {
        if envelope.wire_version != M3_WIRE_VERSION {
            return Err(SessionIdentityError::BadSignature);
        }
        let Some(entry) = self
            .manifest
            .entries
            .iter()
            .find(|entry| entry.player_id == envelope.sender)
        else {
            return Err(SessionIdentityError::BadSignature);
        };
        if entry.session_public_key.as_bytes() != &signing_key.verifying_key().to_bytes() {
            return Err(SessionIdentityError::BadSignature);
        }
        let digest = m3_envelope_digest(
            envelope,
            self.manifest.network_generation,
            self.manifest.session_generation,
            self.roster_hash,
        );
        envelope.session = Some(SessionBinding {
            network_generation: self.manifest.network_generation,
            session_generation: self.manifest.session_generation,
            roster_hash: self.roster_hash,
            signature: SessionSignature::from_bytes(signing_key.sign(&digest).to_bytes()),
        });
        Ok(())
    }

    pub fn expire_and_migrate(&mut self, now_ms: u64) -> Option<(PlayerId, u64)> {
        self.pending_migration = None;
        self.state.expire_and_migrate(now_ms)
    }

    pub fn accept_with_source(
        &mut self,
        envelope: &M3EnvelopeV2,
        source: SocketAddr,
        current_node_key: Option<NodeKey>,
        now_ms: u64,
    ) -> AcceptOutcome {
        self.accept_with_source_validated(envelope, source, current_node_key, now_ms, |_| true)
    }

    /// Secure receive gate with a final application checkpoint preflight.
    /// The callback runs before replay, authority, or installed state mutates.
    pub fn accept_with_source_validated(
        &mut self,
        envelope: &M3EnvelopeV2,
        source: SocketAddr,
        current_node_key: Option<NodeKey>,
        now_ms: u64,
        mut checkpoint_is_installable: impl FnMut(&M3MatchCheckpointV2) -> bool,
    ) -> AcceptOutcome {
        if envelope.wire_version != M3_WIRE_VERSION {
            return AcceptOutcome::InvalidPayloadRole;
        }
        let Some(binding) = envelope.session else {
            return AcceptOutcome::UnsignedInSecureMode;
        };
        let Some(entry) = self
            .manifest
            .entries
            .iter()
            .find(|entry| entry.player_id == envelope.sender)
        else {
            return AcceptOutcome::UnknownSender;
        };
        if entry.tailnet_address != source.ip() || entry.application_port != source.port() {
            return AcceptOutcome::EndpointMismatch;
        }
        if let (Some(claimed), Some(current)) = (entry.node_key, current_node_key) {
            if claimed != current {
                return AcceptOutcome::NodeKeyMismatch;
            }
        }
        if envelope.lobby_id != self.manifest.lobby_id {
            return AcceptOutcome::WrongLobby;
        }
        if binding.network_generation != self.manifest.network_generation
            || binding.session_generation != self.manifest.session_generation
        {
            return AcceptOutcome::WrongGeneration;
        }
        if binding.roster_hash != self.roster_hash {
            return AcceptOutcome::RosterMismatch;
        }
        let digest = m3_envelope_digest(
            envelope,
            binding.network_generation,
            binding.session_generation,
            binding.roster_hash,
        );
        if entry
            .session_public_key
            .verify_digest(&digest, binding.signature)
            .is_err()
        {
            return AcceptOutcome::BadSignature;
        }
        if validate_payload(&envelope.payload).is_err() {
            return AcceptOutcome::InvalidPayloadSubject;
        }
        self.accept_authenticated(envelope, now_ms, &mut checkpoint_is_installable)
    }

    fn accept_authenticated(
        &mut self,
        envelope: &M3EnvelopeV2,
        now_ms: u64,
        checkpoint_is_installable: &mut impl FnMut(&M3MatchCheckpointV2) -> bool,
    ) -> AcceptOutcome {
        if envelope.lobby_id != self.state.lobby_id {
            return AcceptOutcome::WrongLobby;
        }
        if envelope.authority_epoch < self.state.authority_epoch {
            return AcceptOutcome::StaleAuthorityEpoch;
        }
        if !self.state.peers.contains_key(&envelope.sender) {
            return AcceptOutcome::UnknownSender;
        }
        if matches!(envelope.payload, M3PeerPayloadV2::MigrationFragment { .. }) {
            return self.accept_migration_fragment(envelope, now_ms, checkpoint_is_installable);
        }
        let authority_claim = matches!(envelope.payload, M3PeerPayloadV2::Authority { .. });
        if envelope.authority_epoch != self.state.authority_epoch && !authority_claim {
            return AcceptOutcome::InvalidPayloadRole;
        }
        if matches!(
            envelope.payload,
            M3PeerPayloadV2::ActorSnapshot { .. }
                | M3PeerPayloadV2::ShotResult { .. }
                | M3PeerPayloadV2::MatchState { .. }
        ) && envelope.sender != self.state.authority
        {
            return AcceptOutcome::InvalidPayloadRole;
        }
        let subject_is_invalid = match &envelope.payload {
            M3PeerPayloadV2::ShotCommand { command } => {
                command.shooter_peer_id != envelope.sender
                    || command.tick.as_u64() != envelope.simulation_tick
            }
            M3PeerPayloadV2::ShotResult { result } => {
                result.tick.as_u64() != envelope.simulation_tick
                    || !self.state.peers.contains_key(&result.shooter_peer_id)
            }
            M3PeerPayloadV2::ActorSnapshot { snapshot } => {
                !self.state.peers.contains_key(&snapshot.rider_player_id)
            }
            M3PeerPayloadV2::MatchState { state } => {
                state.authority_epoch != envelope.authority_epoch
                    || state.current_tick.as_u64() != envelope.simulation_tick
                    || state
                        .players
                        .iter()
                        .any(|row| !self.state.peers.contains_key(&row.player_id))
            }
            M3PeerPayloadV2::Authority { authority, epoch } => {
                *authority != envelope.sender || *epoch != envelope.authority_epoch
            }
            M3PeerPayloadV2::Hello { .. }
            | M3PeerPayloadV2::Heartbeat
            | M3PeerPayloadV2::Probe { .. }
            | M3PeerPayloadV2::ActorLoadout { .. }
            | M3PeerPayloadV2::ActorInput { .. }
            | M3PeerPayloadV2::Leave => false,
            M3PeerPayloadV2::MigrationFragment { .. } => unreachable!("handled above"),
        };
        if subject_is_invalid {
            return AcceptOutcome::InvalidPayloadSubject;
        }
        if matches!(
            envelope.payload,
            M3PeerPayloadV2::ShotResult { ref result }
                if self.state.applied_shot_results.contains(&(
                    envelope.authority_epoch,
                    result.shooter_peer_id,
                    result.tick.as_u64(),
                ))
        ) {
            return AcceptOutcome::DuplicateShotResult;
        }
        if envelope.sequence <= self.state.peers[&envelope.sender].last_sequence {
            return AcceptOutcome::DuplicateOrReplay;
        }
        if let M3PeerPayloadV2::Authority { authority, epoch } = envelope.payload {
            if !self
                .state
                .authority_claim_is_coherent(envelope.sender, authority, epoch, now_ms)
            {
                return AcceptOutcome::InvalidAuthorityClaim;
            }
        }
        let peer = self
            .state
            .peers
            .get_mut(&envelope.sender)
            .expect("sender membership checked");
        peer.last_sequence = envelope.sequence;
        peer.last_seen_ms = now_ms;
        peer.connected = !matches!(envelope.payload, M3PeerPayloadV2::Leave);
        if let M3PeerPayloadV2::ShotResult { ref result } = envelope.payload {
            self.state.applied_shot_results.insert((
                envelope.authority_epoch,
                result.shooter_peer_id,
                result.tick.as_u64(),
            ));
        }
        if let M3PeerPayloadV2::Authority { authority, epoch } = envelope.payload {
            if epoch > self.state.authority_epoch
                || (epoch == self.state.authority_epoch && authority < self.state.authority)
            {
                self.state.authority = authority;
                self.state.authority_epoch = epoch;
            }
        }
        AcceptOutcome::Accepted
    }

    fn accept_migration_fragment(
        &mut self,
        envelope: &M3EnvelopeV2,
        now_ms: u64,
        checkpoint_is_installable: &mut impl FnMut(&M3MatchCheckpointV2) -> bool,
    ) -> AcceptOutcome {
        let M3PeerPayloadV2::MigrationFragment {
            authority,
            epoch,
            state_hash,
            fragment_index,
            fragment_count,
            ..
        } = &envelope.payload
        else {
            unreachable!("caller matched fragment")
        };
        if *authority != envelope.sender
            || *epoch != envelope.authority_epoch
            || !self
                .state
                .authority_claim_is_coherent(envelope.sender, *authority, *epoch, now_ms)
        {
            return AcceptOutcome::InvalidAuthorityClaim;
        }
        let last_sequence = self.state.peers[&envelope.sender].last_sequence;
        if envelope.sequence <= last_sequence {
            return AcceptOutcome::DuplicateOrReplay;
        }
        if self.pending_migration.as_ref().is_some_and(|pending| {
            now_ms.saturating_sub(pending.started_ms) >= HEARTBEAT_TIMEOUT_MS
        }) {
            self.pending_migration = None;
        }
        let pending = self
            .pending_migration
            .get_or_insert_with(|| PendingMigration {
                authority: *authority,
                epoch: *epoch,
                state_hash: *state_hash,
                fragment_count: *fragment_count,
                started_ms: now_ms,
                envelopes: BTreeMap::new(),
            });
        if pending.authority != *authority
            || pending.epoch != *epoch
            || pending.state_hash != *state_hash
            || pending.fragment_count != *fragment_count
        {
            return AcceptOutcome::InvalidCheckpoint;
        }
        if pending.envelopes.contains_key(fragment_index)
            || pending
                .envelopes
                .values()
                .any(|candidate| candidate.sequence == envelope.sequence)
        {
            return AcceptOutcome::DuplicateOrReplay;
        }
        pending.envelopes.insert(*fragment_index, envelope.clone());
        if pending.envelopes.len() != usize::from(*fragment_count) {
            return AcceptOutcome::PendingMigration;
        }
        let pending = self
            .pending_migration
            .take()
            .expect("complete pending migration exists");
        let payloads = pending
            .envelopes
            .values()
            .map(|candidate| candidate.payload.clone())
            .collect::<Vec<_>>();
        let Ok((authority, epoch, checkpoint)) = reassemble_m3_checkpoint(&payloads) else {
            return AcceptOutcome::InvalidCheckpoint;
        };
        if pending
            .envelopes
            .values()
            .any(|candidate| candidate.simulation_tick != checkpoint.combat.tick)
            || !self
                .state
                .authority_claim_is_coherent(envelope.sender, authority, epoch, now_ms)
        {
            return AcceptOutcome::InvalidCheckpoint;
        }
        if !checkpoint_is_installable(&checkpoint) {
            return AcceptOutcome::InvalidCheckpoint;
        }
        let sequences = pending
            .envelopes
            .values()
            .map(|candidate| candidate.sequence)
            .collect::<BTreeSet<_>>();
        if sequences.len() != pending.envelopes.len()
            || sequences
                .first()
                .is_none_or(|sequence| *sequence <= last_sequence)
        {
            return AcceptOutcome::DuplicateOrReplay;
        }
        let peer = self
            .state
            .peers
            .get_mut(&envelope.sender)
            .expect("sender membership checked");
        peer.last_sequence = *sequences.last().expect("complete migration is nonempty");
        peer.last_seen_ms = now_ms;
        peer.connected = true;
        self.state.applied_shot_results.extend(
            checkpoint
                .combat
                .resolved_shots
                .iter()
                .map(|(shooter, tick)| (checkpoint.combat.source_epoch, *shooter, *tick)),
        );
        self.state.authority = authority;
        self.state.authority_epoch = epoch;
        self.installed_checkpoint = Some(checkpoint);
        AcceptOutcome::Accepted
    }
}

fn m3_envelope_digest(
    envelope: &M3EnvelopeV2,
    network_generation: u64,
    session_generation: u64,
    roster_hash: RosterHash,
) -> [u8; 32] {
    canonical_envelope_digest(
        envelope.wire_version,
        envelope.lobby_id,
        network_generation,
        session_generation,
        roster_hash,
        envelope.sender,
        envelope.authority_epoch,
        envelope.sequence,
        envelope.simulation_tick,
        &canonical_m3_payload_bytes(&envelope.payload),
    )
}

/// Encodes only exact wire 2.0 and canonical M3 fields.
pub fn encode_m3(envelope: &M3EnvelopeV2) -> Result<Vec<u8>, CodecError> {
    if envelope.wire_version != M3_WIRE_VERSION {
        return Err(CodecError::IncompatibleVersion);
    }
    validate_payload(&envelope.payload)?;
    let encoded =
        serde_json::to_vec(envelope).map_err(|error| CodecError::Malformed(error.to_string()))?;
    if encoded.len() > MAX_DATAGRAM_BYTES {
        return Err(CodecError::TooLarge);
    }
    Ok(encoded)
}

/// Decodes only exact wire 2.0; active v1 packets fail closed.
pub fn decode_m3(bytes: &[u8]) -> Result<M3EnvelopeV2, CodecError> {
    if bytes.len() > MAX_DATAGRAM_BYTES {
        return Err(CodecError::TooLarge);
    }
    #[derive(Deserialize)]
    struct VersionHeader {
        #[serde(rename = "v", alias = "wire_version")]
        wire_version: WireVersion,
    }
    let header: VersionHeader =
        serde_json::from_slice(bytes).map_err(|error| CodecError::Malformed(error.to_string()))?;
    if header.wire_version != M3_WIRE_VERSION {
        return Err(CodecError::IncompatibleVersion);
    }
    let envelope: M3EnvelopeV2 =
        serde_json::from_slice(bytes).map_err(|error| CodecError::Malformed(error.to_string()))?;
    validate_payload(&envelope.payload)?;
    Ok(envelope)
}

fn validate_payload(payload: &M3PeerPayloadV2) -> Result<(), CodecError> {
    let valid = match payload {
        M3PeerPayloadV2::Hello { hostname } => !hostname.is_empty() && hostname.len() <= 255,
        M3PeerPayloadV2::ActorInput { input } => input.is_canonical(),
        M3PeerPayloadV2::ActorSnapshot { snapshot } => snapshot.is_canonical(),
        M3PeerPayloadV2::MatchState { state } => state.is_canonical(),
        M3PeerPayloadV2::MigrationFragment {
            fragment_index,
            fragment_count,
            fragment,
            ..
        } => {
            usize::from(*fragment_count) <= MAX_M3_CHECKPOINT_FRAGMENTS
                && *fragment_count > 0
                && *fragment_index < *fragment_count
                && !fragment.as_bytes().is_empty()
                && fragment.as_bytes().len() <= M3_CHECKPOINT_FRAGMENT_BYTES
        }
        M3PeerPayloadV2::Heartbeat
        | M3PeerPayloadV2::Probe { .. }
        | M3PeerPayloadV2::ActorLoadout { .. }
        | M3PeerPayloadV2::ShotCommand { .. }
        | M3PeerPayloadV2::ShotResult { .. }
        | M3PeerPayloadV2::Authority { .. }
        | M3PeerPayloadV2::Leave => true,
    };
    if valid {
        Ok(())
    } else {
        Err(noncanonical_payload())
    }
}

/// Fixed-layout payload bytes used by strict application signatures.
#[must_use]
pub fn canonical_m3_payload_bytes(payload: &M3PeerPayloadV2) -> Vec<u8> {
    let mut out = Vec::new();
    match payload {
        M3PeerPayloadV2::Hello { hostname } => {
            out.push(0);
            string(&mut out, hostname);
        }
        M3PeerPayloadV2::Heartbeat => out.push(1),
        M3PeerPayloadV2::Probe { nonce, reply } => {
            out.push(2);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.push(u8::from(*reply));
        }
        M3PeerPayloadV2::ActorLoadout { loadout } => {
            out.push(3);
            out.push(horse_class_code(loadout.horse_class));
            out.push(loadout.weapon_id.as_u8());
        }
        M3PeerPayloadV2::ActorInput { input } => {
            out.push(4);
            for value in [
                input.throttle_milli,
                input.steer_milli,
                input.move_x_milli,
                input.move_z_milli,
            ] {
                out.extend_from_slice(&value.to_be_bytes());
            }
            out.extend_from_slice(&input.buttons.to_be_bytes());
        }
        M3PeerPayloadV2::ActorSnapshot { snapshot } => {
            out.push(5);
            out.extend_from_slice(snapshot.rider_player_id.as_bytes());
            i32_array(&mut out, snapshot.rider_position_mm);
            i32_array(&mut out, snapshot.rider_velocity_mmps);
            out.extend_from_slice(&snapshot.rider_yaw_millidegrees.to_be_bytes());
            out.push(snapshot.stance.as_u8());
            out.extend_from_slice(&snapshot.rider_health.to_be_bytes());
            out.extend_from_slice(&snapshot.stamina_ticks.to_be_bytes());
            out.extend_from_slice(&snapshot.horse.entity_id.0.to_be_bytes());
            out.push(horse_class_code(snapshot.horse.class));
            out.push(horse_state_code(snapshot.horse.state));
            i32_array(&mut out, snapshot.horse.position_mm);
            i32_array(&mut out, snapshot.horse.velocity_mmps);
            out.extend_from_slice(&snapshot.horse.yaw_millidegrees.to_be_bytes());
            out.extend_from_slice(&snapshot.horse.health.to_be_bytes());
            direction(&mut out, snapshot.horse.bolt_away_direction);
            out.push(recall_state_code(snapshot.recall_state));
            option_tick(&mut out, snapshot.recall_ready_tick);
        }
        M3PeerPayloadV2::ShotCommand { command } => {
            out.push(6);
            out.extend_from_slice(&crate::canonical_payload_bytes(
                &crate::PeerPayload::ShotCommand {
                    command: command.clone(),
                },
            ));
        }
        M3PeerPayloadV2::ShotResult { result } => {
            out.push(7);
            out.extend_from_slice(&crate::canonical_payload_bytes(
                &crate::PeerPayload::ShotResult {
                    result: result.clone(),
                },
            ));
        }
        M3PeerPayloadV2::Authority { authority, epoch } => {
            out.push(8);
            out.extend_from_slice(authority.as_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
        }
        M3PeerPayloadV2::MigrationFragment {
            authority,
            epoch,
            state_hash,
            fragment_index,
            fragment_count,
            fragment,
        } => {
            out.push(9);
            out.extend_from_slice(authority.as_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
            out.extend_from_slice(state_hash);
            out.extend_from_slice(&fragment_index.to_be_bytes());
            out.extend_from_slice(&fragment_count.to_be_bytes());
            out.extend_from_slice(fragment.as_bytes());
        }
        M3PeerPayloadV2::Leave => out.push(10),
        M3PeerPayloadV2::MatchState { state } => {
            out.push(11);
            out.extend_from_slice(&state.authority_epoch.to_be_bytes());
            out.extend_from_slice(&state.lobby_seed.to_be_bytes());
            out.extend_from_slice(&state.current_tick.as_u64().to_be_bytes());
            out.extend_from_slice(&state.end_tick.as_u64().to_be_bytes());
            out.extend_from_slice(
                &u16::try_from(state.players.len())
                    .unwrap_or(u16::MAX)
                    .to_be_bytes(),
            );
            for row in &state.players {
                out.extend_from_slice(row.player_id.as_bytes());
                out.extend_from_slice(&row.score.to_be_bytes());
                out.extend_from_slice(&row.eliminations.to_be_bytes());
                out.extend_from_slice(&row.assists.to_be_bytes());
                out.extend_from_slice(&row.deaths.to_be_bytes());
                out.push(u8::from(row.alive));
                option_tick(&mut out, row.respawn_at_tick);
                option_tick(&mut out, row.respawn_speed_buff_end_tick);
                option_tick(&mut out, row.horse_buff_end_tick);
                match row.score_breakdown {
                    Some(breakdown) => {
                        out.push(1);
                        for points in breakdown {
                            out.extend_from_slice(&points.to_be_bytes());
                        }
                    }
                    None => out.push(0),
                }
            }
            match state.active_reveal {
                Some(reveal) => {
                    out.push(1);
                    out.extend_from_slice(reveal.player_id.as_bytes());
                    out.extend_from_slice(&reveal.started_tick.as_u64().to_be_bytes());
                    out.extend_from_slice(&reveal.end_tick.as_u64().to_be_bytes());
                }
                None => out.push(0),
            }
            match state.active_objective {
                Some(objective) => {
                    out.push(1);
                    out.extend_from_slice(&objective.objective_id.to_be_bytes());
                    out.push(objective_kind_code(objective.kind));
                    out.extend_from_slice(&objective.started_tick.as_u64().to_be_bytes());
                    out.extend_from_slice(&objective.end_tick.as_u64().to_be_bytes());
                    out.push(u8::from(objective.completed));
                    out.extend_from_slice(&objective.x_mm.to_be_bytes());
                    out.extend_from_slice(&objective.z_mm.to_be_bytes());
                }
                None => out.push(0),
            }
            out.push(u8::from(state.finished));
            match state.winner {
                Some(winner) => {
                    out.push(1);
                    out.extend_from_slice(winner.as_bytes());
                }
                None => out.push(0),
            }
        }
    }
    out
}

/// Splits one validated checkpoint into independently signed MTU-safe payloads.
pub fn fragment_m3_checkpoint(
    authority: PlayerId,
    epoch: u64,
    checkpoint: &M3MatchCheckpointV2,
) -> Result<Vec<M3PeerPayloadV2>, CodecError> {
    if checkpoint.combat.source_epoch.checked_add(1) != Some(epoch)
        || !checkpoint.is_bounded_and_canonical()
        || !checkpoint
            .combat
            .riders
            .iter()
            .any(|rider| rider.rider_player_id == authority)
    {
        return Err(noncanonical_payload());
    }
    let bytes =
        serde_json::to_vec(checkpoint).map_err(|error| CodecError::Malformed(error.to_string()))?;
    let fragment_count = bytes.len().div_ceil(M3_CHECKPOINT_FRAGMENT_BYTES);
    if fragment_count == 0 || fragment_count > MAX_M3_CHECKPOINT_FRAGMENTS {
        return Err(noncanonical_payload());
    }
    let fragment_count = u16::try_from(fragment_count).map_err(|_| noncanonical_payload())?;
    let state_hash: [u8; 32] = Sha256::digest(&bytes).into();
    Ok(bytes
        .chunks(M3_CHECKPOINT_FRAGMENT_BYTES)
        .enumerate()
        .map(|(index, bytes)| M3PeerPayloadV2::MigrationFragment {
            authority,
            epoch,
            state_hash,
            fragment_index: u16::try_from(index).expect("fragment count is u16-bounded"),
            fragment_count,
            fragment: M3CheckpointFragment(bytes.to_vec()),
        })
        .collect())
}

/// Reassembles a complete, hash-bound checkpoint without exposing partial state.
pub fn reassemble_m3_checkpoint(
    fragments: &[M3PeerPayloadV2],
) -> Result<(PlayerId, u64, M3MatchCheckpointV2), CodecError> {
    if fragments.is_empty() || fragments.len() > MAX_M3_CHECKPOINT_FRAGMENTS {
        return Err(noncanonical_payload());
    }
    let mut authority = None;
    let mut epoch = None;
    let mut state_hash = None;
    let mut fragment_count = None;
    let mut ordered = BTreeMap::new();
    for payload in fragments {
        validate_payload(payload)?;
        let M3PeerPayloadV2::MigrationFragment {
            authority: row_authority,
            epoch: row_epoch,
            state_hash: row_hash,
            fragment_index,
            fragment_count: row_count,
            fragment,
        } = payload
        else {
            return Err(noncanonical_payload());
        };
        if authority
            .replace(*row_authority)
            .is_some_and(|value| value != *row_authority)
            || epoch
                .replace(*row_epoch)
                .is_some_and(|value| value != *row_epoch)
            || state_hash
                .replace(*row_hash)
                .is_some_and(|value| value != *row_hash)
            || fragment_count
                .replace(*row_count)
                .is_some_and(|value| value != *row_count)
            || ordered
                .insert(*fragment_index, fragment.as_bytes())
                .is_some()
        {
            return Err(noncanonical_payload());
        }
    }
    let expected_count = fragment_count.ok_or_else(noncanonical_payload)?;
    if usize::from(expected_count) != fragments.len()
        || ordered.keys().copied().ne(0..expected_count)
    {
        return Err(noncanonical_payload());
    }
    let bytes = ordered
        .values()
        .flat_map(|fragment| fragment.iter().copied())
        .collect::<Vec<_>>();
    let expected_hash = state_hash.ok_or_else(noncanonical_payload)?;
    if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected_hash {
        return Err(noncanonical_payload());
    }
    let checkpoint: M3MatchCheckpointV2 =
        serde_json::from_slice(&bytes).map_err(|error| CodecError::Malformed(error.to_string()))?;
    let authority = authority.ok_or_else(noncanonical_payload)?;
    let epoch = epoch.ok_or_else(noncanonical_payload)?;
    if checkpoint.hash() != expected_hash
        || checkpoint.combat.source_epoch.checked_add(1) != Some(epoch)
        || !checkpoint.is_bounded_and_canonical()
        || !checkpoint
            .combat
            .riders
            .iter()
            .any(|rider| rider.rider_player_id == authority)
    {
        return Err(noncanonical_payload());
    }
    Ok((authority, epoch, checkpoint))
}

fn noncanonical_payload() -> CodecError {
    CodecError::Malformed("noncanonical wire-v2 payload".to_owned())
}

fn string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn i32_array(out: &mut Vec<u8>, values: [i32; 3]) {
    for value in values {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn direction(out: &mut Vec<u8>, value: QuantizedDirection) {
    out.extend_from_slice(&value.x.to_be_bytes());
    out.extend_from_slice(&value.y.to_be_bytes());
    out.extend_from_slice(&value.z.to_be_bytes());
}

fn option_tick(out: &mut Vec<u8>, value: Option<SimulationTick>) {
    match value {
        Some(tick) => {
            out.push(1);
            out.extend_from_slice(&tick.as_u64().to_be_bytes());
        }
        None => out.push(0),
    }
}

const fn horse_class_code(value: HorseVitalityClass) -> u8 {
    match value {
        HorseVitalityClass::Courser => 0,
        HorseVitalityClass::Warhorse => 1,
        HorseVitalityClass::Mustang => 2,
    }
}

const fn horse_state_code(value: HorseVitalityState) -> u8 {
    match value {
        HorseVitalityState::Available => 0,
        HorseVitalityState::Bolting => 1,
        HorseVitalityState::Despawned => 2,
    }
}

const fn recall_state_code(value: RecallState) -> u8 {
    match value {
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

const fn objective_kind_code(value: DynamicObjectiveKind) -> u8 {
    match value {
        DynamicObjectiveKind::MovingBounty => 0,
        DynamicObjectiveKind::SupplyHerd => 1,
        DynamicObjectiveKind::AmmoWagon => 2,
        DynamicObjectiveKind::SignalTower => 3,
        DynamicObjectiveKind::HorseBuffStation => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MatchCheckpoint, RiderCheckpoint};
    use spurfire_protocol::{
        ActorM3TickInput, M3AuthorityBank, M3ReloadCheckpointV2, OnFootTickInput, QuantizedOrigin,
        RiderStance, RosterHash, RosterManifestEntry, SessionSignature,
    };

    fn lobby() -> LobbyId {
        LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap()
    }

    fn player(last: u8) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{last:012x}")).unwrap()
    }

    fn snapshot() -> M3ActorSnapshot {
        M3ActorSnapshot {
            rider_player_id: player(2),
            rider_position_mm: [1, 2, 3],
            rider_velocity_mmps: [4, 5, 6],
            rider_yaw_millidegrees: 7_000,
            stance: M3ActorStance::Mounted,
            rider_health: 100,
            stamina_ticks: ON_FOOT_STAMINA_TICKS,
            horse: M3HorseSnapshot {
                entity_id: EntityId(22),
                class: HorseVitalityClass::Courser,
                state: HorseVitalityState::Available,
                position_mm: [8, 9, 10],
                velocity_mmps: [11, 12, 13],
                yaw_millidegrees: 14_000,
                health: 200,
                bolt_away_direction: QuantizedDirection::new(0, 0, DIRECTION_UNITS),
            },
            recall_state: RecallState::HorsePresent,
            recall_ready_tick: None,
            spur_meter: 0,
            charge_started_tick: None,
            charge_end_tick: None,
        }
    }

    fn match_state(epoch: u64, tick: u64, player_count: u8) -> M5MatchStateV2 {
        let mut kernel = spurfire_protocol::BountyMatchKernel::new(
            epoch,
            0,
            SimulationTick::new(0),
            (1..=player_count).map(player).collect(),
        )
        .unwrap();
        kernel.advance_tick(SimulationTick::new(tick)).unwrap();
        M5MatchStateV2::from_snapshot(&kernel.snapshot())
    }

    fn envelope(payload: M3PeerPayloadV2) -> M3EnvelopeV2 {
        M3EnvelopeV2 {
            wire_version: M3_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(2),
            sequence: 3,
            authority_epoch: 4,
            simulation_tick: 5,
            payload,
            session: Some(SessionBinding {
                network_generation: 6,
                session_generation: 7,
                roster_hash: RosterHash::from_bytes([8; 32]),
                signature: SessionSignature::from_bytes([9; 64]),
            }),
        }
    }

    fn migration_checkpoint_for(source_epoch: u64, players: &[PlayerId]) -> M3MatchCheckpointV2 {
        let mut gameplay = M3AuthorityBank::new(source_epoch);
        for (index, player_id) in players.iter().copied().enumerate() {
            assert!(gameplay.register_actor(
                player_id,
                EntityId(22 + u64::try_from(index).unwrap()),
                HorseVitalityClass::Courser,
            ));
        }
        let tick = SimulationTick::new(10);
        for player_id in players.iter().copied() {
            gameplay
                .advance_actor(
                    player_id,
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
                    },
                )
                .unwrap();
        }
        let mut bounty = spurfire_protocol::BountyMatchKernel::new(
            source_epoch,
            0,
            SimulationTick::new(0),
            players.to_vec(),
        )
        .unwrap();
        bounty.advance_tick(tick).unwrap();
        M3MatchCheckpointV2 {
            wire_version: M3_WIRE_VERSION,
            combat: MatchCheckpoint {
                source_epoch,
                tick: 10,
                riders: players
                    .iter()
                    .copied()
                    .map(|rider_player_id| RiderCheckpoint {
                        rider_player_id,
                        position_mm: [0; 3],
                        velocity_mmps: [0; 3],
                        yaw_millidegrees: 0,
                        stance: RiderStance::Mounted,
                        health: 100,
                        weapon_id: WeaponId::Dustwalker.as_u8(),
                        ammo_magazine: 30,
                        ammo_reserve: 120,
                        last_input_tick: 10,
                        last_shot_tick: None,
                        last_command_tick: None,
                        shot_index: 0,
                    })
                    .collect(),
                resolved_shots: Vec::new(),
            },
            gameplay: gameplay.checkpoint(),
            reloads: players
                .iter()
                .copied()
                .map(|rider_player_id| M3ReloadCheckpointV2 {
                    rider_player_id,
                    current_tick: Some(tick),
                    reload_held: false,
                    reload: None,
                })
                .collect(),
            next_horse_damage_sequence: 1,
            bounty: bounty.checkpoint(),
        }
    }

    fn migration_checkpoint(source_epoch: u64) -> M3MatchCheckpointV2 {
        migration_checkpoint_for(source_epoch, &[player(2)])
    }

    fn signing_key(value: u8) -> SigningKey {
        SigningKey::from_bytes(&[value; 32])
    }

    fn secure_session(local: PlayerId) -> M3SecureSession {
        let server = signing_key(9);
        let server_public = SessionPublicKey::from_bytes(server.verifying_key().to_bytes());
        let manifest = RosterManifest {
            lobby_id: lobby(),
            network_generation: 6,
            session_generation: 7,
            roster_revision: 8,
            entries: (1..=3)
                .map(|index| RosterManifestEntry {
                    player_id: player(index),
                    session_public_key: SessionPublicKey::from_bytes(
                        signing_key(index).verifying_key().to_bytes(),
                    ),
                    tailnet_address: format!("100.64.0.{index}").parse().unwrap(),
                    application_port: 7_777,
                    node_key: None,
                })
                .collect(),
        };
        let signature = SessionSignature::from_bytes(
            server
                .sign(&canonical_manifest_digest(server_public, &manifest))
                .to_bytes(),
        );
        let mut state = SessionState::new(lobby(), local, player(1), 0);
        for index in 1..=3 {
            state.add_peer(player(index), 0);
        }
        M3SecureSession::new(manifest, server_public, signature, state).unwrap()
    }

    fn source(index: u8) -> SocketAddr {
        format!("100.64.0.{index}:7777").parse().unwrap()
    }

    #[test]
    fn v2_snapshot_codec_and_signing_bytes_are_exactly_version_separated() {
        let envelope = envelope(M3PeerPayloadV2::ActorSnapshot {
            snapshot: snapshot(),
        });
        let encoded = encode_m3(&envelope).unwrap();
        assert_eq!(decode_m3(&encoded).unwrap(), envelope);
        assert!(envelope.signing_digest().is_some());
        assert_eq!(
            crate::decode(&encoded),
            Err(CodecError::IncompatibleVersion)
        );

        let mut wrong_version = envelope.clone();
        wrong_version.wire_version = crate::CURRENT_WIRE_VERSION;
        assert_eq!(
            encode_m3(&wrong_version),
            Err(CodecError::IncompatibleVersion)
        );
        let encoded_v1 = serde_json::to_vec(&wrong_version).unwrap();
        assert_eq!(decode_m3(&encoded_v1), Err(CodecError::IncompatibleVersion));

        let base = canonical_m3_payload_bytes(&envelope.payload);
        let mut changed = snapshot();
        changed.horse.health -= 1;
        assert_ne!(
            base,
            canonical_m3_payload_bytes(&M3PeerPayloadV2::ActorSnapshot { snapshot: changed })
        );
    }

    #[test]
    fn v2_input_rejects_diagonal_overdrive_ranges_and_reserved_bits() {
        let valid = M3ActorInput {
            throttle_milli: 1_000,
            steer_milli: -1_000,
            move_x_milli: 600,
            move_z_milli: 800,
            buttons: M3_INPUT_JUMP_PRESSED | M3_INPUT_CROUCH_PRESSED,
        };
        assert!(valid.is_canonical());
        assert!(encode_m3(&envelope(M3PeerPayloadV2::ActorInput { input: valid })).is_ok());
        for invalid in [
            M3ActorInput {
                move_x_milli: 601,
                ..valid
            },
            M3ActorInput {
                throttle_milli: 1_001,
                ..valid
            },
            M3ActorInput {
                buttons: valid.buttons | (1 << 15),
                ..valid
            },
        ] {
            assert!(!invalid.is_canonical());
            assert!(matches!(
                encode_m3(&envelope(M3PeerPayloadV2::ActorInput { input: invalid })),
                Err(CodecError::Malformed(_))
            ));
        }
    }

    #[test]
    fn v2_snapshot_cross_field_state_fails_closed() {
        let mut invalid = snapshot();
        invalid.horse.state = HorseVitalityState::Despawned;
        assert!(!invalid.is_canonical());
        invalid.horse.health = 0;
        invalid.recall_state = RecallState::CoolingDown;
        invalid.recall_ready_tick = Some(SimulationTick::new(100));
        assert!(invalid.is_canonical());
        invalid.stamina_ticks = ON_FOOT_STAMINA_TICKS + 1;
        assert!(!invalid.is_canonical());

        let bytes = serde_json::to_vec(&envelope(M3PeerPayloadV2::ActorSnapshot {
            snapshot: invalid,
        }))
        .unwrap();
        assert!(matches!(decode_m3(&bytes), Err(CodecError::Malformed(_))));
    }

    #[test]
    fn v2_bounds_datagrams_hostnames_and_migration_epochs() {
        assert_eq!(
            decode_m3(&vec![b'x'; MAX_DATAGRAM_BYTES + 1]),
            Err(CodecError::TooLarge)
        );
        assert!(matches!(
            encode_m3(&envelope(M3PeerPayloadV2::Hello {
                hostname: "x".repeat(256),
            })),
            Err(CodecError::Malformed(_))
        ));

        let checkpoint = migration_checkpoint(3);
        let mut fragments = fragment_m3_checkpoint(player(2), 4, &checkpoint).unwrap();
        assert!(fragments.len() > 1);
        for fragment in &fragments {
            let encoded = encode_m3(&envelope(fragment.clone())).unwrap();
            assert!(encoded.len() <= MAX_DATAGRAM_BYTES);
        }
        fragments.reverse();
        let (authority, epoch, restored) = reassemble_m3_checkpoint(&fragments).unwrap();
        assert_eq!((authority, epoch, restored), (player(2), 4, checkpoint));

        assert!(matches!(
            reassemble_m3_checkpoint(&fragments[1..]),
            Err(CodecError::Malformed(_))
        ));
        let mut corrupt = fragments.clone();
        if let M3PeerPayloadV2::MigrationFragment { fragment, .. } = &mut corrupt[0] {
            fragment.0[0] ^= 1;
        }
        assert!(matches!(
            reassemble_m3_checkpoint(&corrupt),
            Err(CodecError::Malformed(_))
        ));

        let overflow = migration_checkpoint(u64::MAX);
        assert!(matches!(
            fragment_m3_checkpoint(player(2), 0, &overflow),
            Err(CodecError::Malformed(_))
        ));
    }

    #[test]
    fn core_v2_payload_variants_have_distinct_canonical_tags() {
        let payloads = [
            M3PeerPayloadV2::Hello {
                hostname: "rider".into(),
            },
            M3PeerPayloadV2::Heartbeat,
            M3PeerPayloadV2::Probe {
                nonce: 1,
                reply: false,
            },
            M3PeerPayloadV2::ActorLoadout {
                loadout: M3ActorLoadout {
                    horse_class: HorseVitalityClass::Mustang,
                    weapon_id: WeaponId::Rattler,
                },
            },
            M3PeerPayloadV2::ActorInput {
                input: M3ActorInput {
                    throttle_milli: 0,
                    steer_milli: 0,
                    move_x_milli: 0,
                    move_z_milli: 0,
                    buttons: 0,
                },
            },
            M3PeerPayloadV2::ActorSnapshot {
                snapshot: snapshot(),
            },
            M3PeerPayloadV2::Authority {
                authority: player(1),
                epoch: 2,
            },
            M3PeerPayloadV2::Leave,
            M3PeerPayloadV2::MatchState {
                state: match_state(4, 5, 3),
            },
        ];
        let tags = payloads
            .iter()
            .map(|payload| canonical_m3_payload_bytes(payload)[0])
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(tags.len(), payloads.len());
    }

    #[test]
    fn eight_player_match_state_with_reveal_and_objective_stays_mtu_safe() {
        let mut state = match_state(4, 7_200, 8);
        assert!(state.active_reveal.is_some());
        assert!(state.active_objective.is_some());
        for (index, row) in state.players.iter_mut().enumerate() {
            row.horse_buff_end_tick = Some(SimulationTick::new(8_000));
            if index < 4 {
                row.alive = false;
                row.respawn_at_tick = Some(SimulationTick::new(7_500));
            } else {
                row.respawn_speed_buff_end_tick = Some(SimulationTick::new(7_800));
            }
        }
        assert!(state.is_canonical());
        let mut packet = envelope(M3PeerPayloadV2::MatchState { state });
        packet.simulation_tick = 7_200;
        let encoded = encode_m3(&packet).unwrap();
        assert!(encoded.len() <= MAX_DATAGRAM_BYTES, "{}", encoded.len());
        assert_eq!(decode_m3(&encoded).unwrap(), packet);
    }

    #[test]
    fn eight_player_final_results_with_score_categories_stay_mtu_safe() {
        let state = match_state(4, 54_000, 8);
        assert!(state.finished);
        assert!(state
            .players
            .iter()
            .all(|row| row.score_breakdown.is_some()));
        assert!(state.is_canonical());
        let mut packet = envelope(M3PeerPayloadV2::MatchState { state });
        packet.simulation_tick = 54_000;
        let encoded = encode_m3(&packet).unwrap();
        assert!(encoded.len() <= MAX_DATAGRAM_BYTES, "{}", encoded.len());
        assert_eq!(decode_m3(&encoded).unwrap(), packet);
    }

    #[test]
    fn match_state_is_authority_only_and_binds_epoch_tick_and_roster() {
        let mut authority = secure_session(player(1));
        let packet = authority
            .envelope(
                30,
                M3PeerPayloadV2::MatchState {
                    state: match_state(1, 30, 3),
                },
                &signing_key(1),
            )
            .unwrap();
        let mut follower = secure_session(player(2));
        assert_eq!(
            follower.accept_with_source(&packet, source(1), None, 30),
            AcceptOutcome::Accepted
        );
        assert!(secure_session(player(2))
            .envelope(
                30,
                M3PeerPayloadV2::MatchState {
                    state: match_state(1, 30, 3),
                },
                &signing_key(2),
            )
            .is_err());

        let mut wrong_tick = match_state(1, 30, 3);
        wrong_tick.current_tick = SimulationTick::new(29);
        assert!(secure_session(player(1))
            .envelope(
                30,
                M3PeerPayloadV2::MatchState { state: wrong_tick },
                &signing_key(1),
            )
            .is_err());
    }

    #[test]
    fn v2_secure_session_binds_source_signature_subject_and_replay() {
        let mut sender = secure_session(player(2));
        let mut receiver = secure_session(player(3));
        let input = M3ActorInput {
            throttle_milli: 500,
            steer_milli: -250,
            move_x_milli: 0,
            move_z_milli: 0,
            buttons: M3_INPUT_ADS_PRESSED,
        };
        let packet = sender
            .envelope(42, M3PeerPayloadV2::ActorInput { input }, &signing_key(2))
            .unwrap();
        let encoded = encode_m3(&packet).unwrap();
        let decoded = decode_m3(&encoded).unwrap();
        assert_eq!(
            receiver.accept_with_source(&decoded, source(2), None, 10),
            AcceptOutcome::Accepted
        );
        assert_eq!(
            receiver.accept_with_source(&decoded, source(2), None, 11),
            AcceptOutcome::DuplicateOrReplay
        );
        assert_eq!(
            secure_session(player(3)).accept_with_source(&decoded, source(1), None, 10),
            AcceptOutcome::EndpointMismatch
        );
        assert!(sender
            .envelope(
                43,
                M3PeerPayloadV2::ActorSnapshot {
                    snapshot: snapshot(),
                },
                &signing_key(2),
            )
            .is_err());

        let mut forged = decoded;
        if let M3PeerPayloadV2::ActorInput { input } = &mut forged.payload {
            input.steer_milli += 1;
        }
        assert_eq!(
            secure_session(player(3)).accept_with_source(&forged, source(2), None, 10),
            AcceptOutcome::BadSignature
        );
    }

    #[test]
    fn v2_secure_migration_installs_only_after_complete_out_of_order_fragments() {
        let mut sender = secure_session(player(2));
        let mut receiver = secure_session(player(3));
        let heartbeat = sender
            .envelope(9, M3PeerPayloadV2::Heartbeat, &signing_key(2))
            .unwrap();
        assert_eq!(
            receiver.accept_with_source(&heartbeat, source(2), None, 2_999),
            AcceptOutcome::Accepted
        );
        assert_eq!(sender.expire_and_migrate(3_000), Some((player(2), 2)));

        let checkpoint = migration_checkpoint_for(1, &[player(1), player(2), player(3)]);
        let mut envelopes = fragment_m3_checkpoint(player(2), 2, &checkpoint)
            .unwrap()
            .into_iter()
            .map(|payload| {
                sender
                    .envelope(checkpoint.combat.tick, payload, &signing_key(2))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(envelopes.len() > 1);
        envelopes.reverse();
        for envelope in &envelopes[..envelopes.len() - 1] {
            assert_eq!(
                receiver.accept_with_source(envelope, source(2), None, 3_000),
                AcceptOutcome::PendingMigration
            );
            assert_eq!(receiver.state().authority(), player(1));
            assert!(receiver.installed_checkpoint().is_none());
        }
        assert_eq!(
            receiver.accept_with_source(envelopes.last().unwrap(), source(2), None, 3_000),
            AcceptOutcome::Accepted
        );
        assert_eq!(receiver.state().authority(), player(2));
        assert_eq!(receiver.state().authority_epoch(), 2);
        assert_eq!(receiver.installed_checkpoint(), Some(&checkpoint));
        assert_eq!(receiver.take_installed_checkpoint(), Some(checkpoint));
        assert!(receiver.installed_checkpoint().is_none());

        let mut rejected = secure_session(player(3));
        assert_eq!(
            rejected.accept_with_source(&heartbeat, source(2), None, 2_999),
            AcceptOutcome::Accepted
        );
        for envelope in &envelopes[..envelopes.len() - 1] {
            assert_eq!(
                rejected.accept_with_source_validated(envelope, source(2), None, 3_000, |_| false,),
                AcceptOutcome::PendingMigration
            );
        }
        assert_eq!(
            rejected.accept_with_source_validated(
                envelopes.last().unwrap(),
                source(2),
                None,
                3_000,
                |_| false,
            ),
            AcceptOutcome::InvalidCheckpoint
        );
        assert_eq!(rejected.state().authority(), player(1));
        assert_eq!(rejected.state().authority_epoch(), 1);
        assert!(rejected.installed_checkpoint().is_none());
    }
}
