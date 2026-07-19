#![forbid(unsafe_code)]
//! Bounded, transport-independent peer-session protocol for Spurfire.

use std::{collections::BTreeMap, net::SocketAddr};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use spurfire_protocol::{
    canonical_envelope_digest, canonical_manifest_digest, LobbyId, NodeKey, PlayerId, RiderStance,
    RosterHash, RosterManifest, SessionBinding, SessionIdentityError, SessionPublicKey,
    SessionSignature, ShotCommand, ShotResult, WireVersion, CURRENT_WIRE_VERSION,
};
use thiserror::Error;

pub mod replication;
#[cfg(feature = "rustscale")]
pub mod rustscale;

pub const MAX_DATAGRAM_BYTES: usize = 1_200;
pub const HEARTBEAT_TIMEOUT_MS: u64 = 3_000;
pub const RECONNECT_GRACE_MS: u64 = 5_000;
/// Existing mounted-jump edge bit, now formally assigned.
pub const RIDER_INPUT_JUMP_PRESSED: u16 = 1 << 0;
/// M2 dismount/remount E edge bit.
pub const RIDER_INPUT_INTERACT_PRESSED: u16 = 1 << 1;
/// Every other input bit is reserved and must remain zero in wire 1.1.
pub const RIDER_INPUT_RESERVED_MASK: u16 =
    !(RIDER_INPUT_JUMP_PRESSED | RIDER_INPUT_INTERACT_PRESSED);

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
        authority: PlayerId,
        epoch: u64,
        tick: u64,
        state_hash: String,
    },
    Leave,
}

/// Backward-compatible missing-field default for pre-M2 rider snapshots.
#[must_use]
pub const fn legacy_mounted_stance() -> RiderStance {
    RiderStance::Mounted
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
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|error| CodecError::Malformed(error.to_string()))?;
    if !CURRENT_WIRE_VERSION.is_compatible_with(envelope.wire_version) {
        return Err(CodecError::IncompatibleVersion);
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
        // A rider may submit only its own command identity.
        if matches!(
            envelope.payload,
            PeerPayload::ShotCommand { ref command }
                if command.shooter_peer_id != envelope.sender
        ) {
            return AcceptOutcome::InvalidPayloadRole;
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
            return authority <= self.authority;
        }
        if epoch != self.authority_epoch.saturating_add(1) {
            return false;
        }
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
        self.authority = successor;
        self.authority_epoch = self.authority_epoch.saturating_add(1);
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
            position_mm,
            velocity_mmps,
            yaw_millidegrees,
            stance,
        } => {
            out.push(4);
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
            tick,
            state_hash,
        } => {
            out.push(8);
            out.extend_from_slice(authority.as_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
            out.extend_from_slice(&tick.to_be_bytes());
            string(&mut out, state_hash);
        }
        PeerPayload::Leave => out.push(9),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let signed = sender
            .envelope(
                1,
                PeerPayload::RiderSnapshot {
                    position_mm: [1, 2, 3],
                    velocity_mmps: [4, 5, 6],
                    yaw_millidegrees: 7_000,
                    stance: RiderStance::Mounted,
                },
                &sender_key,
            )
            .unwrap();
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

        // Once the authority is genuinely silent, the exact-step self-
        // nomination the deterministic election would produce is accepted.
        let elected = claim(player(3), player(3), 2, 5);
        assert_eq!(
            session.accept(&elected, 200 + HEARTBEAT_TIMEOUT_MS),
            AcceptOutcome::Accepted
        );
        assert_eq!(session.authority(), player(3));
        assert_eq!(session.authority_epoch(), 2);
        // The installed claim cannot be replayed and a further self-nominated
        // advance requires the new authority to go silent first.
        let replay = claim(player(3), player(3), 2, 6);
        assert_eq!(
            session.accept(&replay, 200 + HEARTBEAT_TIMEOUT_MS + 1),
            AcceptOutcome::Accepted
        );
        let ratchet = claim(player(3), player(3), 3, 7);
        assert_eq!(
            session.accept(&ratchet, 200 + HEARTBEAT_TIMEOUT_MS + 2),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(session.authority_epoch(), 2);
    }

    #[test]
    fn same_epoch_tie_break_converges_to_lowest_sender() {
        let mut session = SessionState::new(lobby(), player(2), player(3), 0);
        session.add_peer(player(1), 0);
        session.add_peer(player(3), 0);
        let claim = |sender: PlayerId, sequence: u64| Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender,
            sequence,
            authority_epoch: 1,
            simulation_tick: 10,
            payload: PeerPayload::Authority {
                authority: sender,
                epoch: 1,
            },
            session: None,
        };
        // A higher-ID same-epoch claim does not displace the current authority.
        assert_eq!(
            session.accept(&claim(player(3), 1), 10),
            AcceptOutcome::Accepted
        );
        assert_eq!(session.authority(), player(3));
        // The lowest-ID self-nomination wins the same-epoch tie-break.
        assert_eq!(
            session.accept(&claim(player(1), 1), 11),
            AcceptOutcome::Accepted
        );
        assert_eq!(session.authority(), player(1));
        assert_eq!(session.authority_epoch(), 1);
        // Once converged, the higher-ID claim is incoherent and inert.
        assert_eq!(
            session.accept(&claim(player(3), 2), 12),
            AcceptOutcome::InvalidAuthorityClaim
        );
        assert_eq!(session.authority(), player(1));
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
                position_mm: [0; 3],
                velocity_mmps: [0; 3],
                yaw_millidegrees: 0,
                stance,
            });
            assert_eq!(decode(&encode(&original).unwrap()).unwrap(), original);
        }

        let base = serde_json::to_value(envelope(PeerPayload::RiderSnapshot {
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
                epoch: u64::MAX,
                tick: u64::MAX,
                state_hash: "f".repeat(64),
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
    fn noncanonical_unknown_stance_aliases_are_rejected_before_transport() {
        for known_id in RiderStance::MOUNTED_ID..=RiderStance::ON_FOOT_STANDING_ID {
            let aliased = envelope(PeerPayload::RiderSnapshot {
                position_mm: [0; 3],
                velocity_mmps: [0; 3],
                yaw_millidegrees: 0,
                stance: RiderStance::Unknown(known_id),
            });
            assert!(matches!(encode(&aliased), Err(CodecError::Malformed(_))));
        }
    }
}
