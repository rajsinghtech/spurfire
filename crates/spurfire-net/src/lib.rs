#![forbid(unsafe_code)]
//! Bounded, transport-independent peer-session protocol for Spurfire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use spurfire_protocol::{
    LobbyId, PlayerId, RiderStance, ShotCommand, ShotResult, WireVersion, CURRENT_WIRE_VERSION,
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
    if matches!(
        envelope.payload,
        PeerPayload::RiderInput { buttons, .. } if buttons & RIDER_INPUT_RESERVED_MASK != 0
    ) {
        return Err(CodecError::Malformed(
            "rider input contains reserved button bits".to_owned(),
        ));
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

    fn envelope(payload: PeerPayload) -> Envelope {
        Envelope {
            wire_version: CURRENT_WIRE_VERSION,
            lobby_id: lobby(),
            sender: player(2),
            sequence: 1,
            authority_epoch: 1,
            simulation_tick: 42,
            payload,
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
        for payload in payloads {
            let encoded = encode(&envelope(payload)).unwrap();
            assert!(
                encoded.len() <= MAX_DATAGRAM_BYTES,
                "{} bytes",
                encoded.len()
            );
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
        let mut future_minor = envelope(PeerPayload::Heartbeat);
        future_minor.wire_version = WireVersion::new(1, 99);
        assert_eq!(
            decode(&serde_json::to_vec(&future_minor).unwrap()).unwrap(),
            future_minor
        );
    }
}
