//! Engine-neutral lobby, roster, horse, and connectivity models.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{LobbyId, LobbyState, PlayerId, UnixMillis, WireVersion};

fn is_false(value: &bool) -> bool {
    !*value
}

/// Default lobby capacity when the create request omits `max_players`.
pub const DEFAULT_MAX_PLAYERS: u8 = 8;
/// Hard protocol cap for a lobby roster.
pub const MAX_PLAYERS: u8 = 16;
/// Prototype minimum roster size required to start.
pub const PROTOTYPE_MIN_PLAYERS: u8 = 2;
/// Idle TTL while a lobby is forming or ready.
pub const IDLE_TTL_MS: u64 = 10 * 60 * 1_000;
/// Absolute lobby TTL.
pub const ABSOLUTE_TTL_MS: u64 = 60 * 60 * 1_000;
/// Maximum lifetime of a real join credential.
pub const JOIN_CREDENTIAL_TTL_MS: u64 = 5 * 60 * 1_000;

/// Horse archetype selected by a player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorseSelection {
    /// Fast with lower durability.
    Courser,
    /// Durable with slower acceleration.
    Warhorse,
    /// Agile over rough ground with average speed.
    Mustang,
    /// A horse archetype introduced by a newer wire peer.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Live path used to reach one peer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRoute {
    /// End-to-end peer-to-peer path.
    Direct,
    /// Traffic forwarded through another peer.
    PeerRelay,
    /// Traffic forwarded through a Tailscale DERP relay.
    DerpRelay,
    /// Route unavailable or introduced by a newer wire version.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Counts of known routes from one candidate to the rest of the roster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSummary {
    /// Direct peer paths.
    pub direct_count: u32,
    /// Paths forwarded by another peer.
    pub peer_relay_count: u32,
    /// Paths forwarded by DERP.
    pub derp_count: u32,
}

impl RouteSummary {
    /// Returns the number of peers assigned a known route, checking overflow.
    #[must_use]
    pub const fn checked_known_peer_count(self) -> Option<u32> {
        match self.direct_count.checked_add(self.peer_relay_count) {
            Some(partial) => partial.checked_add(self.derp_count),
            None => None,
        }
    }
}

/// Integer-only connectivity and device sample used by authority election.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectivitySample {
    /// Route distribution observed by the reporting player.
    pub route_summary: RouteSummary,
    /// Median round-trip time in milliseconds.
    pub rtt_ms_median: u32,
    /// Worst round-trip time in milliseconds.
    pub rtt_ms_worst: u32,
    /// Jitter in milliseconds.
    pub jitter_ms: u32,
    /// Packet loss where 10,000 means 10.000 percent.
    pub loss_pct_milli: u32,
    /// Sustained upload throughput in whole megabits per second.
    pub upload_mbps_sustained: u32,
    /// Integer self-benchmark from 0 through 1,000.
    pub device_perf_score: u32,
    /// Number of peer observations represented by the report.
    pub observed_peer_count: u32,
    /// Server receipt time in Unix milliseconds.
    pub measured_at: UnixMillis,
}

impl ConnectivitySample {
    /// Validates protocol ranges and verifies that route counters faithfully
    /// describe no more peers than exist in the roster. Fewer observations are
    /// valid (those peers were unreachable); zero reachable peers is handled by
    /// the election eligibility filter rather than treated as malformed input.
    pub fn validate_for_roster(
        &self,
        roster_size: usize,
    ) -> Result<(), ConnectivityValidationError> {
        if self.loss_pct_milli > 10_000 {
            return Err(ConnectivityValidationError::LossOutOfRange {
                value: self.loss_pct_milli,
            });
        }
        if self.device_perf_score > 1_000 {
            return Err(ConnectivityValidationError::DevicePerformanceOutOfRange {
                value: self.device_perf_score,
            });
        }

        let expected = roster_size
            .checked_sub(1)
            .and_then(|count| u32::try_from(count).ok())
            .ok_or(ConnectivityValidationError::InvalidRosterSize { roster_size })?;
        let known = self
            .route_summary
            .checked_known_peer_count()
            .ok_or(ConnectivityValidationError::RouteCountOverflow)?;
        if known != self.observed_peer_count {
            return Err(ConnectivityValidationError::RouteCountMismatch {
                route_count: known,
                observed_peer_count: self.observed_peer_count,
            });
        }
        if self.observed_peer_count > expected {
            return Err(ConnectivityValidationError::PeerCountExceedsRoster {
                maximum_peer_count: expected,
                observed_peer_count: self.observed_peer_count,
            });
        }
        Ok(())
    }
}

/// Invalid or incomplete connectivity measurements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum ConnectivityValidationError {
    /// Loss reports use the inclusive 0..=10,000 protocol range.
    #[error("loss_pct_milli {value} exceeds 10000")]
    LossOutOfRange {
        /// Supplied value.
        value: u32,
    },
    /// Device self-benchmark uses the inclusive 0..=1,000 range.
    #[error("device_perf_score {value} exceeds 1000")]
    DevicePerformanceOutOfRange {
        /// Supplied value.
        value: u32,
    },
    /// Route counters could not be summed in `u32`.
    #[error("route counters overflow")]
    RouteCountOverflow,
    /// Route counters and `observed_peer_count` disagree.
    #[error(
        "route counters cover {route_count} peers but observed_peer_count is {observed_peer_count}"
    )]
    RouteCountMismatch {
        /// Sum of direct, peer relay, and DERP route counters.
        route_count: u32,
        /// Reported observation count.
        observed_peer_count: u32,
    },
    /// Report claims observations for more peers than the roster contains.
    #[error(
        "measurement covers {observed_peer_count} peers but roster allows at most {maximum_peer_count}"
    )]
    PeerCountExceedsRoster {
        /// Roster size minus the reporting player.
        maximum_peer_count: u32,
        /// Reported observation count.
        observed_peer_count: u32,
    },
    /// A zero-sized roster or one too large to encode cannot be validated.
    #[error("invalid roster size {roster_size}")]
    InvalidRosterSize {
        /// Supplied roster size.
        roster_size: usize,
    },
}

/// Progress of a player joining the data plane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerJoinState {
    /// Credential was issued but the peer has not connected yet.
    CredentialIssued,
    /// Peer is joining the lobby network.
    Connecting,
    /// Peer is connected to the lobby network.
    Connected,
    /// The player left and cleanup has been scheduled.
    Left,
    /// A device/key operation was denied and will be retried.
    CleanupPending,
    /// State introduced by a newer peer.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Public roster entry. It deliberately contains no key, OAuth, or device ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    /// Client-generated UUIDv4.
    pub player_id: PlayerId,
    /// Human-facing lobby name.
    pub display_name: String,
    /// Data-plane join progress.
    pub join_state: PlayerJoinState,
    /// Wire version advertised by this player.
    pub wire_version: WireVersion,
    /// Authority formula understood by this player.
    pub formula_version: String,
    /// Selected horse archetype, if chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horse_selection: Option<HorseSelection>,
    /// Latest public route summary.
    #[serde(default)]
    pub route_summary: RouteSummary,
    /// When this player entered the roster.
    pub joined_at: UnixMillis,
    /// Whether best-effort device/key cleanup needs a retry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cleanup_pending: bool,
}

/// Compact authority information included in a lobby snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritySummary {
    /// Current deterministic winner.
    pub candidate_player_id: PlayerId,
    /// Formula used to produce this result.
    pub formula_version: String,
    /// Winner's final election score.
    pub score_milli: u32,
}

/// Idle and absolute expiration deadlines for a lobby.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyTtl {
    /// Deadline refreshed by join, leave, or measurement activity.
    pub idle_expires_at: UnixMillis,
    /// Deadline fixed at lobby creation.
    pub absolute_expires_at: UnixMillis,
}

/// Requested backing mode for lobby network provisioning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningMode {
    /// Shared managed tailnet isolated by lobby tags and ACLs.
    #[default]
    SharedTailnet,
    /// Simulated lifecycle with no mutating Tailscale requests.
    DryRun,
    /// Explicitly unavailable because tested create endpoints return 404.
    TailnetPerLobby,
}

impl ProvisioningMode {
    /// Whether this mode is unconditionally unavailable under the verified API verdict.
    #[must_use]
    pub const fn is_known_unavailable(self) -> bool {
        matches!(self, Self::TailnetPerLobby)
    }
}

/// Public lobby model and pollable snapshot. It contains no secret material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lobby {
    /// Stable lobby UUIDv4.
    pub lobby_id: LobbyId,
    /// Human-facing lobby name.
    pub display_name: String,
    /// Current lifecycle state.
    pub state: LobbyState,
    /// Machine-readable reason, mandatory in `FAILED`.
    pub state_reason: Option<String>,
    /// Current roster.
    pub roster: Vec<Player>,
    /// Hard roster capacity (never greater than [`MAX_PLAYERS`]).
    pub max_players: u8,
    /// Seed fixed when entering `STARTING`, otherwise `None`.
    pub map_seed: Option<u64>,
    /// Current authority, or `None` until election succeeds.
    pub authority: Option<AuthoritySummary>,
    /// Idle and absolute deadlines.
    pub ttl: LobbyTtl,
    /// Wire version emitted by the service.
    pub wire_version: WireVersion,
    /// Selected provisioning behavior.
    pub provisioning_mode: ProvisioningMode,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// At least one upstream key or device cleanup needs a retry. No resource
    /// identifier is exposed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cleanup_pending: bool,
}

impl Lobby {
    /// Checks invariants that can be validated without service state.
    pub fn validate(&self) -> Result<(), LobbyValidationError> {
        if self.max_players == 0 || self.max_players > MAX_PLAYERS {
            return Err(LobbyValidationError::InvalidMaxPlayers {
                value: self.max_players,
            });
        }
        if self.roster.len() > usize::from(self.max_players) {
            return Err(LobbyValidationError::RosterExceedsCapacity {
                roster_size: self.roster.len(),
                max_players: self.max_players,
            });
        }
        if self.state.requires_state_reason()
            && self
                .state_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(LobbyValidationError::MissingStateReason);
        }
        Ok(())
    }
}

/// Invalid public lobby model.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum LobbyValidationError {
    /// Capacity is zero or exceeds the hard cap.
    #[error("max_players {value} must be between 1 and {MAX_PLAYERS}")]
    InvalidMaxPlayers {
        /// Supplied capacity.
        value: u8,
    },
    /// The roster has more entries than the advertised capacity.
    #[error("roster size {roster_size} exceeds max_players {max_players}")]
    RosterExceedsCapacity {
        /// Current number of roster entries.
        roster_size: usize,
        /// Advertised capacity.
        max_players: u8,
    },
    /// `FAILED` snapshots require a non-empty machine-readable reason.
    #[error("failed lobby requires state_reason")]
    MissingStateReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_additive_enum_values_are_forward_compatible() {
        assert_eq!(
            serde_json::from_str::<NetworkRoute>(r#""quantum_relay""#).unwrap(),
            NetworkRoute::Unknown
        );
        assert_eq!(
            serde_json::from_str::<HorseSelection>(r#""pegasus""#).unwrap(),
            HorseSelection::Unknown
        );
    }

    #[test]
    fn sample_requires_internally_complete_route_counts() {
        let sample = ConnectivitySample {
            route_summary: RouteSummary {
                direct_count: 1,
                peer_relay_count: 1,
                derp_count: 0,
            },
            rtt_ms_median: 20,
            rtt_ms_worst: 30,
            jitter_ms: 2,
            loss_pct_milli: 0,
            upload_mbps_sustained: 20,
            device_perf_score: 800,
            observed_peer_count: 2,
            measured_at: UnixMillis::new(1_000),
        };
        assert!(sample.validate_for_roster(3).is_ok());
        assert!(sample.validate_for_roster(4).is_ok());

        let mut incomplete = sample;
        incomplete.observed_peer_count = 1;
        assert!(matches!(
            incomplete.validate_for_roster(3),
            Err(ConnectivityValidationError::RouteCountMismatch { .. })
        ));
    }
}
