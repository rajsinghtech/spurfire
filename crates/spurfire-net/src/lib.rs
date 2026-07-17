#![forbid(unsafe_code)]
//! Bounded, transport-independent peer-session protocol for Spurfire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use spurfire_protocol::{
    LobbyId, PlayerId, ShotCommand, ShotResult, WireVersion, CURRENT_WIRE_VERSION,
};
use thiserror::Error;

pub mod replication;
#[cfg(feature = "rustscale")]
pub mod rustscale;

pub const MAX_DATAGRAM_BYTES: usize = 1_200;
pub const HEARTBEAT_TIMEOUT_MS: u64 = 3_000;
pub const RECONNECT_GRACE_MS: u64 = 5_000;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub wire_version: WireVersion,
    pub lobby_id: LobbyId,
    pub sender: PlayerId,
    pub sequence: u64,
    pub authority_epoch: u64,
    pub simulation_tick: u64,
    pub payload: PeerPayload,
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
    Ok(envelope)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptOutcome {
    Accepted,
    DuplicateOrReplay,
    WrongLobby,
    StaleAuthorityEpoch,
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
        }
    }

    pub fn accept(&mut self, envelope: &Envelope, now_ms: u64) -> AcceptOutcome {
        if envelope.lobby_id != self.lobby_id {
            return AcceptOutcome::WrongLobby;
        }
        if envelope.authority_epoch < self.authority_epoch {
            return AcceptOutcome::StaleAuthorityEpoch;
        }
        let peer = self.peers.entry(envelope.sender).or_insert(PeerState {
            last_sequence: 0,
            last_seen_ms: now_ms,
            connected: true,
        });
        if envelope.sequence <= peer.last_sequence {
            return AcceptOutcome::DuplicateOrReplay;
        }
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
        };
        assert_eq!(
            session.accept(&stale, 3_101),
            AcceptOutcome::StaleAuthorityEpoch
        );
    }
}
