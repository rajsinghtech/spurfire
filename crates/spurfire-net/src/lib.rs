#![forbid(unsafe_code)]
//! Bounded, transport-independent peer-session protocol for Spurfire.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spurfire_protocol::{
    canonical_envelope_digest, canonical_manifest_digest, LobbyId, M3AuthorityCheckpointV2,
    NodeKey, PlayerId, RiderStance, RosterHash, RosterManifest, SessionBinding,
    SessionIdentityError, SessionPublicKey, SessionSignature, ShotCommand, ShotResult,
    SimulationTick, WeaponId, WireVersion, CURRENT_WIRE_VERSION, M3_WIRE_VERSION,
};
use thiserror::Error;

pub mod replication;
#[cfg(feature = "rustscale")]
pub mod rustscale;
pub mod v2;

pub const MAX_DATAGRAM_BYTES: usize = 1_200;
pub const HEARTBEAT_TIMEOUT_MS: u64 = 3_000;
pub const RECONNECT_GRACE_MS: u64 = 5_000;
/// Maximum invited-friends roster represented by one M2 checkpoint.
pub const MAX_CHECKPOINT_RIDERS: usize = 16;
/// Every rider target currently has a fixed 100-point combat health ceiling.
pub const MAX_CHECKPOINT_RIDER_HEALTH: u16 = 100;
/// Existing mounted-jump edge bit, now formally assigned.
pub const RIDER_INPUT_JUMP_PRESSED: u16 = 1 << 0;
/// M2 dismount/remount E edge bit.
pub const RIDER_INPUT_INTERACT_PRESSED: u16 = 1 << 1;
/// Every other input bit is reserved and must remain zero in wire 1.1.
pub const RIDER_INPUT_RESERVED_MASK: u16 =
    !(RIDER_INPUT_JUMP_PRESSED | RIDER_INPUT_INTERACT_PRESSED);

/// Bounded authority-owned state retained by every peer for migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiderCheckpoint {
    #[serde(rename = "p", alias = "rider_player_id")]
    pub rider_player_id: PlayerId,
    #[serde(rename = "x", alias = "position_mm")]
    pub position_mm: [i32; 3],
    #[serde(rename = "v", alias = "velocity_mmps")]
    pub velocity_mmps: [i32; 3],
    #[serde(rename = "y", alias = "yaw_millidegrees")]
    pub yaw_millidegrees: i32,
    #[serde(rename = "s", alias = "stance")]
    pub stance: RiderStance,
    #[serde(rename = "h", alias = "health")]
    pub health: u16,
    #[serde(rename = "w", alias = "weapon_id")]
    pub weapon_id: u8,
    #[serde(rename = "m", alias = "ammo_magazine")]
    pub ammo_magazine: u16,
    #[serde(rename = "r", alias = "ammo_reserve")]
    pub ammo_reserve: u16,
    #[serde(rename = "i", alias = "last_input_tick")]
    pub last_input_tick: u64,
    #[serde(rename = "f", alias = "last_shot_tick")]
    pub last_shot_tick: Option<u64>,
    #[serde(default, rename = "c", alias = "last_command_tick")]
    pub last_command_tick: Option<u64>,
    #[serde(default, rename = "n", alias = "shot_index")]
    pub shot_index: u64,
}

/// Complete bounded M2 handoff. Combat receipts prevent damage/ammo replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchCheckpoint {
    #[serde(rename = "e", alias = "source_epoch")]
    pub source_epoch: u64,
    #[serde(rename = "t", alias = "tick")]
    pub tick: u64,
    #[serde(rename = "r", alias = "riders")]
    pub riders: Vec<RiderCheckpoint>,
    #[serde(rename = "d", alias = "resolved_shots")]
    pub resolved_shots: Vec<(PlayerId, u64)>,
}

impl MatchCheckpoint {
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("checkpoint serialization cannot fail");
        Sha256::digest(bytes).into()
    }

    #[must_use]
    pub fn is_bounded_and_canonical(&self) -> bool {
        if self.riders.is_empty()
            || self.riders.len() > MAX_CHECKPOINT_RIDERS
            || self.resolved_shots.len() > MAX_CHECKPOINT_RIDERS * 64
            || self.riders.iter().any(|rider| {
                let weapon = WeaponId::try_from(i64::from(rider.weapon_id)).ok();
                !rider.stance.is_canonical()
                    || rider.health > MAX_CHECKPOINT_RIDER_HEALTH
                    || weapon.is_none()
                    || weapon.is_some_and(|weapon| {
                        let stats = weapon.stats();
                        rider.ammo_magazine > stats.magazine_capacity
                            || rider.ammo_reserve > stats.reserve_capacity
                    })
                    || rider.last_shot_tick.is_some() != (rider.shot_index > 0)
                    || (rider.last_shot_tick.is_some() && rider.last_command_tick.is_none())
                    || rider.last_command_tick.is_some_and(|command| {
                        rider.last_shot_tick.is_some_and(|shot| shot > command)
                    })
            })
        {
            return false;
        }
        let rider_ids = self
            .riders
            .iter()
            .map(|rider| rider.rider_player_id)
            .collect::<BTreeSet<_>>();
        let receipts = self.resolved_shots.iter().copied().collect::<BTreeSet<_>>();
        rider_ids.len() == self.riders.len() && receipts.len() == self.resolved_shots.len()
    }
}

/// Complete wire-v2 migration state. Live M3 sessions encode this type; the
/// retained wire-1.2 proof codec cannot interpret it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct M3MatchCheckpointV2 {
    /// Exact M3 schema boundary.
    #[serde(rename = "v", alias = "wire_version")]
    pub wire_version: WireVersion,
    /// Existing movement/combat/ammo/receipt state.
    #[serde(rename = "c", alias = "combat")]
    pub combat: MatchCheckpoint,
    /// Complete horse/on-foot/recall actor bank.
    #[serde(rename = "g", alias = "gameplay")]
    pub gameplay: M3AuthorityCheckpointV2,
    /// Next authority-global horse damage receipt sequence.
    #[serde(rename = "n", alias = "next_horse_damage_sequence")]
    pub next_horse_damage_sequence: u64,
}

impl M3MatchCheckpointV2 {
    /// Canonical digest signed by the future wire-v2 migration envelope.
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("checkpoint serialization cannot fail");
        Sha256::digest(bytes).into()
    }

    /// Validates schema, epoch, bounded canonical rosters, and sequence state.
    #[must_use]
    pub fn is_bounded_and_canonical(&self) -> bool {
        if self.wire_version != M3_WIRE_VERSION
            || self.gameplay.wire_version() != M3_WIRE_VERSION
            || self.combat.source_epoch != self.gameplay.source_authority_epoch()
            || self.next_horse_damage_sequence == 0
            || !self.combat.is_bounded_and_canonical()
            || self.gameplay.actors().is_empty()
            || self.gameplay.actors().len() != self.combat.riders.len()
            || self
                .gameplay
                .actors()
                .iter()
                .any(|row| row.actor.current_tick() != Some(SimulationTick::new(self.combat.tick)))
        {
            return false;
        }
        self.combat
            .riders
            .iter()
            .zip(self.gameplay.actors())
            .all(|(rider, actor)| {
                rider.rider_player_id == actor.rider_player_id
                    && match actor.actor.mode() {
                        spurfire_protocol::ActorM3Mode::Mounted => matches!(
                            rider.stance,
                            RiderStance::Mounted
                                | RiderStance::MountedAirborne
                                | RiderStance::SaddleDiveAirborne
                                | RiderStance::LandingProne
                                | RiderStance::LandingRecovery
                        ),
                        spurfire_protocol::ActorM3Mode::SpookStunned
                        | spurfire_protocol::ActorM3Mode::OnFoot
                        | spurfire_protocol::ActorM3Mode::ReturningHorse => {
                            rider.stance == RiderStance::OnFootStanding
                        }
                    }
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerPayload {
    Hello {
        hostname: String,
    },
    Heartbeat,
    Probe {
        nonce: u64,
        reply: bool,
    },
    RiderInput {
        throttle_milli: i16,
        steer_milli: i16,
        buttons: u16,
    },
    RiderSnapshot {
        /// Authority-owned actor represented by this snapshot. Pre-1.2 packets
        /// are rebound to the envelope sender during decode.
        #[serde(default = "legacy_snapshot_subject")]
        rider_player_id: PlayerId,
        position_mm: [i32; 3],
        velocity_mmps: [i32; 3],
        yaw_millidegrees: i32,
        /// Added in wire 1.1. Missing 1.0 fields represent mounted riders.
        #[serde(default = "legacy_mounted_stance")]
        stance: RiderStance,
    },
    ShotCommand {
        command: ShotCommand,
    },
    ShotResult {
        result: ShotResult,
    },
    Authority {
        authority: PlayerId,
        epoch: u64,
    },
    MigrationSnapshot {
        #[serde(rename = "a")]
        authority: PlayerId,
        #[serde(rename = "e")]
        epoch: u64,
        #[serde(rename = "c")]
        checkpoint: MatchCheckpoint,
        #[serde(rename = "h")]
        state_hash: [u8; 32],
    },
    Leave,
}

/// Backward-compatible missing-field default for pre-M2 rider snapshots.
#[must_use]
pub const fn legacy_mounted_stance() -> RiderStance {
    RiderStance::Mounted
}

fn legacy_snapshot_subject() -> PlayerId {
    PlayerId::parse("ffffffff-ffff-4fff-bfff-ffffffffffff")
        .expect("legacy snapshot sentinel is a UUIDv4")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub wire_version: WireVersion,
    pub lobby_id: LobbyId,
    pub sender: PlayerId,
    pub sequence: u64,
    pub authority_epoch: u64,
    pub simulation_tick: u64,
    pub payload: PeerPayload,
    /// Wire 1.2 application identity. Absent only in explicit insecure demo/test mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionBinding>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("peer datagram exceeds {MAX_DATAGRAM_BYTES} bytes")]
    TooLarge,
    #[error("peer datagram is malformed: {0}")]
    Malformed(String),
    #[error("incompatible wire version")]
    IncompatibleVersion,
}

pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, CodecError> {
    validate_local_payload(&envelope.payload)?;
    let encoded =
        serde_json::to_vec(envelope).map_err(|error| CodecError::Malformed(error.to_string()))?;
    if encoded.len() > MAX_DATAGRAM_BYTES {
        return Err(CodecError::TooLarge);
    }
    Ok(encoded)
}

pub fn decode(bytes: &[u8]) -> Result<Envelope, CodecError> {
    if bytes.len() > MAX_DATAGRAM_BYTES {
        return Err(CodecError::TooLarge);
    }
    #[derive(Deserialize)]
    struct VersionHeader {
        #[serde(alias = "v")]
        wire_version: WireVersion,
    }
    let header: VersionHeader =
        serde_json::from_slice(bytes).map_err(|error| CodecError::Malformed(error.to_string()))?;
    if !CURRENT_WIRE_VERSION.is_compatible_with(header.wire_version) {
        return Err(CodecError::IncompatibleVersion);
    }
    let mut envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|error| CodecError::Malformed(error.to_string()))?;
    if envelope.wire_version.minor() < 2 {
        if let PeerPayload::RiderSnapshot {
            rider_player_id, ..
        } = &mut envelope.payload
        {
            if *rider_player_id == legacy_snapshot_subject() {
                *rider_player_id = envelope.sender;
            }
        }
    }
    validate_remote_payload(&envelope.payload, envelope.wire_version)?;
    Ok(envelope)
}

fn validate_local_payload(payload: &PeerPayload) -> Result<(), CodecError> {
    validate_stance(payload)?;
    if matches!(
        payload,
        PeerPayload::RiderInput { buttons, .. } if *buttons & RIDER_INPUT_RESERVED_MASK != 0
    ) {
        return Err(CodecError::Malformed(
            "rider input contains reserved button bits".to_owned(),
        ));
    }
    Ok(())
}

fn validate_remote_payload(
    payload: &PeerPayload,
    sender_version: WireVersion,
) -> Result<(), CodecError> {
    validate_stance(payload)?;
    // Wire 1.1 requires its unassigned bits to be zero. A newer same-major
    // sender may assign those bits additively; old readers retain the packet
    // even though they do not interpret the future input capability.
    if sender_version.minor() <= CURRENT_WIRE_VERSION.minor()
        && matches!(
            payload,
            PeerPayload::RiderInput { buttons, .. } if *buttons & RIDER_INPUT_RESERVED_MASK != 0
        )
    {
        return Err(CodecError::Malformed(
            "rider input contains reserved button bits".to_owned(),
        ));
    }
    Ok(())
}

fn validate_stance(payload: &PeerPayload) -> Result<(), CodecError> {
    if matches!(
        payload,
        PeerPayload::RiderSnapshot { stance, .. } if !stance.is_canonical()
    ) {
        return Err(CodecError::Malformed(
            "rider snapshot contains a noncanonical stance".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptOutcome {
    Accepted,
    DuplicateOrReplay,
    WrongLobby,
    StaleAuthorityEpoch,
    UnknownSender,
    UnsignedInSecureMode,
    EndpointMismatch,
    NodeKeyMismatch,
    WrongGeneration,
    RosterMismatch,
    BadSignature,
    InvalidAuthorityClaim,
    InvalidPayloadRole,
    InvalidPayloadSubject,
    InvalidCheckpoint,
    DuplicateShotResult,
    PendingMigration,
}

#[derive(Clone, Debug)]
struct PeerState {
    last_sequence: u64,
    last_seen_ms: u64,
    connected: bool,
}

/// Deterministic membership, replay, heartbeat, and authority-epoch state.
#[derive(Clone, Debug)]
pub struct SessionState {
    lobby_id: LobbyId,
    local_player: PlayerId,
    authority: PlayerId,
    authority_epoch: u64,
    next_sequence: u64,
    peers: BTreeMap<PlayerId, PeerState>,
    applied_shot_results: BTreeSet<(u64, PlayerId, u64)>,
    checkpoint: Option<MatchCheckpoint>,
}

impl SessionState {
    #[must_use]
    pub fn new(
        lobby_id: LobbyId,
        local_player: PlayerId,
        authority: PlayerId,
        now_ms: u64,
    ) -> Self {
        let mut peers = BTreeMap::new();
        for player in [local_player, authority] {
            peers.entry(player).or_insert(PeerState {
                last_sequence: 0,
                last_seen_ms: now_ms,
                connected: true,
            });
        }
        Self {
            lobby_id,
            local_player,
            authority,
            authority_epoch: 1,
            next_sequence: 1,
            peers,
            applied_shot_results: BTreeSet::new(),
            checkpoint: None,
        }
    }

    #[must_use]
    pub const fn authority(&self) -> PlayerId {
        self.authority
    }
    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    #[must_use]
    pub fn checkpoint(&self) -> Option<&MatchCheckpoint> {
        self.checkpoint.as_ref()
    }

    #[must_use]
    pub fn has_applied_shot_result(&self, epoch: u64, shooter: PlayerId, tick: u64) -> bool {
        self.applied_shot_results.contains(&(epoch, shooter, tick))
    }

    pub fn add_peer(&mut self, player: PlayerId, now_ms: u64) {
        self.peers.insert(
            player,
            PeerState {
                last_sequence: 0,
                last_seen_ms: now_ms,
                connected: true,
            },
        );
    }

    pub fn envelope(&mut self, tick: u64, payload: PeerPayload) -> Envelope {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: self.lobby_id,
            sender: self.local_player,
            sequence,
            authority_epoch: self.authority_epoch,
            simulation_tick: tick,
            payload,
            session: None,
        }
    }

    pub fn accept(&mut self, envelope: &Envelope, now_ms: u64) -> AcceptOutcome {
        if envelope.lobby_id != self.lobby_id {
            return AcceptOutcome::WrongLobby;
        }
        if envelope.authority_epoch < self.authority_epoch {
            return AcceptOutcome::StaleAuthorityEpoch;
        }
        if !self.peers.contains_key(&envelope.sender) {
            return AcceptOutcome::UnknownSender;
        }
        let authority_claim = matches!(
            envelope.payload,
            PeerPayload::Authority { .. } | PeerPayload::MigrationSnapshot { .. }
        );
        // Ordinary gameplay traffic belongs to the installed epoch. Only a
        // coherent authority claim may introduce the next epoch.
        if envelope.authority_epoch != self.authority_epoch && !authority_claim {
            return AcceptOutcome::InvalidPayloadRole;
        }
        // Snapshots and resolved combat truth are authority-only. Signing proves
        // roster identity, not permission to speak for the authority.
        if matches!(
            envelope.payload,
            PeerPayload::RiderSnapshot { .. } | PeerPayload::ShotResult { .. }
        ) && envelope.sender != self.authority
        {
            return AcceptOutcome::InvalidPayloadRole;
        }
        // Subject and tick bindings are checked before replay/liveness mutation.
        if matches!(
            envelope.payload,
            PeerPayload::ShotCommand { ref command }
                if command.shooter_peer_id != envelope.sender
                    || command.tick.as_u64() != envelope.simulation_tick
        ) {
            return AcceptOutcome::InvalidPayloadSubject;
        }
        if matches!(
            envelope.payload,
            PeerPayload::ShotResult { ref result }
                if result.tick.as_u64() != envelope.simulation_tick
                    || !self.peers.contains_key(&result.shooter_peer_id)
        ) {
            return AcceptOutcome::InvalidPayloadSubject;
        }
        if matches!(
            envelope.payload,
            PeerPayload::RiderSnapshot { rider_player_id, .. }
                if !self.peers.contains_key(&rider_player_id)
        ) {
            return AcceptOutcome::InvalidPayloadSubject;
        }
        if matches!(
            envelope.payload,
            PeerPayload::ShotResult { ref result }
                if self.applied_shot_results.contains(&(
                    envelope.authority_epoch,
                    result.shooter_peer_id,
                    result.tick.as_u64(),
                ))
        ) {
            return AcceptOutcome::DuplicateShotResult;
        }
        if let PeerPayload::MigrationSnapshot {
            authority,
            epoch,
            ref checkpoint,
            state_hash,
        } = envelope.payload
        {
            let expected_epoch = checkpoint.source_epoch.checked_add(1);
            let epoch_window = checkpoint.source_epoch == self.authority_epoch
                || (expected_epoch == Some(self.authority_epoch) && epoch == self.authority_epoch);
            if authority != envelope.sender
                || expected_epoch != Some(epoch)
                || !epoch_window
                || checkpoint.tick != envelope.simulation_tick
                || checkpoint.hash() != state_hash
                || !checkpoint.is_bounded_and_canonical()
                || checkpoint
                    .riders
                    .iter()
                    .any(|rider| !self.peers.contains_key(&rider.rider_player_id))
            {
                return AcceptOutcome::InvalidCheckpoint;
            }
        }
        if envelope.sequence <= self.peers[&envelope.sender].last_sequence {
            return AcceptOutcome::DuplicateOrReplay;
        }
        if let PeerPayload::Authority { authority, epoch }
        | PeerPayload::MigrationSnapshot {
            authority, epoch, ..
        } = envelope.payload
        {
            // Authority claims are validated before any replay, liveness, or
            // authority state can mutate, so a forged claim is fully inert.
            if !self.authority_claim_is_coherent(envelope.sender, authority, epoch, now_ms) {
                return AcceptOutcome::InvalidAuthorityClaim;
            }
        }
        let peer = self
            .peers
            .get_mut(&envelope.sender)
            .expect("sender membership was checked above");
        peer.last_sequence = envelope.sequence;
        peer.last_seen_ms = now_ms;
        peer.connected = !matches!(envelope.payload, PeerPayload::Leave);
        if let PeerPayload::ShotResult { ref result } = envelope.payload {
            self.applied_shot_results.insert((
                envelope.authority_epoch,
                result.shooter_peer_id,
                result.tick.as_u64(),
            ));
        }
        if let PeerPayload::MigrationSnapshot { ref checkpoint, .. } = envelope.payload {
            self.applied_shot_results.extend(
                checkpoint
                    .resolved_shots
                    .iter()
                    .map(|(shooter, tick)| (checkpoint.source_epoch, *shooter, *tick)),
            );
            self.checkpoint = Some(checkpoint.clone());
        }
        if let PeerPayload::Authority { authority, epoch }
        | PeerPayload::MigrationSnapshot {
            authority, epoch, ..
        } = envelope.payload
        {
            if epoch > self.authority_epoch
                || (epoch == self.authority_epoch && authority < self.authority)
            {
                self.authority = authority;
                self.authority_epoch = epoch;
            }
        }
        AcceptOutcome::Accepted
    }

    /// Returns whether a signed authority claim may mutate session authority.
    ///
    /// A claim is coherent only when it is a self-nomination (`authority` is the
    /// envelope sender), and one of:
    /// - an exact one-epoch advance (`epoch == current + 1`) while the current
    ///   authority is silent in the receiver's own view, mirroring the local
    ///   deterministic election precondition — the local player always counts
    ///   as fresh, so a live authority fails closed against remote usurpation;
    /// - a same-epoch claim converging split elections toward the lowest
    ///   `PlayerId`, which never invalidates traffic at the current epoch.
    ///
    /// Anything else — third-party installs, epoch jumps such as `u64::MAX`,
    /// or advances over a demonstrably live authority — is rejected so a
    /// signed roster member cannot freeze legitimate lower-epoch traffic.
    fn authority_claim_is_coherent(
        &self,
        sender: PlayerId,
        authority: PlayerId,
        epoch: u64,
        now_ms: u64,
    ) -> bool {
        if authority != sender {
            return false;
        }
        if epoch == self.authority_epoch {
            return authority == self.authority
                || (authority < self.authority && self.current_authority_is_silent(now_ms));
        }
        if self.authority_epoch.checked_add(1) != Some(epoch)
            || !self.current_authority_is_silent(now_ms)
        {
            return false;
        }
        self.deterministic_successor(now_ms) == Some(authority)
    }

    fn deterministic_successor(&self, now_ms: u64) -> Option<PlayerId> {
        self.peers
            .iter()
            .filter_map(|(player, peer)| {
                (*player == self.local_player
                    || (peer.connected
                        && now_ms.saturating_sub(peer.last_seen_ms) < HEARTBEAT_TIMEOUT_MS))
                    .then_some(*player)
            })
            .min()
    }

    fn current_authority_is_silent(&self, now_ms: u64) -> bool {
        self.authority != self.local_player
            && self
                .peers
                .get(&self.authority)
                .is_none_or(|peer| now_ms.saturating_sub(peer.last_seen_ms) >= HEARTBEAT_TIMEOUT_MS)
    }

    /// Expires silent peers and deterministically elects the smallest connected player ID.
    pub fn expire_and_migrate(&mut self, now_ms: u64) -> Option<(PlayerId, u64)> {
        for (player, peer) in &mut self.peers {
            if *player == self.local_player {
                peer.connected = true;
            } else if now_ms.saturating_sub(peer.last_seen_ms) >= HEARTBEAT_TIMEOUT_MS {
                peer.connected = false;
            }
        }
        if self
            .peers
            .get(&self.authority)
            .is_some_and(|peer| peer.connected)
        {
            return None;
        }
        let successor = self
            .peers
            .iter()
            .filter_map(|(id, peer)| peer.connected.then_some(*id))
            .min()?;
        let next_epoch = self.authority_epoch.checked_add(1)?;
        self.authority = successor;
        self.authority_epoch = next_epoch;
        Some((successor, self.authority_epoch))
    }
}

/// A verified complete roster plus replay/epoch state. All identity and source
/// checks occur before `SessionState` can mutate.
#[derive(Clone, Debug)]
pub struct SecureSession {
    manifest: RosterManifest,
    roster_hash: RosterHash,
    state: SessionState,
}

impl SecureSession {
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

    pub fn envelope(
        &mut self,
        tick: u64,
        payload: PeerPayload,
        signing_key: &SigningKey,
    ) -> Result<Envelope, SessionIdentityError> {
        if matches!(
            payload,
            PeerPayload::RiderSnapshot { .. } | PeerPayload::ShotResult { .. }
        ) && self.state.local_player != self.state.authority
        {
            return Err(SessionIdentityError::BadSignature);
        }
        if matches!(
            payload,
            PeerPayload::ShotCommand { ref command }
                if command.shooter_peer_id != self.state.local_player
                    || command.tick.as_u64() != tick
        ) || matches!(
            payload,
            PeerPayload::ShotResult { ref result } if result.tick.as_u64() != tick
        ) {
            return Err(SessionIdentityError::BadSignature);
        }
        let mut envelope = self.state.envelope(tick, payload);
        self.sign(&mut envelope, signing_key)?;
        Ok(envelope)
    }

    pub fn sign(
        &self,
        envelope: &mut Envelope,
        signing_key: &SigningKey,
    ) -> Result<(), SessionIdentityError> {
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
        let digest = envelope_digest(
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
        self.state.expire_and_migrate(now_ms)
    }

    pub fn accept_with_source(
        &mut self,
        envelope: &Envelope,
        source: SocketAddr,
        current_node_key: Option<NodeKey>,
        now_ms: u64,
    ) -> AcceptOutcome {
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
        let digest = envelope_digest(
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
        self.state.accept(envelope, now_ms)
    }
}

fn envelope_digest(
    envelope: &Envelope,
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
        &canonical_payload_bytes(&envelope.payload),
    )
}

/// Explicit fixed-layout payload bytes used only as signature input.
#[must_use]
pub fn canonical_payload_bytes(payload: &PeerPayload) -> Vec<u8> {
    fn string(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    fn option<T>(out: &mut Vec<u8>, value: &Option<T>, write: impl FnOnce(&mut Vec<u8>, &T)) {
        out.push(u8::from(value.is_some()));
        if let Some(value) = value {
            write(out, value);
        }
    }
    fn direction(out: &mut Vec<u8>, value: &spurfire_protocol::QuantizedDirection) {
        out.extend_from_slice(&value.x.to_be_bytes());
        out.extend_from_slice(&value.y.to_be_bytes());
        out.extend_from_slice(&value.z.to_be_bytes());
    }
    let mut out = Vec::with_capacity(160);
    match payload {
        PeerPayload::Hello { hostname } => {
            out.push(0);
            string(&mut out, hostname);
        }
        PeerPayload::Heartbeat => out.push(1),
        PeerPayload::Probe { nonce, reply } => {
            out.push(2);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.push(u8::from(*reply));
        }
        PeerPayload::RiderInput {
            throttle_milli,
            steer_milli,
            buttons,
        } => {
            out.push(3);
            out.extend_from_slice(&throttle_milli.to_be_bytes());
            out.extend_from_slice(&steer_milli.to_be_bytes());
            out.extend_from_slice(&buttons.to_be_bytes());
        }
        PeerPayload::RiderSnapshot {
            rider_player_id,
            position_mm,
            velocity_mmps,
            yaw_millidegrees,
            stance,
        } => {
            out.push(4);
            out.extend_from_slice(rider_player_id.as_bytes());
            for value in position_mm.iter().chain(velocity_mmps) {
                out.extend_from_slice(&value.to_be_bytes());
            }
            out.extend_from_slice(&yaw_millidegrees.to_be_bytes());
            out.push(stance.as_u8());
        }
        PeerPayload::ShotCommand { command } => {
            out.push(5);
            out.extend_from_slice(&command.tick.as_u64().to_be_bytes());
            out.extend_from_slice(command.shooter_peer_id.as_bytes());
            out.push(command.weapon_id.as_u8());
            for value in [command.origin.x, command.origin.y, command.origin.z] {
                out.extend_from_slice(&value.to_be_bytes());
            }
            direction(&mut out, &command.direction);
            out.extend_from_slice(&command.spread_seed.to_be_bytes());
            option(&mut out, &command.claimed_target, |out, target| {
                out.extend_from_slice(&target.target_id.0.to_be_bytes());
                option(out, &target.hit_zone, |out, zone| {
                    string(out, zone.as_str())
                });
                option(out, &target.damage, |out, value| {
                    out.extend_from_slice(&value.to_be_bytes())
                });
                option(out, &target.distance_mm, |out, value| {
                    out.extend_from_slice(&value.to_be_bytes())
                });
            });
        }
        PeerPayload::ShotResult { result } => {
            out.push(6);
            out.extend_from_slice(&result.tick.as_u64().to_be_bytes());
            out.extend_from_slice(result.shooter_peer_id.as_bytes());
            out.push(result.weapon_id.as_u8());
            string(&mut out, result.outcome.as_str());
            option(&mut out, &result.rejection_reason, |out, reason| {
                string(out, reason.as_str())
            });
            option(&mut out, &result.resolved_direction, direction);
            option(&mut out, &result.target_id, |out, id| {
                out.extend_from_slice(&id.0.to_be_bytes())
            });
            option(&mut out, &result.hit_zone, |out, zone| {
                string(out, zone.as_str())
            });
            out.extend_from_slice(&result.damage.to_be_bytes());
            option(&mut out, &result.distance_mm, |out, value| {
                out.extend_from_slice(&value.to_be_bytes())
            });
            out.push(u8::from(result.eliminated));
        }
        PeerPayload::Authority { authority, epoch } => {
            out.push(7);
            out.extend_from_slice(authority.as_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
        }
        PeerPayload::MigrationSnapshot {
            authority,
            epoch,
            checkpoint,
            state_hash,
        } => {
            out.push(8);
            out.extend_from_slice(authority.as_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
            out.extend_from_slice(&checkpoint.source_epoch.to_be_bytes());
            out.extend_from_slice(&checkpoint.tick.to_be_bytes());
            out.extend_from_slice(&state_hash[..]);
            out.extend_from_slice(
                &serde_json::to_vec(checkpoint).expect("checkpoint serialization cannot fail"),
            );
        }
        PeerPayload::Leave => out.push(9),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use spurfire_protocol::{
        ActorM3TickInput, EntityId, HorseVitalityClass, M3AuthorityBank, OnFootTickInput,
        QuantizedOrigin,
    };

    fn lobby() -> LobbyId {
        LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap()
    }
    fn player(last: u8) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{last:012x}")).unwrap()
    }

    #[test]
    fn codec_is_bounded_and_round_trips() {
        let mut state = SessionState::new(lobby(), player(2), player(1), 0);
        let envelope = state.envelope(
            42,
            PeerPayload::Hello {
                hostname: "rider-a".into(),
            },
        );
        assert_eq!(decode(&encode(&envelope).unwrap()).unwrap(), envelope);
        assert_eq!(
            decode(&vec![b'x'; MAX_DATAGRAM_BYTES + 1]),
            Err(CodecError::TooLarge)
        );
    }

    #[test]
    fn m3_checkpoint_binds_combat_and_gameplay_rosters_epochs_and_digest() {
        let mut gameplay = M3AuthorityBank::new(3);
        assert!(gameplay.register_actor(player(1), EntityId(201), HorseVitalityClass::Courser,));
        assert!(gameplay.register_actor(player(2), EntityId(202), HorseVitalityClass::Warhorse,));
        let tick = SimulationTick::new(10);
        for player_id in [player(1), player(2)] {
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
                        rider_position: QuantizedOrigin::default(),
                        return_horse_position: QuantizedOrigin::default(),
                        return_horse_moving: false,
                    },
                )
                .unwrap();
        }
        let rider = |rider_player_id| RiderCheckpoint {
            rider_player_id,
            position_mm: [0; 3],
            velocity_mmps: [0; 3],
            yaw_millidegrees: 0,
            stance: RiderStance::Mounted,
            health: 100,
            weapon_id: 0,
            ammo_magazine: 30,
            ammo_reserve: 120,
            last_input_tick: 10,
            last_shot_tick: None,
            last_command_tick: None,
            shot_index: 0,
        };
        let checkpoint = M3MatchCheckpointV2 {
            wire_version: M3_WIRE_VERSION,
            combat: MatchCheckpoint {
                source_epoch: 3,
                tick: 10,
                riders: vec![rider(player(1)), rider(player(2))],
                resolved_shots: Vec::new(),
            },
            gameplay: gameplay.checkpoint(),
            next_horse_damage_sequence: 4,
        };
        assert!(checkpoint.is_bounded_and_canonical());
        let encoded = serde_json::to_vec(&checkpoint).unwrap();
        let decoded: M3MatchCheckpointV2 = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, checkpoint);
        assert_eq!(decoded.hash(), checkpoint.hash());

        let mut wrong_epoch = checkpoint.clone();
        wrong_epoch.combat.source_epoch = 2;
        assert!(!wrong_epoch.is_bounded_and_canonical());
        let mut wrong_order = checkpoint.clone();
        wrong_order.combat.riders.swap(0, 1);
        assert!(!wrong_order.is_bounded_and_canonical());
        let mut wrong_tick = checkpoint.clone();
        wrong_tick.combat.tick = 11;
        assert!(!wrong_tick.is_bounded_and_canonical());
        let mut wrong_stance = checkpoint.clone();
        wrong_stance.combat.riders[0].stance = RiderStance::Unknown(0);
        assert!(!wrong_stance.is_bounded_and_canonical());
        let mut zero_sequence = checkpoint;
        zero_sequence.next_horse_damage_sequence = 0;
        assert!(!zero_sequence.is_bounded_and_canonical());
    }

    fn secure_fixture(local: PlayerId) -> (SecureSession, SigningKey, SocketAddr) {
        let server = SigningKey::from_bytes(&[9; 32]);
        let key1 = SigningKey::from_bytes(&[1; 32]);
        let key2 = SigningKey::from_bytes(&[2; 32]);
        let manifest = RosterManifest {
            lobby_id: lobby(),
            network_generation: 7,
            session_generation: 8,
            roster_revision: 9,
            entries: vec![
                spurfire_protocol::RosterManifestEntry {
                    player_id: player(1),
                    session_public_key: SessionPublicKey::from_bytes(
                        key1.verifying_key().to_bytes(),
                    ),
                    tailnet_address: "100.64.0.1".parse().unwrap(),
                    application_port: 41643,
                    node_key: Some(NodeKey::from_bytes([1; 32])),
                },
                spurfire_protocol::RosterManifestEntry {
                    player_id: player(2),
                    session_public_key: SessionPublicKey::from_bytes(
                        key2.verifying_key().to_bytes(),
                    ),
                    tailnet_address: "100.64.0.2".parse().unwrap(),
                    application_port: 41643,
                    node_key: Some(NodeKey::from_bytes([2; 32])),
                },
            ],
        };
        let public = SessionPublicKey::from_bytes(server.verifying_key().to_bytes());
        let signature = SessionSignature::from_bytes(
            server
                .sign(&canonical_manifest_digest(public, &manifest))
                .to_bytes(),
        );
        let mut state = SessionState::new(lobby(), local, player(1), 0);
        state.add_peer(player(1), 0);
        state.add_peer(player(2), 0);
        let signing = if local == player(1) { key1 } else { key2 };
        (
            SecureSession::new(manifest, public, signature, state).unwrap(),
            signing,
            format!("100.64.0.{}:41643", if local == player(1) { 1 } else { 2 })
                .parse()
                .unwrap(),
        )
    }

    #[test]
    fn secure_gate_rejects_source_tamper_forgery_and_replay_before_mutation() {
        let (mut sender, sender_key, source) = secure_fixture(player(2));
        let (mut receiver, _, _) = secure_fixture(player(1));
        let signed = sender
            .envelope(1, PeerPayload::Heartbeat, &sender_key)
            .unwrap();
        let mut unsigned = signed.clone();
        unsigned.session = None;
        assert_eq!(
            receiver.accept_with_source(&unsigned, source, None, 0),
            AcceptOutcome::UnsignedInSecureMode
        );

        assert_eq!(
            receiver.accept_with_source(&signed, "100.64.0.99:41643".parse().unwrap(), None, 1,),
            AcceptOutcome::EndpointMismatch
        );
        let mut wrong_generation = signed.clone();
        wrong_generation
            .session
            .as_mut()
            .unwrap()
            .session_generation += 1;
        assert_eq!(
            receiver.accept_with_source(&wrong_generation, source, None, 2),
            AcceptOutcome::WrongGeneration
        );
        let mut wrong_roster = signed.clone();
        wrong_roster.session.as_mut().unwrap().roster_hash = RosterHash::from_bytes([4; 32]);
        assert_eq!(
            receiver.accept_with_source(&wrong_roster, source, None, 3),
            AcceptOutcome::RosterMismatch
        );
        let mut tampered = signed.clone();
        tampered.simulation_tick += 1;
        assert_eq!(
            receiver.accept_with_source(&tampered, source, None, 4),
            AcceptOutcome::BadSignature
        );
        let mut forged_sender = signed.clone();
        forged_sender.sender = player(1);
        assert_eq!(
            receiver.accept_with_source(&forged_sender, source, None, 5),
            AcceptOutcome::EndpointMismatch
        );
        assert_eq!(
            receiver.accept_with_source(&signed, source, Some(NodeKey::from_bytes([3; 32])), 6),
            AcceptOutcome::NodeKeyMismatch
        );
        assert_eq!(
            receiver.accept_with_source(&signed, source, Some(NodeKey::from_bytes([2; 32])), 7),
            AcceptOutcome::Accepted
        );
        assert_eq!(
            receiver.accept_with_source(&signed, source, Some(NodeKey::from_bytes([2; 32])), 8),
            AcceptOutcome::DuplicateOrReplay
        );
    }

    #[test]
    fn signed_non_authority_snapshot_is_rejected_before_replay_mutation() {
        let (mut sender, sender_key, source) = secure_fixture(player(2));
        let (mut receiver, _, _) = secure_fixture(player(1));
        let mut signed = sender
            .envelope(1, PeerPayload::Heartbeat, &sender_key)
            .unwrap();
        signed.payload = PeerPayload::RiderSnapshot {
            rider_player_id: player(2),
            position_mm: [1, 2, 3],
            velocity_mmps: [4, 5, 6],
            yaw_millidegrees: 7_000,
            stance: RiderStance::Mounted,
        };
        sender.sign(&mut signed, &sender_key).unwrap();
        let before = receiver.state().peers[&player(2)].clone();
        assert_eq!(
            receiver.accept_with_source(&signed, source, None, 10),
            AcceptOutcome::InvalidPayloadRole
        );
        let after = &receiver.state().peers[&player(2)];
        assert_eq!(after.last_sequence, before.last_sequence);
        assert_eq!(after.last_seen_ms, before.last_seen_ms);
    }

    #[test]
    fn unknown_sender_never_auto_inserts() {
        let mut receiver = SessionState::new(lobby(), player(1), player(1), 0);
        let mut outsider = SessionState::new(lobby(), player(3), player(1), 0);
        let packet = outsider.envelope(1, PeerPayload::Heartbeat);
        assert_eq!(receiver.accept(&packet, 1), AcceptOutcome::UnknownSender);
        assert_eq!(receiver.accept(&packet, 2), AcceptOutcome::UnknownSender);
    }

    #[test]
    fn replay_wrong_lobby_and_stale_epoch_are_rejected() {
        let mut sender = SessionState::new(lobby(), player(2), player(1), 0);
        let mut receiver = SessionState::new(lobby(), player(1), player(1), 0);
        receiver.add_peer(player(2), 0);
        let hello = sender.envelope(1, PeerPayload::Heartbeat);
        assert_eq!(receiver.accept(&hello, 10), AcceptOutcome::Accepted);
        assert_eq!(
            receiver.accept(&hello, 11),
            AcceptOutcome::DuplicateOrReplay
        );
        let mut stale = sender.envelope(2, PeerPayload::Heartbeat);
        stale.authority_epoch = 0;
        assert_eq!(
            receiver.accept(&stale, 12),
            AcceptOutcome::StaleAuthorityEpoch
        );
    }

    #[test]
    fn authority_loss_migrates_and_old_epoch_is_rejected() {
        let mut session = SessionState::new(lobby(), player(2), player(1), 0);
        session.add_peer(player(3), 0);
        // Keep local and peer 3 alive while authority 1 expires.
        session.peers.get_mut(&player(2)).unwrap().last_seen_ms = 2_000;
        session.peers.get_mut(&player(3)).unwrap().last_seen_ms = 2_000;
        assert_eq!(session.expire_and_migrate(3_100), Some((player(2), 2)));
        let stale = Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(1),
            sequence: 9,
            authority_epoch: 1,
            simulation_tick: 10,
            payload: PeerPayload::Heartbeat,
            session: None,
        };
        assert_eq!(
            session.accept(&stale, 3_101),
            AcceptOutcome::StaleAuthorityEpoch
        );
    }

    #[test]
    fn malicious_authority_claims_never_mutate_state() {
        let claim = |sender: PlayerId, authority: PlayerId, epoch: u64, sequence: u64| Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender,
            sequence,
            authority_epoch: epoch,
            simulation_tick: 10,
            payload: PeerPayload::Authority { authority, epoch },
            session: None,
        };
        let baseline = |session: &SessionState| {
            (
                session.authority(),
                session.authority_epoch(),
                session.peers[&player(3)].last_sequence,
                session.peers[&player(3)].last_seen_ms,
            )
        };
        let mut session = SessionState::new(lobby(), player(2), player(1), 0);
        session.add_peer(player(3), 0);
        // The current authority heartbeats at 200 ms and stays live.
        let alive = Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(1),
            sequence: 1,
            authority_epoch: 1,
            simulation_tick: 9,
            payload: PeerPayload::Heartbeat,
            session: None,
        };
        assert_eq!(session.accept(&alive, 200), AcceptOutcome::Accepted);

        // A u64::MAX self-nominated epoch jump is rejected and fully inert.
        let max_jump = claim(player(3), player(3), u64::MAX, 1);
        let before = baseline(&session);
        assert_eq!(
            session.accept(&max_jump, 300),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(baseline(&session), before);

        // A third-party install is rejected even at a plausible epoch.
        let third_party = claim(player(3), player(1), 2, 2);
        assert_eq!(
            session.accept(&third_party, 301),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(baseline(&session), before);

        // A +2 epoch skip is rejected.
        let skip = claim(player(3), player(3), 3, 3);
        assert_eq!(
            session.accept(&skip, 302),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(baseline(&session), before);

        // An exact-step advance over a demonstrably live authority is rejected.
        let premature = claim(player(3), player(3), 2, 4);
        assert_eq!(
            session.accept(&premature, 303),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(baseline(&session), before);

        // Once the authority is genuinely silent, a higher signed rider still
        // cannot preempt the deterministic lowest connected successor.
        let usurp = claim(player(3), player(3), 2, 5);
        assert_eq!(
            session.accept(&usurp, 200 + HEARTBEAT_TIMEOUT_MS),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(session.authority(), player(1));
        assert_eq!(session.authority_epoch(), 1);
        assert_eq!(
            session.expire_and_migrate(200 + HEARTBEAT_TIMEOUT_MS),
            Some((player(2), 2))
        );
        assert_eq!(session.authority(), player(2));
        assert_eq!(session.authority_epoch(), 2);
    }

    #[test]
    fn same_epoch_tie_break_waits_for_installed_authority_timeout() {
        let mut session = SessionState::new(lobby(), player(2), player(3), 0);
        session.add_peer(player(1), 0);
        session.add_peer(player(3), 0);
        let claim = |sender: PlayerId, sequence: u64, migration: bool| Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender,
            sequence,
            authority_epoch: 1,
            simulation_tick: 10,
            payload: if migration {
                let checkpoint = MatchCheckpoint {
                    source_epoch: 0,
                    tick: 10,
                    riders: vec![RiderCheckpoint {
                        rider_player_id: player(2),
                        position_mm: [0; 3],
                        velocity_mmps: [0; 3],
                        yaw_millidegrees: 0,
                        stance: RiderStance::Mounted,
                        health: 100,
                        weapon_id: 0,
                        ammo_magazine: 30,
                        ammo_reserve: 120,
                        last_input_tick: 10,
                        last_shot_tick: None,
                        last_command_tick: None,
                        shot_index: 0,
                    }],
                    resolved_shots: vec![],
                };
                let state_hash = checkpoint.hash();
                PeerPayload::MigrationSnapshot {
                    authority: sender,
                    epoch: 1,
                    checkpoint,
                    state_hash,
                }
            } else {
                PeerPayload::Authority {
                    authority: sender,
                    epoch: 1,
                }
            },
            session: None,
        };
        assert_eq!(
            session.accept(&claim(player(3), 1, false), 10),
            AcceptOutcome::Accepted
        );
        let before = session.clone();
        assert_eq!(
            session.accept(&claim(player(1), 1, false), 11),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(session.authority(), before.authority());
        assert_eq!(
            session.peers[&player(1)].last_sequence,
            before.peers[&player(1)].last_sequence
        );
        // A valid, signed-shape migration claim cannot bypass the same gate.
        assert_eq!(
            session.accept(&claim(player(1), 2, true), 12),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(session.authority(), player(3));
        assert_eq!(
            session.accept(&claim(player(1), 3, false), 10 + HEARTBEAT_TIMEOUT_MS),
            AcceptOutcome::Accepted
        );
        assert_eq!(session.authority(), player(1));
        assert_eq!(session.authority_epoch(), 1);
    }

    #[test]
    fn secure_receive_rejects_signed_lower_id_same_epoch_claim_while_authority_is_fresh() {
        let (mut attacker, attacker_signing, attacker_source) = secure_fixture(player(1));
        let (mut receiver, _, _) = secure_fixture(player(2));
        receiver.state.authority = player(2);
        let signed = attacker
            .envelope(
                1,
                PeerPayload::Authority {
                    authority: player(1),
                    epoch: 1,
                },
                &attacker_signing,
            )
            .unwrap();
        let before = receiver.state().clone();
        assert_eq!(
            receiver.accept_with_source(&signed, attacker_source, None, 10),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(receiver.state().authority(), before.authority());
        assert_eq!(
            receiver.state().peers[&player(1)].last_sequence,
            before.peers[&player(1)].last_sequence
        );

        let checkpoint = MatchCheckpoint {
            source_epoch: 0,
            tick: 2,
            riders: vec![RiderCheckpoint {
                rider_player_id: player(1),
                position_mm: [0; 3],
                velocity_mmps: [0; 3],
                yaw_millidegrees: 0,
                stance: RiderStance::Mounted,
                health: 100,
                weapon_id: 0,
                ammo_magazine: 30,
                ammo_reserve: 120,
                last_input_tick: 2,
                last_shot_tick: None,
                last_command_tick: None,
                shot_index: 0,
            }],
            resolved_shots: vec![],
        };
        let migration = attacker
            .envelope(
                2,
                PeerPayload::MigrationSnapshot {
                    authority: player(1),
                    epoch: 1,
                    state_hash: checkpoint.hash(),
                    checkpoint,
                },
                &attacker_signing,
            )
            .unwrap();
        assert_eq!(
            receiver.accept_with_source(&migration, attacker_source, None, 11),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(receiver.state().authority(), player(2));
    }

    #[test]
    fn secure_receive_rejects_forged_authority_claim_without_mutation() {
        let (mut attacker_session, attacker_signing, attacker_source) = secure_fixture(player(2));
        let (mut receiver, _, _) = secure_fixture(player(1));
        let signed = attacker_session
            .envelope(
                1,
                PeerPayload::Authority {
                    authority: player(2),
                    epoch: u64::MAX,
                },
                &attacker_signing,
            )
            .unwrap();
        let before = receiver.state().clone();
        assert_eq!(
            receiver.accept_with_source(&signed, attacker_source, None, 10),
            AcceptOutcome::InvalidAuthorityClaim
        );
        let after = receiver.state();
        assert_eq!(after.authority(), before.authority());
        assert_eq!(after.authority_epoch(), before.authority_epoch());
    }

    fn envelope(payload: PeerPayload) -> Envelope {
        Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(2),
            sequence: 1,
            authority_epoch: 1,
            simulation_tick: 42,
            payload,
            session: None,
        }
    }

    #[test]
    fn w01_old_snapshot_defaults_mounted_and_new_field_is_legacy_ignorable() {
        let old = br#"{
            "wire_version":"1.0",
            "lobby_id":"00000000-0000-4000-8000-000000000001",
            "sender":"00000000-0000-4000-8000-000000000002",
            "sequence":1,
            "authority_epoch":1,
            "simulation_tick":42,
            "payload":{"type":"rider_snapshot","position_mm":[1,2,3],"velocity_mmps":[4,5,6],"yaw_millidegrees":7000}
        }"#;
        let decoded = decode(old).unwrap();
        assert!(matches!(
            decoded.payload,
            PeerPayload::RiderSnapshot {
                stance: RiderStance::Mounted,
                ..
            }
        ));

        let new = envelope(PeerPayload::RiderSnapshot {
            rider_player_id: player(2),
            position_mm: [1, 2, 3],
            velocity_mmps: [4, 5, 6],
            yaw_millidegrees: 7_000,
            stance: RiderStance::SaddleDiveAirborne,
        });
        let encoded = encode(&new).unwrap();
        let text = std::str::from_utf8(&encoded).unwrap();
        assert!(text.contains("\"stance\":3"));

        #[derive(Deserialize)]
        struct LegacyEnvelope {
            payload: LegacyPayload,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum LegacyPayload {
            RiderSnapshot {
                position_mm: [i32; 3],
                velocity_mmps: [i32; 3],
                yaw_millidegrees: i32,
            },
        }
        let legacy: LegacyEnvelope = serde_json::from_slice(&encoded).unwrap();
        let LegacyPayload::RiderSnapshot {
            position_mm,
            velocity_mmps,
            yaw_millidegrees,
        } = legacy.payload;
        assert_eq!(position_mm, [1, 2, 3]);
        assert_eq!(velocity_mmps, [4, 5, 6]);
        assert_eq!(yaw_millidegrees, 7_000);
    }

    #[test]
    fn w01_known_and_unknown_stances_round_trip_and_malformed_values_fail() {
        for stance in [
            RiderStance::Mounted,
            RiderStance::MountedAirborne,
            RiderStance::SaddleDiveAirborne,
            RiderStance::LandingProne,
            RiderStance::LandingRecovery,
            RiderStance::OnFootStanding,
            RiderStance::Unknown(0),
            RiderStance::Unknown(200),
            RiderStance::Unknown(255),
        ] {
            let original = envelope(PeerPayload::RiderSnapshot {
                rider_player_id: player(2),
                position_mm: [0; 3],
                velocity_mmps: [0; 3],
                yaw_millidegrees: 0,
                stance,
            });
            assert_eq!(decode(&encode(&original).unwrap()).unwrap(), original);
        }

        let base = serde_json::to_value(envelope(PeerPayload::RiderSnapshot {
            rider_player_id: player(2),
            position_mm: [0; 3],
            velocity_mmps: [0; 3],
            yaw_millidegrees: 0,
            stance: RiderStance::Mounted,
        }))
        .unwrap();
        for malformed in [
            serde_json::json!(-1),
            serde_json::json!(256),
            serde_json::json!(1.5),
            serde_json::json!("3"),
        ] {
            let mut value = base.clone();
            value["payload"]["stance"] = malformed;
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(matches!(decode(&bytes), Err(CodecError::Malformed(_))));
        }
    }

    #[test]
    fn w01_every_payload_vector_is_bounded_and_no_event_variant_is_added() {
        use spurfire_protocol::{
            HitZone, QuantizedDirection, QuantizedOrigin, ShotOutcome, SimulationTick, WeaponId,
        };

        let command = ShotCommand {
            tick: SimulationTick::new(42),
            shooter_peer_id: player(2),
            weapon_id: WeaponId::Dustwalker,
            origin: QuantizedOrigin::new(1, 2, 3),
            direction: QuantizedDirection::new(0, 0, -1_000_000),
            spread_seed: u64::MAX,
            claimed_target: None,
        };
        let result = ShotResult {
            tick: SimulationTick::new(42),
            shooter_peer_id: player(2),
            weapon_id: WeaponId::Dustwalker,
            outcome: ShotOutcome::Hit,
            rejection_reason: None,
            resolved_direction: Some(QuantizedDirection::new(0, 0, -1_000_000)),
            target_id: None,
            hit_zone: Some(HitZone::Head),
            damage: 28,
            distance_mm: Some(60_000),
            eliminated: false,
        };
        let payloads = vec![
            PeerPayload::Hello {
                hostname: "rider-a".to_owned(),
            },
            PeerPayload::Heartbeat,
            PeerPayload::Probe {
                nonce: u64::MAX,
                reply: true,
            },
            PeerPayload::RiderInput {
                throttle_milli: 1_000,
                steer_milli: -1_000,
                buttons: RIDER_INPUT_JUMP_PRESSED | RIDER_INPUT_INTERACT_PRESSED,
            },
            PeerPayload::RiderSnapshot {
                rider_player_id: player(2),
                position_mm: [i32::MAX, i32::MIN, 0],
                velocity_mmps: [i32::MIN, i32::MAX, 0],
                yaw_millidegrees: i32::MAX,
                stance: RiderStance::Unknown(255),
            },
            PeerPayload::ShotCommand { command },
            PeerPayload::ShotResult { result },
            PeerPayload::Authority {
                authority: player(1),
                epoch: u64::MAX,
            },
            PeerPayload::MigrationSnapshot {
                authority: player(1),
                epoch: 2,
                checkpoint: MatchCheckpoint {
                    source_epoch: 1,
                    tick: u64::MAX,
                    riders: vec![RiderCheckpoint {
                        rider_player_id: player(2),
                        position_mm: [i32::MAX, i32::MIN, 0],
                        velocity_mmps: [i32::MIN, i32::MAX, 0],
                        yaw_millidegrees: i32::MAX,
                        stance: RiderStance::Mounted,
                        health: 100,
                        weapon_id: 0,
                        ammo_magazine: 6,
                        ammo_reserve: 24,
                        last_input_tick: u64::MAX,
                        last_shot_tick: Some(u64::MAX),
                        last_command_tick: Some(u64::MAX),
                        shot_index: 1,
                    }],
                    resolved_shots: vec![],
                },
                state_hash: [0xf; 32],
            },
            PeerPayload::Leave,
        ];
        assert_eq!(
            payloads.len(),
            10,
            "top-level M2 events are not peer payloads"
        );
        let (secure, signing, _) = secure_fixture(player(2));
        for payload in payloads {
            let mut signed = envelope(payload);
            secure.sign(&mut signed, &signing).unwrap();
            let encoded = encode(&signed).unwrap();
            assert!(
                encoded.len() <= MAX_DATAGRAM_BYTES,
                "signed payload is {} bytes",
                encoded.len()
            );
            assert!(encoded.len() <= 1_200);
            assert_eq!(decode(&encoded).unwrap().wire_version, CURRENT_WIRE_VERSION);
        }
    }

    #[test]
    fn input_reserved_bits_and_major_versions_are_rejected_but_minor_versions_work() {
        let reserved = envelope(PeerPayload::RiderInput {
            throttle_milli: 0,
            steer_milli: 0,
            buttons: 1 << 2,
        });
        assert!(matches!(encode(&reserved), Err(CodecError::Malformed(_))));
        assert!(matches!(
            decode(&serde_json::to_vec(&reserved).unwrap()),
            Err(CodecError::Malformed(_))
        ));

        let mut major = envelope(PeerPayload::Heartbeat);
        major.wire_version = WireVersion::new(2, 0);
        assert_eq!(
            decode(&serde_json::to_vec(&major).unwrap()),
            Err(CodecError::IncompatibleVersion)
        );
        let mut unknown_v2_payload = serde_json::to_value(&major).unwrap();
        unknown_v2_payload["payload"] = serde_json::json!({"type": "actor_snapshot"});
        assert_eq!(
            decode(&serde_json::to_vec(&unknown_v2_payload).unwrap()),
            Err(CodecError::IncompatibleVersion)
        );
        let mut unknown_v1_payload =
            serde_json::to_value(envelope(PeerPayload::Heartbeat)).unwrap();
        unknown_v1_payload["payload"] = serde_json::json!({"type": "actor_snapshot"});
        assert!(matches!(
            decode(&serde_json::to_vec(&unknown_v1_payload).unwrap()),
            Err(CodecError::Malformed(_))
        ));
        let mut future_minor = envelope(PeerPayload::RiderInput {
            throttle_milli: 0,
            steer_milli: 0,
            buttons: 1 << 2,
        });
        future_minor.wire_version = WireVersion::new(1, 99);
        assert_eq!(
            decode(&serde_json::to_vec(&future_minor).unwrap()).unwrap(),
            future_minor
        );
        assert!(matches!(
            encode(&future_minor),
            Err(CodecError::Malformed(_))
        ));
    }

    #[test]
    fn shot_results_are_subject_bound_and_applied_once_across_sequences() {
        use spurfire_protocol::{ShotOutcome, SimulationTick, WeaponId};
        let result = ShotResult {
            tick: SimulationTick::new(42),
            shooter_peer_id: player(2),
            weapon_id: WeaponId::Dustwalker,
            outcome: ShotOutcome::Miss,
            rejection_reason: None,
            resolved_direction: None,
            target_id: None,
            hit_zone: None,
            damage: 0,
            distance_mm: None,
            eliminated: false,
        };
        let mut session = SessionState::new(lobby(), player(2), player(1), 0);
        session.add_peer(player(1), 0);
        let packet = Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(1),
            sequence: 1,
            authority_epoch: 1,
            simulation_tick: 42,
            payload: PeerPayload::ShotResult {
                result: result.clone(),
            },
            session: None,
        };
        assert_eq!(session.accept(&packet, 1), AcceptOutcome::Accepted);
        assert!(session.has_applied_shot_result(1, player(2), 42));
        let mut duplicate = packet.clone();
        duplicate.sequence = 2;
        assert_eq!(
            session.accept(&duplicate, 2),
            AcceptOutcome::DuplicateShotResult
        );
        let mut forged_subject = packet;
        forged_subject.sequence = 3;
        if let PeerPayload::ShotResult { result } = &mut forged_subject.payload {
            result.shooter_peer_id = player(3);
        }
        assert_eq!(
            session.accept(&forged_subject, 3),
            AcceptOutcome::InvalidPayloadSubject
        );
    }

    #[test]
    fn migration_checkpoint_installs_atomically_and_advances_exactly_one_epoch() {
        let checkpoint = MatchCheckpoint {
            source_epoch: 1,
            tick: 180,
            riders: vec![RiderCheckpoint {
                rider_player_id: player(2),
                position_mm: [2_000, 0, 3_000],
                velocity_mmps: [500, 0, 750],
                yaw_millidegrees: 45_000,
                stance: RiderStance::Mounted,
                health: 72,
                weapon_id: 0,
                ammo_magazine: 3,
                ammo_reserve: 18,
                last_input_tick: 179,
                last_shot_tick: Some(170),
                last_command_tick: Some(170),
                shot_index: 1,
            }],
            resolved_shots: vec![(player(2), 170)],
        };
        let packet = |hash| Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(2),
            sequence: 1,
            authority_epoch: 2,
            simulation_tick: 180,
            payload: PeerPayload::MigrationSnapshot {
                authority: player(2),
                epoch: 2,
                checkpoint: checkpoint.clone(),
                state_hash: hash,
            },
            session: None,
        };
        let mut session = SessionState::new(lobby(), player(3), player(1), 0);
        session.add_peer(player(2), 3_000);
        session.add_peer(player(3), 3_000);
        let before = session.clone();
        assert_eq!(
            session.accept(&packet([0; 32]), HEARTBEAT_TIMEOUT_MS),
            AcceptOutcome::InvalidCheckpoint
        );
        assert_eq!(session.authority(), before.authority());
        assert!(session.checkpoint().is_none());

        // Regression: gameplay restoration has a fixed 100-health ceiling. The
        // transport must reject an otherwise signed/hash-valid checkpoint
        // before authority, replay, receipts, or checkpoint state can move.
        let mut excessive_health = checkpoint.clone();
        excessive_health.riders[0].health = MAX_CHECKPOINT_RIDER_HEALTH + 1;
        assert_eq!(
            session.accept(
                &Envelope {
                    payload: PeerPayload::MigrationSnapshot {
                        authority: player(2),
                        epoch: 2,
                        state_hash: excessive_health.hash(),
                        checkpoint: excessive_health,
                    },
                    ..packet(checkpoint.hash())
                },
                HEARTBEAT_TIMEOUT_MS,
            ),
            AcceptOutcome::InvalidCheckpoint
        );
        assert_eq!(session.authority(), before.authority());
        assert_eq!(session.authority_epoch(), before.authority_epoch());
        assert!(session.checkpoint().is_none());
        assert!(!session.has_applied_shot_result(1, player(2), 170));

        // Reusing the same sequence proves the invalid checkpoint did not
        // advance replay state.
        assert_eq!(
            session.accept(&packet(checkpoint.hash()), HEARTBEAT_TIMEOUT_MS),
            AcceptOutcome::Accepted
        );
        assert_eq!(session.authority(), player(2));
        assert_eq!(session.authority_epoch(), 2);
        assert_eq!(session.checkpoint(), Some(&checkpoint));
        assert!(session.has_applied_shot_result(1, player(2), 170));
    }

    #[test]
    fn noncanonical_unknown_stance_aliases_are_rejected_before_transport() {
        for known_id in RiderStance::MOUNTED_ID..=RiderStance::ON_FOOT_STANDING_ID {
            let aliased = envelope(PeerPayload::RiderSnapshot {
                rider_player_id: player(2),
                position_mm: [0; 3],
                velocity_mmps: [0; 3],
                yaw_millidegrees: 0,
                stance: RiderStance::Unknown(known_id),
            });
            assert!(matches!(encode(&aliased), Err(CodecError::Malformed(_))));
        }
    }
}
