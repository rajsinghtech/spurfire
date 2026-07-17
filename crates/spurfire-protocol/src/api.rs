//! Versioned HTTP API request and response DTOs.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{
    AuthorityElection, AuthorityScore, AuthoritySummary, ConnectivitySample, HorseSelection,
    InputHash, JoinCredential, Lobby, LobbyId, LobbyState, Player, PlayerId, ProvisioningMode,
    RouteSummary, UnixMillis, WireVersion, WireVersionMismatch, AUTHORITY_FORMULA_VERSION,
    DEFAULT_MAX_PLAYERS, MAX_PLAYERS, WIRE_VERSION,
};

/// Maximum Unicode scalar count accepted for a lobby or player display name.
pub const MAX_DISPLAY_NAME_CHARS: usize = 64;

const fn default_max_players() -> u8 {
    DEFAULT_MAX_PLAYERS
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_authority_formula_version() -> String {
    AUTHORITY_FORMULA_VERSION.to_owned()
}

/// Non-secret description of a mutating call suppressed or planned by dry-run mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAction {
    /// HTTP method that would be used against Tailscale.
    pub method: String,
    /// Redacted endpoint path (never includes credentials or device IDs).
    pub path: String,
    /// Human-readable, non-secret summary.
    pub description: String,
}

/// Metadata flattened into API responses when simulation is active.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseMetadata {
    /// True when no mutating Tailscale call was made.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Calls that would have occurred in a real request. This is always
    /// serialized so a dry-run response with no mutations still carries `[]`.
    #[serde(default)]
    pub planned_actions: Vec<PlannedAction>,
}

/// `POST /v1/lobbies` request body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLobbyRequest {
    /// Human-facing lobby name.
    pub display_name: String,
    /// Roster capacity; defaults to 8 and may not exceed 16.
    #[serde(default = "default_max_players")]
    pub max_players: u8,
    /// Shared-tailnet or dry-run behavior. Tailnet-per-lobby is parsed so the
    /// service can return the explicit `mode_unavailable` response.
    pub provisioning_mode: ProvisioningMode,
    /// Optional placement hint; it does not affect election behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_hint: Option<String>,
}

impl CreateLobbyRequest {
    /// Validates protocol-level create constraints.
    pub fn validate(&self) -> Result<(), ApiValidationError> {
        validate_display_name(&self.display_name)?;
        if self.max_players == 0 || self.max_players > MAX_PLAYERS {
            return Err(ApiValidationError::InvalidMaxPlayers {
                value: self.max_players,
            });
        }
        if self.provisioning_mode.is_known_unavailable() {
            return Err(ApiValidationError::ModeUnavailable);
        }
        Ok(())
    }
}

/// `201/200` response from `POST /v1/lobbies`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLobbyResponse {
    /// Stable created or replayed lobby ID.
    pub lobby_id: LobbyId,
    /// Initially `PROVISIONING`.
    pub state: LobbyState,
    /// Service wire version.
    pub wire_version: WireVersion,
    /// Absolute creation timestamp.
    pub created_at: UnixMillis,
    /// Fixed 60-minute absolute expiry.
    pub expires_at: UnixMillis,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Pollable `GET /v1/lobbies/{lobby_id}` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyResponse {
    /// Public snapshot with no secret material.
    #[serde(flatten)]
    pub lobby: Lobby,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Conventional route-specific alias for [`LobbyResponse`].
pub type GetLobbyResponse = LobbyResponse;

/// `POST /v1/lobbies/{lobby_id}/join` request body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinLobbyRequest {
    /// Client-generated UUIDv4.
    pub player_id: PlayerId,
    /// Human-facing roster name.
    pub display_name: String,
    /// Client protocol version.
    pub client_wire_version: WireVersion,
    /// Authority formula implemented by the client. Older clients default to
    /// the current formula, while mixed-formula rosters are rejected at start.
    #[serde(default = "default_authority_formula_version")]
    pub authority_formula_version: String,
    /// Optional horse choice for lobby display and match setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horse_selection: Option<HorseSelection>,
}

impl JoinLobbyRequest {
    /// Validates display name and major wire compatibility.
    pub fn validate(&self, service_version: WireVersion) -> Result<(), ApiValidationError> {
        validate_display_name(&self.display_name)?;
        service_version
            .check_compatible_with(self.client_wire_version)
            .map_err(ApiValidationError::WireVersionIncompatible)
    }
}

/// First successful `201` join response.
///
/// Its custom serializer is the sole protocol serializer allowed to expose an
/// auth key. Its derived-style `Debug` path delegates to the credential's
/// redacted implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinLobbyResponse {
    /// One-use secret returned exactly once.
    pub join_credential: JoinCredential,
    /// Public post-join lobby snapshot.
    pub lobby: Lobby,
    /// Dry-run metadata.
    pub metadata: ResponseMetadata,
}

impl Serialize for JoinLobbyResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Wire<'a> {
            join_credential: crate::credential::JoinCredentialWire<'a>,
            lobby: &'a Lobby,
            #[serde(flatten)]
            metadata: &'a ResponseMetadata,
        }

        Wire {
            join_credential: self.join_credential.as_wire(),
            lobby: &self.lobby,
            metadata: &self.metadata,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JoinLobbyResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            join_credential: JoinCredential,
            lobby: Lobby,
            #[serde(flatten)]
            metadata: ResponseMetadata,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            join_credential: wire.join_credential,
            lobby: wire.lobby,
            metadata: wire.metadata,
        })
    }
}

/// Non-secret credential receipt used for idempotent replay after key delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCredentialReceipt {
    /// Same credential ID as the first response.
    pub credential_id: String,
    /// Original absolute expiry.
    pub expires_at: UnixMillis,
    /// Always true.
    pub one_use: bool,
}

impl From<&JoinCredential> for JoinCredentialReceipt {
    fn from(credential: &JoinCredential) -> Self {
        Self {
            credential_id: credential.credential_id.clone(),
            expires_at: credential.expires_at,
            one_use: true,
        }
    }
}

/// Idempotent join replay that proves identity without re-emitting key material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinLobbyReplayResponse {
    /// Non-secret receipt for the already-issued credential.
    pub join_credential: JoinCredentialReceipt,
    /// Current public snapshot.
    pub lobby: Lobby,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// `POST /v1/lobbies/{lobby_id}/leave` request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveLobbyRequest {
    /// Player to remove.
    pub player_id: PlayerId,
}

/// Idempotent leave response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveLobbyResponse {
    /// True even when the player had already left.
    pub left: bool,
    /// True when capability-dependent cleanup was queued for retry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cleanup_pending: bool,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Connectivity fields sent by a player, before the server stamps receipt time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectivityReport {
    /// Route distribution to peers.
    pub route_summary: RouteSummary,
    /// Median RTT in milliseconds.
    pub rtt_ms_median: u32,
    /// Worst RTT in milliseconds.
    pub rtt_ms_worst: u32,
    /// Jitter in milliseconds.
    pub jitter_ms: u32,
    /// Loss where 10,000 means 10.000 percent.
    pub loss_pct_milli: u32,
    /// Sustained whole Mbps upload.
    pub upload_mbps_sustained: u32,
    /// Device benchmark from 0 through 1,000.
    pub device_perf_score: u32,
    /// Number of route observations.
    pub observed_peer_count: u32,
}

impl ConnectivityReport {
    /// Adds a trusted service receipt timestamp.
    #[must_use]
    pub fn into_sample(self, measured_at: UnixMillis) -> ConnectivitySample {
        ConnectivitySample {
            route_summary: self.route_summary,
            rtt_ms_median: self.rtt_ms_median,
            rtt_ms_worst: self.rtt_ms_worst,
            jitter_ms: self.jitter_ms,
            loss_pct_milli: self.loss_pct_milli,
            upload_mbps_sustained: self.upload_mbps_sustained,
            device_perf_score: self.device_perf_score,
            observed_peer_count: self.observed_peer_count,
            measured_at,
        }
    }
}

impl From<&ConnectivitySample> for ConnectivityReport {
    fn from(sample: &ConnectivitySample) -> Self {
        Self {
            route_summary: sample.route_summary,
            rtt_ms_median: sample.rtt_ms_median,
            rtt_ms_worst: sample.rtt_ms_worst,
            jitter_ms: sample.jitter_ms,
            loss_pct_milli: sample.loss_pct_milli,
            upload_mbps_sustained: sample.upload_mbps_sustained,
            device_perf_score: sample.device_perf_score,
            observed_peer_count: sample.observed_peer_count,
        }
    }
}

/// `POST /v1/lobbies/{lobby_id}/measurements` request body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitMeasurementsRequest {
    /// Reporting roster member.
    pub player_id: PlayerId,
    /// Integer election inputs, flattened to match the HTTP contract.
    #[serde(flatten)]
    pub report: ConnectivityReport,
}

impl SubmitMeasurementsRequest {
    /// Creates and validates a server-timestamped sample for the current roster.
    pub fn into_validated_sample(
        self,
        measured_at: UnixMillis,
        roster_size: usize,
    ) -> Result<ConnectivitySample, ApiValidationError> {
        let sample = self.report.into_sample(measured_at);
        sample
            .validate_for_roster(roster_size)
            .map_err(ApiValidationError::InvalidConnectivitySample)?;
        Ok(sample)
    }
}

/// Measurement acceptance response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitMeasurementsResponse {
    /// True when this row became the last-write-wins sample.
    pub accepted: bool,
    /// State after readiness/election evaluation.
    pub state: LobbyState,
    /// Current winner if one can be computed.
    pub authority: Option<AuthoritySummary>,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// `GET /v1/lobbies/{lobby_id}/authority` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityResponse {
    /// Formula used for every score.
    pub formula_version: String,
    /// Candidate set after filtering (or degraded fallback), in player-ID order.
    pub eligible: Vec<AuthorityScore>,
    /// Deterministic winner.
    pub winner_player_id: PlayerId,
    /// SHA-256 canonical election-input fingerprint.
    pub input_hash: InputHash,
    /// Exact public input needed for peer recomputation, including the
    /// eligibility evaluation time and server-stamped measurement times.
    pub input: crate::AuthorityElectionInput,
    /// True when normal eligibility was empty and raw scores decided.
    #[serde(default, skip_serializing_if = "is_false")]
    pub degraded: bool,
}

impl From<&AuthorityElection> for AuthorityResponse {
    fn from(election: &AuthorityElection) -> Self {
        Self {
            formula_version: election.formula_version.clone(),
            eligible: election.eligible.clone(),
            winner_player_id: election.winner_player_id,
            input_hash: election.input_hash,
            input: election.input.clone(),
            degraded: election.degraded,
        }
    }
}

/// `POST /v1/lobbies/{lobby_id}/start` request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartLobbyRequest {
    /// Creator identity; idempotency never bypasses this check.
    pub creator_player_id: PlayerId,
    /// Optional caller-selected deterministic map seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_seed: Option<u64>,
}

/// Successful or idempotently replayed start response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartLobbyResponse {
    /// Lobby being started.
    pub lobby_id: LobbyId,
    /// Normally `STARTING`, or the current later state on replay.
    pub state: LobbyState,
    /// Fixed map seed.
    pub map_seed: u64,
    /// Elected authority.
    pub authority: AuthoritySummary,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// One player's final prototype scoreboard row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalScore {
    /// Roster identity.
    pub player_id: PlayerId,
    /// Final bounty score. Service-side plausibility checks still apply.
    pub score: i64,
    /// Eliminations recorded for auditing.
    #[serde(default)]
    pub eliminations: u32,
    /// Assists recorded for auditing.
    #[serde(default)]
    pub assists: u32,
    /// Deaths recorded for auditing.
    #[serde(default)]
    pub deaths: u32,
}

/// `POST /v1/lobbies/{lobby_id}/heartbeat` request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityHeartbeatRequest {
    /// Must be the currently elected authority.
    pub player_id: PlayerId,
    /// Election view under which the authority is operating.
    pub input_hash: InputHash,
}

/// Authority heartbeat acceptance response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityHeartbeatResponse {
    /// True when the heartbeat was accepted.
    pub accepted: bool,
    /// `IN_MATCH` after the first accepted heartbeat.
    pub state: LobbyState,
    /// Current authority summary.
    pub authority: AuthoritySummary,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// `POST /v1/lobbies/{lobby_id}/results` request body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResultsRequest {
    /// Must equal the last known authority winner.
    pub submitted_by: PlayerId,
    /// Optional shallow prototype co-signers (not trusted for ranked play).
    #[serde(default)]
    pub co_signers: Vec<PlayerId>,
    /// Final scoreboard rows.
    pub final_scores: Vec<FinalScore>,
    /// Match duration in whole seconds.
    pub match_duration_s: u32,
    /// Matrix view under which the authority operated.
    pub input_hash: InputHash,
}

/// Accepted (`202`) results response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResultsResponse {
    /// True when shallow prototype verification passed.
    pub accepted: bool,
    /// `CLOSING` after acceptance.
    pub state: LobbyState,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Idempotent `DELETE /v1/lobbies/{lobby_id}` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestroyLobbyResponse {
    /// `CLOSING` while asynchronous teardown runs or `DESTROYED` afterward.
    pub state: LobbyState,
    /// Whether at least one cleanup operation remains queued.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cleanup_pending: bool,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Availability label returned by the capability preflight endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityModeStatus {
    /// Required calls are currently permitted.
    Available,
    /// Standard routes exist but tested OAuth scopes are insufficient.
    BlockedScopes,
    /// Tested alpha create routes returned 404.
    UnavailableApi404,
    /// Status introduced by a newer service.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Provisioning-mode capability summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityModes {
    /// Shared-tailnet-plus-tags readiness.
    pub shared_tailnet: CapabilityModeStatus,
    /// Tailnet-per-lobby alpha API readiness.
    pub tailnet_per_lobby: CapabilityModeStatus,
}

/// `GET /v1/capabilities` response. It contains booleans and verdicts only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    /// OAuth token/settings probe succeeded.
    pub oauth_token_ok: bool,
    /// Auth-key mint probe is permitted.
    pub can_mint_auth_keys: bool,
    /// Device-list probe is permitted.
    pub can_list_devices: bool,
    /// ACL read/write capability is permitted.
    pub can_manage_acl: bool,
    /// Honest provisioning verdicts.
    pub modes: CapabilityModes,
    /// Dry-run metadata.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Stable non-secret API error body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    /// Machine-readable code such as `wire_version_incompatible`.
    pub code: String,
    /// Safe human-readable summary. Secret values must never be interpolated.
    pub message: String,
    /// Optional machine-readable lobby state reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    /// Dry-run metadata and remediation plan.
    #[serde(flatten)]
    pub metadata: ResponseMetadata,
}

/// Protocol-level request validation failure.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ApiValidationError {
    /// Display name was empty after trimming.
    #[error("display_name must not be empty")]
    EmptyDisplayName,
    /// Display name exceeded the protocol bound.
    #[error("display_name exceeds {MAX_DISPLAY_NAME_CHARS} characters")]
    DisplayNameTooLong,
    /// Lobby capacity violated the 1..=16 bound.
    #[error("max_players {value} must be between 1 and {MAX_PLAYERS}")]
    InvalidMaxPlayers {
        /// Supplied capacity.
        value: u8,
    },
    /// Tailnet-per-lobby is explicitly unavailable under the verified 404 verdict.
    #[error("tailnet_per_lobby is unavailable: tested create API routes returned 404")]
    ModeUnavailable,
    /// Client and service major versions differ.
    #[error(transparent)]
    WireVersionIncompatible(WireVersionMismatch),
    /// Measurement ranges or route matrix are invalid.
    #[error(transparent)]
    InvalidConnectivitySample(crate::ConnectivityValidationError),
    /// At least one roster member has a major mismatch.
    #[error("player {player_id} has incompatible wire version {version}")]
    RosterWireVersionIncompatible {
        /// Incompatible player.
        player_id: PlayerId,
        /// Advertised version.
        version: WireVersion,
    },
    /// At least one roster member uses a different election formula.
    #[error("player {player_id} uses incompatible authority formula {formula_version}")]
    MixedAuthorityFormula {
        /// Incompatible player.
        player_id: PlayerId,
        /// Advertised formula.
        formula_version: String,
    },
}

/// Validates the wire and formula start guards for every roster member.
pub fn validate_start_roster(roster: &[Player]) -> Result<(), ApiValidationError> {
    validate_start_roster_against(roster, WIRE_VERSION, AUTHORITY_FORMULA_VERSION)
}

/// Variant of [`validate_start_roster`] for rolling upgrades and test vectors.
pub fn validate_start_roster_against(
    roster: &[Player],
    wire_version: WireVersion,
    formula_version: &str,
) -> Result<(), ApiValidationError> {
    for player in roster {
        if !wire_version.is_compatible_with(player.wire_version) {
            return Err(ApiValidationError::RosterWireVersionIncompatible {
                player_id: player.player_id,
                version: player.wire_version,
            });
        }
        if player.formula_version != formula_version {
            return Err(ApiValidationError::MixedAuthorityFormula {
                player_id: player.player_id,
                formula_version: player.formula_version.clone(),
            });
        }
    }
    Ok(())
}

fn validate_display_name(display_name: &str) -> Result<(), ApiValidationError> {
    if display_name.trim().is_empty() {
        return Err(ApiValidationError::EmptyDisplayName);
    }
    if display_name.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(ApiValidationError::DisplayNameTooLong);
    }
    Ok(())
}

impl fmt::Display for CapabilityModeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Available => "available",
            Self::BlockedScopes => "blocked_scopes",
            Self::UnavailableApi404 => "unavailable_api_404",
            Self::Unknown => "unknown",
        };
        formatter.write_str(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthoritySummary, LobbyTtl, PlayerJoinState, DRY_RUN_AUTH_KEY};

    fn lobby_id() -> LobbyId {
        LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap()
    }

    fn player_id() -> PlayerId {
        PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap()
    }

    fn lobby() -> Lobby {
        Lobby {
            lobby_id: lobby_id(),
            display_name: "High Noon".into(),
            state: LobbyState::Forming,
            state_reason: None,
            roster: Vec::new(),
            max_players: 8,
            map_seed: None,
            authority: None,
            ttl: LobbyTtl {
                idle_expires_at: UnixMillis::new(1_000),
                absolute_expires_at: UnixMillis::new(2_000),
            },
            wire_version: WIRE_VERSION,
            provisioning_mode: ProvisioningMode::SharedTailnet,
            created_at: UnixMillis::new(0),
            cleanup_pending: false,
        }
    }

    #[test]
    fn create_defaults_capacity_and_ignores_unknown_fields() {
        let request: CreateLobbyRequest = serde_json::from_str(
            r#"{"display_name":"High Noon","provisioning_mode":"dry_run","future":true}"#,
        )
        .unwrap();
        assert_eq!(request.max_players, DEFAULT_MAX_PLAYERS);
        assert!(request.validate().is_ok());
    }

    #[test]
    fn join_accepts_minor_difference_and_rejects_major_difference() {
        let mut request = JoinLobbyRequest {
            player_id: player_id(),
            display_name: "Rider".into(),
            client_wire_version: WireVersion::new(WIRE_VERSION.major(), 99),
            authority_formula_version: AUTHORITY_FORMULA_VERSION.into(),
            horse_selection: None,
        };
        assert!(request.validate(WIRE_VERSION).is_ok());

        request.client_wire_version = WireVersion::new(WIRE_VERSION.major() + 1, 0);
        assert!(matches!(
            request.validate(WIRE_VERSION),
            Err(ApiValidationError::WireVersionIncompatible(_))
        ));
    }

    #[test]
    fn measurement_dto_ignores_additive_json_fields() {
        let json = format!(
            r#"{{"player_id":"{}","route_summary":{{"direct_count":1,"peer_relay_count":0,"derp_count":0,"new_route":9}},"rtt_ms_median":20,"rtt_ms_worst":30,"jitter_ms":2,"loss_pct_milli":0,"upload_mbps_sustained":20,"device_perf_score":900,"observed_peer_count":1,"future_metric":42}}"#,
            player_id()
        );
        let request: SubmitMeasurementsRequest = serde_json::from_str(&json).unwrap();
        assert!(request
            .into_validated_sample(UnixMillis::new(100), 2)
            .is_ok());
    }

    #[test]
    fn auth_key_is_serialized_only_by_explicit_first_join_response() {
        let secret = "tskey-auth-canary-secret-value";
        let response = JoinLobbyResponse {
            join_credential: JoinCredential::new(
                "credential-1",
                secret,
                "example.ts.net",
                vec!["tag:spurfire-lobby-example".into()],
                UnixMillis::new(300_000),
            ),
            lobby: lobby(),
            metadata: ResponseMetadata::default(),
        };
        let debug = format!("{response:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("<redacted>"));

        let wire = serde_json::to_string(&response).unwrap();
        assert!(wire.contains(secret));
        let decoded: JoinLobbyResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.join_credential.expose_auth_key(), secret);

        let receipt = JoinCredentialReceipt::from(&response.join_credential);
        let replay = serde_json::to_string(&JoinLobbyReplayResponse {
            join_credential: receipt,
            lobby: lobby(),
            metadata: ResponseMetadata::default(),
        })
        .unwrap();
        assert!(!replay.contains(secret));
    }

    #[test]
    fn dry_run_placeholder_is_structurally_valid_and_non_secret() {
        let credential = JoinCredential::new(
            "dry-credential",
            DRY_RUN_AUTH_KEY,
            "dry-run.invalid",
            vec!["tag:spurfire-lobby-dry-run".into()],
            UnixMillis::new(1),
        );
        assert_eq!(credential.expose_auth_key(), "DRY_RUN_NO_KEY");
        assert!(credential.is_one_use());
    }

    #[test]
    fn mixed_formula_roster_fails_start_guard() {
        let player = Player {
            player_id: player_id(),
            display_name: "Rider".into(),
            join_state: PlayerJoinState::Connected,
            wire_version: WIRE_VERSION,
            formula_version: "election_v2".into(),
            horse_selection: Some(HorseSelection::Mustang),
            route_summary: RouteSummary::default(),
            joined_at: UnixMillis::new(0),
            cleanup_pending: false,
        };
        assert!(matches!(
            validate_start_roster(&[player]),
            Err(ApiValidationError::MixedAuthorityFormula { .. })
        ));
    }

    #[test]
    fn authority_response_has_contract_shape() {
        let authority = AuthoritySummary {
            candidate_player_id: player_id(),
            formula_version: AUTHORITY_FORMULA_VERSION.into(),
            score_milli: 900,
        };
        let response = StartLobbyResponse {
            lobby_id: lobby_id(),
            state: LobbyState::Starting,
            map_seed: 42,
            authority,
            metadata: ResponseMetadata::default(),
        };
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["state"], "STARTING");
        assert!(value.get("dry_run").is_none());
    }
}
