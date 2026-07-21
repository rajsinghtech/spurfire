//! Candidate wire-v2 actor replication contract for M3 activation.
//!
//! This module is deliberately separate from the active wire-1.2 codec. Its
//! types, validation, and canonical signing bytes must be integrated together.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use spurfire_protocol::{
    canonical_envelope_digest, EntityId, HorseVitalityClass, HorseVitalityState, LobbyId,
    M3ActorStance, PlayerId, QuantizedDirection, RecallState, SessionBinding, ShotCommand,
    ShotResult, SimulationTick, WeaponId, WireVersion, DIRECTION_UNITS, M3_WIRE_VERSION,
    ON_FOOT_STAMINA_TICKS,
};

use crate::{CodecError, M3MatchCheckpointV2, MAX_DATAGRAM_BYTES};

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
            || !self.horse.is_canonical()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MatchCheckpoint, RiderCheckpoint};
    use spurfire_protocol::{M3AuthorityBank, RiderStance, RosterHash, SessionSignature};

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
        }
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

    fn migration_checkpoint(source_epoch: u64) -> M3MatchCheckpointV2 {
        let mut gameplay = M3AuthorityBank::new(source_epoch);
        assert!(gameplay.register_actor(player(2), EntityId(22), HorseVitalityClass::Courser,));
        M3MatchCheckpointV2 {
            wire_version: M3_WIRE_VERSION,
            combat: MatchCheckpoint {
                source_epoch,
                tick: 10,
                riders: vec![RiderCheckpoint {
                    rider_player_id: player(2),
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
                }],
                resolved_shots: Vec::new(),
            },
            gameplay: gameplay.checkpoint(),
            next_horse_damage_sequence: 1,
        }
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
        ];
        let tags = payloads
            .iter()
            .map(|payload| canonical_m3_payload_bytes(payload)[0])
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(tags.len(), payloads.len());
    }
}
