//! Capability-protected, selected-lobby network inspection wire models.
//!
//! These DTOs describe cached control-plane, provider, and participant-report
//! facts. They do not make the control service a tailnet member, gameplay peer,
//! relay, or witness. In particular, a provider device count is not a roster or
//! identity claim, and participant reports remain untrusted reports.

use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{InputHash, LobbyId, LobbyState, PlayerId, UnixMillis, MAX_PLAYERS};

/// Version of the selected-lobby network inspection schema.
pub const LOBBY_NETWORK_SCHEMA_VERSION: u16 = 1;
/// Largest application nonce/reply RTT represented by this contract.
pub const MAX_APPLICATION_RTT_MS: u32 = 10_000;
/// Parts-per-million representation of 100 percent packet loss.
pub const MAX_APPLICATION_LOSS_PPM: u32 = 1_000_000;
/// Fixed-point scale used by `direct_ratio_milli`.
pub const DIRECT_RATIO_MILLI_SCALE: u32 = 1_000;
/// Maximum canonical textual length of a DNS name without a trailing root dot.
pub const MAX_TAILNET_DNS_NAME_LEN: usize = 253;

/// Backing behavior selected for a lobby network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackingMode {
    /// No provider mutation is allowed; a dedicated tailnet is only simulated.
    #[default]
    DryRun,
    /// Compatibility mode using scoped resources in one shared tailnet.
    SharedTailnet,
    /// A provider tailnet dedicated to exactly one lobby.
    TailnetPerLobby,
    /// A mode introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

impl From<crate::ProvisioningMode> for BackingMode {
    fn from(value: crate::ProvisioningMode) -> Self {
        match value {
            crate::ProvisioningMode::DryRun => Self::DryRun,
            crate::ProvisioningMode::SharedTailnet => Self::SharedTailnet,
            crate::ProvisioningMode::TailnetPerLobby => Self::TailnetPerLobby,
        }
    }
}

/// Isolation supplied by the selected backing mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkIsolation {
    /// No network exists.
    #[default]
    Simulated,
    /// Lobby resources coexist in a compatibility tailnet.
    Shared,
    /// One provider tailnet is dedicated to the lobby.
    Dedicated,
    /// An isolation class introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

/// Lifecycle of the backing network, independent of the lobby lifecycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NetworkLifecycle {
    /// No provider mutation was permitted and no tailnet exists.
    #[default]
    Simulated,
    /// A durable real-lobby lease and create intent exist.
    Reserved,
    /// A create is in flight or its durable intent awaits a result.
    Creating,
    /// Backing identity and required credential custody were committed.
    Active,
    /// The provider definitively rejected create before a resource could exist.
    CreateRejected,
    /// Create may have succeeded, but the result or credential commit is ambiguous.
    CreateUnknown,
    /// Durable cleanup intent exists.
    CleanupRequested,
    /// Cleanup failed, was denied, or has not completed.
    CleanupPending,
    /// Delete was acknowledged, but exact stable-identity absence is not proven.
    VerifyingAbsence,
    /// Exact dedicated identity absence and credential erasure are complete.
    DedicatedAbsent,
    /// Scoped shared-tailnet keys and devices are clean; the shared tailnet remains.
    SharedResourcesClean,
    /// Evidence is insufficient for safe automation.
    ManualRemediation,
    /// A lifecycle introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

/// Human-visible truth label that prevents simulated and shared views from
/// being mistaken for dedicated real networks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NetworkTruthLabel {
    /// Dry-run view; no provider tailnet exists.
    #[default]
    #[serde(rename = "SIMULATED — NO TAILNET EXISTS")]
    SimulatedNoTailnet,
    /// Real tailnet dedicated to this lobby.
    #[serde(rename = "REAL — DEDICATED TAILNET")]
    RealDedicatedTailnet,
    /// Real shared-tailnet compatibility mode.
    #[serde(rename = "REAL — SHARED COMPATIBILITY")]
    RealSharedCompatibility,
    /// A truth label introduced by a newer schema.
    #[serde(other, rename = "UNKNOWN")]
    Unknown,
}

/// Forward-compatible lobby lifecycle used only by the inspection view.
///
/// This mirrors [`LobbyState`] without weakening the state machine's exhaustive
/// transition checks when a newer inspection producer adds a state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InspectedLobbyLifecycle {
    /// Lobby record or provider provisioning is in progress.
    #[default]
    Provisioning,
    /// Lobby accepts joins and measurements.
    Forming,
    /// Lobby can elect an authority.
    Ready,
    /// Roster is frozen pending authority heartbeat.
    Starting,
    /// Peer-hosted gameplay is active.
    InMatch,
    /// Lobby teardown is in progress.
    Closing,
    /// Lobby failed; the independent network lifecycle may still require cleanup.
    Failed,
    /// Lobby TTL elapsed; the independent network lifecycle may still require cleanup.
    Expired,
    /// Lobby teardown was attempted; this is not proof that a tailnet is absent.
    Destroyed,
    /// A lobby lifecycle introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

impl From<LobbyState> for InspectedLobbyLifecycle {
    fn from(value: LobbyState) -> Self {
        match value {
            LobbyState::Provisioning => Self::Provisioning,
            LobbyState::Forming => Self::Forming,
            LobbyState::Ready => Self::Ready,
            LobbyState::Starting => Self::Starting,
            LobbyState::InMatch => Self::InMatch,
            LobbyState::Closing => Self::Closing,
            LobbyState::Failed => Self::Failed,
            LobbyState::Expired => Self::Expired,
            LobbyState::Destroyed => Self::Destroyed,
        }
    }
}

/// Origin of one displayed fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    /// Durable Spurfire control record or receipt event.
    ControlStore,
    /// Scoped provider API response.
    ProviderApi,
    /// Authenticated-but-untrusted participant report.
    ParticipantReport,
    /// Calculation over explicitly identified inputs.
    Derived,
    /// No source applies.
    #[default]
    None,
    /// A source introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

/// Assurance of one fact within its explicitly stated scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactAssurance {
    /// Authoritative only for the stated control, provider identity, or formula scope.
    Authoritative,
    /// Directly observed by the named source.
    Observed,
    /// Claimed by a participant and not elevated to gameplay truth.
    Reported,
    /// Deterministically calculated from identified inputs.
    Derived,
    /// No usable value is known.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Freshness evaluated from trusted service receipt or poll-completion time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Control state is current at response construction.
    Current,
    /// Cached observation remains inside its source-specific freshness window.
    Fresh,
    /// Last successful value is retained outside its freshness window.
    Stale,
    /// The fact does not apply to this mode or lifecycle.
    NotApplicable,
    /// No freshness statement can be made, including values added by newer schemas.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Why a value is unknown rather than false, zero, offline, or absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// The source has never produced a usable observation.
    #[default]
    NeverObserved,
    /// The source does not expose a live-verified field or semantic.
    Unsupported,
    /// Scoped provider access was denied.
    PermissionDenied,
    /// Collection exceeded its bounded timeout.
    Timeout,
    /// The source failed or returned an invalid response.
    SourceError,
    /// Sources or identity tuples disagree.
    Conflict,
    /// Startup or cleanup reconciliation has not reached a safe conclusion.
    ReconciliationPending,
    /// A formerly known value aged past the bounded retention window.
    StaleBeyondRetention,
    /// No value applies to this mode or lifecycle.
    NotApplicable,
    /// A reason introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

/// Directional path class reported by one participant for one target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionRouteClass {
    /// Direct end-to-end peer path.
    Direct,
    /// RustScale `Relay`, always presented precisely as Peer Relay.
    PeerRelay,
    /// Tailscale DERP relay path.
    DerpRelay,
    /// Reporter attempted but has no usable route.
    Unavailable,
    /// No usable claim exists or a newer route class was received.
    #[default]
    #[serde(other)]
    Unknown,
}

/// A value together with its source, assurance, time, and freshness.
///
/// `received_at` is the trusted service receipt or provider poll-completion
/// time used for freshness. A participant's wall clock never substitutes for
/// it. Unknown values are `null`; zero and `false` remain ordinary known values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact<T> {
    /// Last usable value, or `null` when no value is known.
    pub value: Option<T>,
    /// Source of the value or failed observation.
    pub source: FactSource,
    /// Assurance within the explicitly documented scope.
    pub assurance: FactAssurance,
    /// Time represented by the source value, when one exists.
    pub as_of: Option<UnixMillis>,
    /// Trusted service receipt or poll-completion time.
    pub received_at: Option<UnixMillis>,
    /// Freshness evaluated from `received_at`, not an untrusted peer clock.
    pub freshness: Freshness,
    /// Required reason when `value` is `null`; otherwise `null`.
    pub unknown_reason: Option<UnknownReason>,
}

impl<T> Fact<T> {
    /// Constructs a known fact. [`Fact::validate`] still checks provenance.
    #[must_use]
    pub fn known(
        value: T,
        source: FactSource,
        assurance: FactAssurance,
        as_of: Option<UnixMillis>,
        received_at: UnixMillis,
        freshness: Freshness,
    ) -> Self {
        Self {
            value: Some(value),
            source,
            assurance,
            as_of,
            received_at: Some(received_at),
            freshness,
            unknown_reason: None,
        }
    }

    /// Constructs an unknown fact without inventing a false, zero, or absent value.
    #[must_use]
    pub fn unknown(
        source: FactSource,
        reason: UnknownReason,
        received_at: Option<UnixMillis>,
    ) -> Self {
        Self {
            value: None,
            source,
            assurance: FactAssurance::Unknown,
            as_of: None,
            received_at,
            freshness: Freshness::Unknown,
            unknown_reason: Some(reason),
        }
    }

    /// Constructs the exact wire representation for a fact that does not apply.
    #[must_use]
    pub fn not_applicable() -> Self {
        Self {
            value: None,
            source: FactSource::None,
            assurance: FactAssurance::Unknown,
            as_of: None,
            received_at: None,
            freshness: Freshness::NotApplicable,
            unknown_reason: Some(UnknownReason::NotApplicable),
        }
    }

    /// Validates unknown/stale semantics and source-assurance combinations.
    pub fn validate(&self) -> Result<(), FactValidationError> {
        match self.value.as_ref() {
            Some(_) => {
                if self.assurance == FactAssurance::Unknown {
                    return Err(FactValidationError::KnownValueHasUnknownAssurance);
                }
                if self.unknown_reason.is_some() {
                    return Err(FactValidationError::KnownValueHasUnknownReason);
                }
                if self.source == FactSource::None || self.source == FactSource::Unknown {
                    return Err(FactValidationError::KnownValueHasNoUsableSource);
                }
                if self.received_at.is_none() {
                    return Err(FactValidationError::KnownValueMissingReceivedAt);
                }
                if matches!(
                    self.freshness,
                    Freshness::Unknown | Freshness::NotApplicable
                ) {
                    return Err(FactValidationError::KnownValueHasInvalidFreshness {
                        freshness: self.freshness,
                    });
                }
                if !source_supports_assurance(self.source, self.assurance) {
                    return Err(FactValidationError::InvalidSourceAssurance {
                        fact_source: self.source,
                        assurance: self.assurance,
                    });
                }
            }
            None => {
                if self.assurance != FactAssurance::Unknown {
                    return Err(FactValidationError::UnknownValueHasKnownAssurance);
                }
                if self.unknown_reason.is_none() {
                    return Err(FactValidationError::UnknownValueMissingReason);
                }
                if matches!(
                    self.freshness,
                    Freshness::Current | Freshness::Fresh | Freshness::Stale
                ) {
                    return Err(FactValidationError::UnknownValueHasKnownFreshness {
                        freshness: self.freshness,
                    });
                }
            }
        }

        let uses_not_applicable = self.freshness == Freshness::NotApplicable
            || self.unknown_reason == Some(UnknownReason::NotApplicable);
        if uses_not_applicable
            && !(self.value.is_none()
                && self.source == FactSource::None
                && self.assurance == FactAssurance::Unknown
                && self.as_of.is_none()
                && self.received_at.is_none()
                && self.freshness == Freshness::NotApplicable
                && self.unknown_reason == Some(UnknownReason::NotApplicable))
        {
            return Err(FactValidationError::InvalidNotApplicableShape);
        }

        Ok(())
    }
}

const fn source_supports_assurance(source: FactSource, assurance: FactAssurance) -> bool {
    match source {
        FactSource::ControlStore => matches!(assurance, FactAssurance::Authoritative),
        FactSource::ProviderApi => {
            matches!(
                assurance,
                FactAssurance::Authoritative | FactAssurance::Observed
            )
        }
        FactSource::ParticipantReport => matches!(assurance, FactAssurance::Reported),
        FactSource::Derived => {
            matches!(
                assurance,
                FactAssurance::Authoritative | FactAssurance::Derived
            )
        }
        FactSource::None | FactSource::Unknown => false,
    }
}

/// Invalid source, assurance, freshness, or unknown representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum FactValidationError {
    /// A present value cannot have unknown assurance.
    #[error("known value has unknown assurance")]
    KnownValueHasUnknownAssurance,
    /// A present value cannot carry an unknown reason.
    #[error("known value carries an unknown reason")]
    KnownValueHasUnknownReason,
    /// A present value requires a usable source.
    #[error("known value has no usable source")]
    KnownValueHasNoUsableSource,
    /// Freshness cannot be evaluated without a trusted receipt time.
    #[error("known value is missing received_at")]
    KnownValueMissingReceivedAt,
    /// A present value must be current, fresh, or stale.
    #[error("known value has invalid freshness {freshness:?}")]
    KnownValueHasInvalidFreshness {
        /// Invalid freshness.
        freshness: Freshness,
    },
    /// `null` always has unknown assurance.
    #[error("unknown value has non-unknown assurance")]
    UnknownValueHasKnownAssurance,
    /// `null` always carries a machine-readable reason.
    #[error("unknown value is missing unknown_reason")]
    UnknownValueMissingReason,
    /// Stale retains a last-known value; `null` cannot be current, fresh, or stale.
    #[error("unknown value has value-bearing freshness {freshness:?}")]
    UnknownValueHasKnownFreshness {
        /// Invalid freshness.
        freshness: Freshness,
    },
    /// Source and assurance make an unsupported truth claim.
    #[error("source {fact_source:?} cannot claim assurance {assurance:?}")]
    InvalidSourceAssurance {
        /// Fact source.
        fact_source: FactSource,
        /// Unsupported assurance.
        assurance: FactAssurance,
    },
    /// Not-applicable facts have one exact, unambiguous representation.
    #[error("not-applicable fact has inconsistent fields")]
    InvalidNotApplicableShape,
}

/// Validated provider-returned tailnet DNS name/FQDN.
///
/// The canonical value is lowercase ASCII without a trailing root dot. For
/// `tail9a1c23.ts.net`, `.net` is the TLD; the useful value represented by this
/// type is the complete `tail9a1c23.ts.net` DNS name/FQDN. This type is topology
/// metadata, not a credential, and its `Debug` output is intentionally omitted.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TailnetDnsName(String);

impl TailnetDnsName {
    /// Parses, validates, and canonicalizes a complete DNS name.
    pub fn parse(value: &str) -> Result<Self, TailnetDnsNameValidationError> {
        if value.is_empty() {
            return Err(TailnetDnsNameValidationError::Empty);
        }
        if !value.is_ascii() {
            return Err(TailnetDnsNameValidationError::NonAscii);
        }

        let without_root_dot = value.strip_suffix('.').unwrap_or(value);
        if without_root_dot.is_empty() {
            return Err(TailnetDnsNameValidationError::Empty);
        }
        if without_root_dot.len() > MAX_TAILNET_DNS_NAME_LEN {
            return Err(TailnetDnsNameValidationError::NameTooLong);
        }

        let labels: Vec<&str> = without_root_dot.split('.').collect();
        if labels.len() < 2 {
            return Err(TailnetDnsNameValidationError::NotFullyQualified);
        }
        for label in &labels {
            if label.is_empty() {
                return Err(TailnetDnsNameValidationError::EmptyLabel);
            }
            if label.len() > 63 {
                return Err(TailnetDnsNameValidationError::LabelTooLong);
            }
            let bytes = label.as_bytes();
            if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
                return Err(TailnetDnsNameValidationError::LabelBoundaryHyphen);
            }
            if !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            {
                return Err(TailnetDnsNameValidationError::InvalidCharacter);
            }
        }

        Ok(Self(without_root_dot.to_ascii_lowercase()))
    }

    /// Returns the canonical complete tailnet DNS name/FQDN.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the validated value.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for TailnetDnsName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TailnetDnsName(<topology-metadata>)")
    }
}

impl TryFrom<&str> for TailnetDnsName {
    type Error = TailnetDnsNameValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<String> for TailnetDnsName {
    type Error = TailnetDnsNameValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl Serialize for TailnetDnsName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TailnetDnsName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DnsNameVisitor;

        impl de::Visitor<'_> for DnsNameVisitor {
            type Value = TailnetDnsName;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .write_str("a complete ASCII DNS name without path, port, query, or fragment")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                TailnetDnsName::parse(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(DnsNameVisitor)
    }
}

/// Invalid complete tailnet DNS name/FQDN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TailnetDnsNameValidationError {
    /// Empty string or root-only input.
    #[error("tailnet DNS name must not be empty")]
    Empty,
    /// Provider path identities are ASCII DNS names.
    #[error("tailnet DNS name must be ASCII")]
    NonAscii,
    /// Canonical DNS names are at most 253 characters.
    #[error("tailnet DNS name exceeds 253 characters")]
    NameTooLong,
    /// A complete name contains at least one dot and two labels.
    #[error("tailnet DNS name must be fully qualified")]
    NotFullyQualified,
    /// Empty labels permit ambiguous or injected provider paths.
    #[error("tailnet DNS name contains an empty label")]
    EmptyLabel,
    /// DNS labels are at most 63 characters.
    #[error("tailnet DNS name contains a label longer than 63 characters")]
    LabelTooLong,
    /// DNS labels cannot begin or end with `-`.
    #[error("tailnet DNS label begins or ends with a hyphen")]
    LabelBoundaryHyphen,
    /// Only ASCII letters, digits, and interior hyphens are accepted.
    #[error("tailnet DNS name contains a non-DNS character")]
    InvalidCharacter,
}

/// Public backing identity and lifecycle. Provider stable IDs are deliberately
/// absent; this DTO also has no credential, endpoint, or device-identifier slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkBacking {
    /// Provisioning behavior used for the lobby.
    pub backing_mode: BackingMode,
    /// Real mode represented by dry run; `null` for real modes.
    pub simulates_mode: Option<BackingMode>,
    /// Isolation actually supplied.
    pub isolation: NetworkIsolation,
    /// Non-zero generation binding reports and lobby capabilities.
    pub network_generation: u64,
    /// Independent network lifecycle.
    pub network_lifecycle: NetworkLifecycle,
    /// Provider-returned complete DNS name/FQDN, never a guessed dry-run value.
    pub tailnet_dns_name: Fact<TailnetDnsName>,
    /// Always authoritative `false`; ownership never implies membership.
    pub control_service_member: Fact<bool>,
}

impl NetworkBacking {
    /// Constructs the safe dry-run backing projection without a fake FQDN.
    #[must_use]
    pub fn simulated(network_generation: u64, observed_at: UnixMillis) -> Self {
        Self {
            backing_mode: BackingMode::DryRun,
            simulates_mode: Some(BackingMode::TailnetPerLobby),
            isolation: NetworkIsolation::Simulated,
            network_generation,
            network_lifecycle: NetworkLifecycle::Simulated,
            tailnet_dns_name: Fact::not_applicable(),
            control_service_member: Fact::known(
                false,
                FactSource::ControlStore,
                FactAssurance::Authoritative,
                Some(observed_at),
                observed_at,
                Freshness::Current,
            ),
        }
    }
}

/// Enrollment and report counts with explicit, independent provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCounts {
    /// Control-authoritative roster count.
    pub roster_count: Fact<u32>,
    /// Devices enrolled as of the latest successful child-scoped provider poll.
    pub provider_enrolled_device_count: Fact<u32>,
    /// Provider online count; unknown/unsupported until semantics are live-verified.
    pub provider_online_device_count: Fact<u32>,
    /// Roster participants with an accepted fresh report.
    pub fresh_reporter_count: Fact<u32>,
    /// Accepted fresh directional rows; reverse paths are never inferred.
    pub fresh_directional_observation_count: Fact<u32>,
}

/// Directional route-class aggregate over the latest fresh report per target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteAggregate {
    /// Ordered roster directions expected (`n * (n - 1)`), not undirected pairs.
    pub expected_direction_count: Fact<u32>,
    /// Sum of the five route-class counts below.
    pub reported_direction_count: Fact<u32>,
    /// Direct directional rows.
    pub direct_count: Fact<u32>,
    /// Peer Relay directional rows.
    pub peer_relay_count: Fact<u32>,
    /// DERP Relay directional rows.
    pub derp_relay_count: Fact<u32>,
    /// Attempted directional rows without a usable route.
    pub unavailable_count: Fact<u32>,
    /// Directions without a usable route claim.
    pub unknown_count: Fact<u32>,
    /// `direct + peer_relay + derp_relay`.
    pub reachable_known_count: Fact<u32>,
    /// Floor of `1000 * direct / reachable_known`; `null` at denominator zero.
    pub direct_ratio_milli: Fact<u32>,
}

/// Bounded application nonce/reply quality aggregate.
///
/// These names intentionally cannot be populated from DERP-region latency,
/// WireGuard handshake timing, observer RTT, or legacy election RTT fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationQuality {
    /// Number of accepted directional application samples.
    pub sample_count: Fact<u32>,
    /// Median application nonce/reply RTT in milliseconds.
    pub application_rtt_ms_median: Fact<u32>,
    /// Nearest-rank p95 application nonce/reply RTT in milliseconds.
    pub application_rtt_ms_p95: Fact<u32>,
    /// Worst application nonce/reply RTT in milliseconds.
    pub application_rtt_ms_worst: Fact<u32>,
    /// Median application sequence-window loss in parts per million.
    pub application_loss_ppm_median: Fact<u32>,
}

/// Deterministic control-election reference shown by the inspector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlElectionReference {
    /// Formula applied to the identified input.
    pub formula_version: String,
    /// Deterministic winner over that input.
    pub winner_player_id: PlayerId,
    /// Winner score on the formula's milli scale.
    pub score_milli: u32,
    /// Canonical input fingerprint.
    pub input_hash: InputHash,
    /// Trusted service evaluation time.
    pub evaluated_at: UnixMillis,
    /// Assurance of the inputs, distinct from authoritative formula application.
    pub input_assurance: FactAssurance,
    /// True when the election used its documented degraded fallback.
    pub degraded: bool,
}

/// Accepted control-plane heartbeat receipt; not proof of current gameplay truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedHeartbeatReference {
    /// Player accepted as authority for the referenced input.
    pub player_id: PlayerId,
    /// Match authority epoch supplied by the authority contract.
    pub epoch: u64,
    /// Election input acknowledged by the heartbeat.
    pub input_hash: InputHash,
    /// Trusted service receipt time.
    pub received_at: UnixMillis,
}

/// Correlation summary of participant-reported current match authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerReportedMatchAuthority {
    /// Player most recently reported for the summarized epoch/input.
    pub player_id: PlayerId,
    /// Reported match authority epoch.
    pub epoch: u64,
    /// Reported authority input fingerprint.
    pub input_hash: InputHash,
    /// Fresh reporters considered.
    pub fresh_reporter_count: u32,
    /// Fresh reporters agreeing with this tuple.
    pub agreement_count: u32,
    /// Fresh reporters reporting a conflicting tuple.
    pub conflict_count: u32,
}

/// Authority facts kept separate so no reported claim becomes ranked proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAuthority {
    /// Authoritative formula application over identified, partly reported inputs.
    pub control_election: Fact<ControlElectionReference>,
    /// Authoritative only as a stored receipt event.
    pub last_accepted_heartbeat: Fact<AcceptedHeartbeatReference>,
    /// Authenticated-but-untrusted participant consensus/conflict summary.
    pub peer_reported_match_authority: Fact<PeerReportedMatchAuthority>,
}

/// Participant-safe explanation of cleanup progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantCleanupReason {
    /// No network cleanup intent exists yet.
    #[default]
    NotRequested,
    /// Dry run created no provider network.
    SimulatedNoTailnet,
    /// Cleanup intent is durably recorded.
    CleanupRequested,
    /// Cleanup has not completed or a retry is pending.
    CleanupPending,
    /// Delete was acknowledged but exact identity absence is still being checked.
    VerifyingExactAbsence,
    /// Exact dedicated identity absence and credential erasure are complete.
    DedicatedTailnetAbsent,
    /// Lobby-scoped shared-tailnet resources are clean; the shared tailnet remains.
    SharedResourcesClean,
    /// Safe automatic cleanup cannot continue with available evidence.
    ManualRemediationRequired,
    /// A reason introduced by a newer schema.
    #[serde(other)]
    Unknown,
}

/// Participant-safe cleanup state. It deliberately omits provider identity,
/// retry internals, vault state, and reconciliation codes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCleanup {
    /// Must equal the backing network lifecycle.
    pub network_lifecycle: NetworkLifecycle,
    /// Durable cleanup-intent event time.
    pub requested_at: Fact<UnixMillis>,
    /// Child-scoped delete acknowledgement time; not absence proof.
    pub delete_acknowledged_at: Fact<UnixMillis>,
    /// Time the exact-identity absence policy completed.
    pub absence_confirmed_at: Fact<UnixMillis>,
    /// Coarse participant-safe state explanation.
    pub participant_safe_reason: ParticipantCleanupReason,
}

/// Capability-protected cached view for one exact selected lobby.
///
/// This public/member DTO has no provider stable ID, raw device ID/tag,
/// credential, private endpoint, physical endpoint, packet data, or operator
/// reconciliation detail. Audience-specific operator data belongs in a
/// separate extension type outside this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyNetworkView {
    /// Inspection schema version, independent of gameplay wire version.
    pub schema_version: u16,
    /// Exact capability-bound lobby.
    pub lobby_id: LobbyId,
    /// Trusted service response-construction time.
    pub served_at: UnixMillis,
    /// Explicit real/simulated/shared truth label.
    pub truth_label: NetworkTruthLabel,
    /// Backing mode, isolation, generation, lifecycle, and safe identity.
    pub backing: NetworkBacking,
    /// Independent cached lobby lifecycle.
    pub lobby_lifecycle: Fact<InspectedLobbyLifecycle>,
    /// Independently qualified roster, enrollment, online, and report counts.
    pub counts: NetworkCounts,
    /// Directional route aggregate.
    pub routes: RouteAggregate,
    /// Application-level RTT/loss aggregate only.
    pub application_quality: ApplicationQuality,
    /// Separated control-election, receipt, and peer-reported authority facts.
    pub authority: NetworkAuthority,
    /// Participant-safe cleanup progress.
    pub cleanup: NetworkCleanup,
}

/// Conventional route-specific alias for [`LobbyNetworkView`].
pub type GetLobbyNetworkResponse = LobbyNetworkView;

impl LobbyNetworkView {
    /// Validates cross-field wire invariants without provider I/O or mutation.
    pub fn validate(&self) -> Result<(), NetworkViewValidationError> {
        if self.schema_version != LOBBY_NETWORK_SCHEMA_VERSION {
            return Err(NetworkViewValidationError::UnsupportedSchemaVersion {
                value: self.schema_version,
            });
        }
        if self.truth_label == NetworkTruthLabel::Unknown {
            return Err(NetworkViewValidationError::UnknownEnum {
                field: "truth_label",
            });
        }

        self.validate_backing()?;
        validate_fact("lobby_lifecycle", &self.lobby_lifecycle, self.served_at)?;
        require_known_provenance(
            "lobby_lifecycle",
            &self.lobby_lifecycle,
            FactSource::ControlStore,
            FactAssurance::Authoritative,
        )?;
        let lobby_lifecycle =
            self.lobby_lifecycle
                .value
                .ok_or(NetworkViewValidationError::RequiredKnownFact {
                    field: "lobby_lifecycle",
                })?;
        if lobby_lifecycle == InspectedLobbyLifecycle::Unknown {
            return Err(NetworkViewValidationError::UnknownEnum {
                field: "lobby_lifecycle.value",
            });
        }

        self.validate_counts()?;
        let roster_count = self.counts.roster_count.value.expect("validated above");
        let expected_directions = roster_count
            .checked_mul(roster_count.saturating_sub(1))
            .ok_or(NetworkViewValidationError::ArithmeticOverflow {
                field: "routes.expected_direction_count",
            })?;
        if self.routes.expected_direction_count.value != Some(expected_directions) {
            return Err(NetworkViewValidationError::ExpectedDirectionCountMismatch {
                roster_count,
                expected: expected_directions,
                actual: self.routes.expected_direction_count.value,
            });
        }

        self.validate_routes()?;
        self.validate_application_quality()?;
        self.validate_authority()?;
        self.validate_cleanup()?;

        if let (Some(samples), Some(reported)) = (
            self.application_quality.sample_count.value,
            self.routes.reported_direction_count.value,
        ) {
            if samples > reported {
                return Err(NetworkViewValidationError::CountExceeds {
                    field: "application_quality.sample_count",
                    value: samples,
                    maximum: reported,
                });
            }
        }

        if let Some(peer_authority) = &self.authority.peer_reported_match_authority.value {
            if peer_authority.fresh_reporter_count > roster_count {
                return Err(NetworkViewValidationError::CountExceeds {
                    field: "authority.peer_reported_match_authority.fresh_reporter_count",
                    value: peer_authority.fresh_reporter_count,
                    maximum: roster_count,
                });
            }
        }

        Ok(())
    }

    fn validate_backing(&self) -> Result<(), NetworkViewValidationError> {
        let backing = &self.backing;
        if backing.network_generation == 0 {
            return Err(NetworkViewValidationError::ZeroNetworkGeneration);
        }
        for (field, unknown) in [
            (
                "backing.backing_mode",
                backing.backing_mode == BackingMode::Unknown,
            ),
            (
                "backing.isolation",
                backing.isolation == NetworkIsolation::Unknown,
            ),
            (
                "backing.network_lifecycle",
                backing.network_lifecycle == NetworkLifecycle::Unknown,
            ),
        ] {
            if unknown {
                return Err(NetworkViewValidationError::UnknownEnum { field });
            }
        }
        if backing.simulates_mode == Some(BackingMode::Unknown) {
            return Err(NetworkViewValidationError::UnknownEnum {
                field: "backing.simulates_mode",
            });
        }

        validate_fact(
            "backing.tailnet_dns_name",
            &backing.tailnet_dns_name,
            self.served_at,
        )?;
        validate_fact(
            "backing.control_service_member",
            &backing.control_service_member,
            self.served_at,
        )?;
        require_known_provenance(
            "backing.control_service_member",
            &backing.control_service_member,
            FactSource::ControlStore,
            FactAssurance::Authoritative,
        )?;
        if backing.control_service_member.value != Some(false) {
            return Err(NetworkViewValidationError::ControlServiceMembershipMustBeFalse);
        }
        if backing.control_service_member.freshness != Freshness::Current {
            return Err(NetworkViewValidationError::InvalidFactFreshness {
                field: "backing.control_service_member",
                expected: Freshness::Current,
                actual: backing.control_service_member.freshness,
            });
        }

        if backing.tailnet_dns_name.value.is_some()
            && !matches!(
                (
                    backing.tailnet_dns_name.source,
                    backing.tailnet_dns_name.assurance
                ),
                (FactSource::ProviderApi, FactAssurance::Authoritative)
                    | (FactSource::ControlStore, FactAssurance::Authoritative)
            )
        {
            return Err(NetworkViewValidationError::InvalidFactProvenance {
                field: "backing.tailnet_dns_name",
            });
        }

        match backing.backing_mode {
            BackingMode::DryRun => {
                if self.truth_label != NetworkTruthLabel::SimulatedNoTailnet
                    || backing.simulates_mode != Some(BackingMode::TailnetPerLobby)
                    || backing.isolation != NetworkIsolation::Simulated
                    || backing.network_lifecycle != NetworkLifecycle::Simulated
                {
                    return Err(NetworkViewValidationError::BackingModeMismatch);
                }
                require_not_applicable("backing.tailnet_dns_name", &backing.tailnet_dns_name)?;
            }
            BackingMode::SharedTailnet => {
                if self.truth_label != NetworkTruthLabel::RealSharedCompatibility
                    || backing.simulates_mode.is_some()
                    || backing.isolation != NetworkIsolation::Shared
                    || matches!(
                        backing.network_lifecycle,
                        NetworkLifecycle::Simulated | NetworkLifecycle::DedicatedAbsent
                    )
                {
                    return Err(NetworkViewValidationError::BackingModeMismatch);
                }
                require_dns_name_for_active_backing(backing)?;
            }
            BackingMode::TailnetPerLobby => {
                if self.truth_label != NetworkTruthLabel::RealDedicatedTailnet
                    || backing.simulates_mode.is_some()
                    || backing.isolation != NetworkIsolation::Dedicated
                    || matches!(
                        backing.network_lifecycle,
                        NetworkLifecycle::Simulated | NetworkLifecycle::SharedResourcesClean
                    )
                {
                    return Err(NetworkViewValidationError::BackingModeMismatch);
                }
                require_dns_name_for_active_backing(backing)?;
                if matches!(
                    backing.network_lifecycle,
                    NetworkLifecycle::CreateRejected | NetworkLifecycle::DedicatedAbsent
                ) {
                    require_not_applicable("backing.tailnet_dns_name", &backing.tailnet_dns_name)?;
                }
            }
            BackingMode::Unknown => unreachable!("unknown mode rejected above"),
        }

        Ok(())
    }

    fn validate_counts(&self) -> Result<(), NetworkViewValidationError> {
        let counts = &self.counts;
        for (field, fact) in [
            ("counts.roster_count", &counts.roster_count),
            (
                "counts.provider_enrolled_device_count",
                &counts.provider_enrolled_device_count,
            ),
            (
                "counts.provider_online_device_count",
                &counts.provider_online_device_count,
            ),
            ("counts.fresh_reporter_count", &counts.fresh_reporter_count),
            (
                "counts.fresh_directional_observation_count",
                &counts.fresh_directional_observation_count,
            ),
        ] {
            validate_fact(field, fact, self.served_at)?;
        }

        require_known_provenance(
            "counts.roster_count",
            &counts.roster_count,
            FactSource::ControlStore,
            FactAssurance::Authoritative,
        )?;
        let roster_count =
            counts
                .roster_count
                .value
                .ok_or(NetworkViewValidationError::RequiredKnownFact {
                    field: "counts.roster_count",
                })?;
        if roster_count > u32::from(MAX_PLAYERS) {
            return Err(NetworkViewValidationError::CountExceeds {
                field: "counts.roster_count",
                value: roster_count,
                maximum: u32::from(MAX_PLAYERS),
            });
        }

        for (field, fact) in [
            (
                "counts.provider_enrolled_device_count",
                &counts.provider_enrolled_device_count,
            ),
            (
                "counts.provider_online_device_count",
                &counts.provider_online_device_count,
            ),
        ] {
            if fact.value.is_some()
                && (fact.source != FactSource::ProviderApi
                    || fact.assurance != FactAssurance::Observed)
            {
                return Err(NetworkViewValidationError::InvalidFactProvenance { field });
            }
        }
        for (field, fact) in [
            ("counts.fresh_reporter_count", &counts.fresh_reporter_count),
            (
                "counts.fresh_directional_observation_count",
                &counts.fresh_directional_observation_count,
            ),
        ] {
            if fact.value.is_some()
                && (fact.source != FactSource::Derived || fact.assurance != FactAssurance::Derived)
            {
                return Err(NetworkViewValidationError::InvalidFactProvenance { field });
            }
        }

        if let Some(fresh_reporters) = counts.fresh_reporter_count.value {
            if fresh_reporters > roster_count {
                return Err(NetworkViewValidationError::CountExceeds {
                    field: "counts.fresh_reporter_count",
                    value: fresh_reporters,
                    maximum: roster_count,
                });
            }
        }
        let maximum_directions = roster_count
            .checked_mul(roster_count.saturating_sub(1))
            .ok_or(NetworkViewValidationError::ArithmeticOverflow {
                field: "counts.fresh_directional_observation_count",
            })?;
        if let Some(fresh_directions) = counts.fresh_directional_observation_count.value {
            if fresh_directions > maximum_directions {
                return Err(NetworkViewValidationError::CountExceeds {
                    field: "counts.fresh_directional_observation_count",
                    value: fresh_directions,
                    maximum: maximum_directions,
                });
            }
        }
        if let (Some(online), Some(enrolled)) = (
            counts.provider_online_device_count.value,
            counts.provider_enrolled_device_count.value,
        ) {
            if online > enrolled {
                return Err(NetworkViewValidationError::CountExceeds {
                    field: "counts.provider_online_device_count",
                    value: online,
                    maximum: enrolled,
                });
            }
        }

        if self.backing.backing_mode == BackingMode::DryRun {
            require_not_applicable(
                "counts.provider_enrolled_device_count",
                &counts.provider_enrolled_device_count,
            )?;
            require_not_applicable(
                "counts.provider_online_device_count",
                &counts.provider_online_device_count,
            )?;
        }

        Ok(())
    }

    fn validate_routes(&self) -> Result<(), NetworkViewValidationError> {
        let routes = &self.routes;
        for (field, fact) in [
            (
                "routes.expected_direction_count",
                &routes.expected_direction_count,
            ),
            (
                "routes.reported_direction_count",
                &routes.reported_direction_count,
            ),
            ("routes.direct_count", &routes.direct_count),
            ("routes.peer_relay_count", &routes.peer_relay_count),
            ("routes.derp_relay_count", &routes.derp_relay_count),
            ("routes.unavailable_count", &routes.unavailable_count),
            ("routes.unknown_count", &routes.unknown_count),
            (
                "routes.reachable_known_count",
                &routes.reachable_known_count,
            ),
            ("routes.direct_ratio_milli", &routes.direct_ratio_milli),
        ] {
            validate_fact(field, fact, self.served_at)?;
            if fact.value.is_some()
                && (fact.source != FactSource::Derived || fact.assurance != FactAssurance::Derived)
            {
                return Err(NetworkViewValidationError::InvalidFactProvenance { field });
            }
        }

        if routes.expected_direction_count.value.is_none() {
            return Err(NetworkViewValidationError::RequiredKnownFact {
                field: "routes.expected_direction_count",
            });
        }

        let class_values = [
            routes.reported_direction_count.value,
            routes.direct_count.value,
            routes.peer_relay_count.value,
            routes.derp_relay_count.value,
            routes.unavailable_count.value,
            routes.unknown_count.value,
            routes.reachable_known_count.value,
        ];
        let known_count = class_values.iter().filter(|value| value.is_some()).count();
        if known_count != 0 && known_count != class_values.len() {
            return Err(NetworkViewValidationError::IncompleteRouteAggregate);
        }
        if known_count == 0 {
            if routes.direct_ratio_milli.value.is_some() {
                return Err(NetworkViewValidationError::IncompleteRouteAggregate);
            }
            return Ok(());
        }

        let reported = routes.reported_direction_count.value.expect("complete set");
        let direct = routes.direct_count.value.expect("complete set");
        let peer_relay = routes.peer_relay_count.value.expect("complete set");
        let derp_relay = routes.derp_relay_count.value.expect("complete set");
        let unavailable = routes.unavailable_count.value.expect("complete set");
        let unknown = routes.unknown_count.value.expect("complete set");
        let reachable = routes.reachable_known_count.value.expect("complete set");

        let class_sum = [direct, peer_relay, derp_relay, unavailable, unknown]
            .into_iter()
            .try_fold(0_u32, u32::checked_add)
            .ok_or(NetworkViewValidationError::ArithmeticOverflow {
                field: "routes.reported_direction_count",
            })?;
        if reported != class_sum {
            return Err(NetworkViewValidationError::ReportedDirectionCountMismatch {
                expected: class_sum,
                actual: reported,
            });
        }
        let reachable_sum = [direct, peer_relay, derp_relay]
            .into_iter()
            .try_fold(0_u32, u32::checked_add)
            .ok_or(NetworkViewValidationError::ArithmeticOverflow {
                field: "routes.reachable_known_count",
            })?;
        if reachable != reachable_sum {
            return Err(NetworkViewValidationError::ReachableCountMismatch {
                expected: reachable_sum,
                actual: reachable,
            });
        }
        let expected = routes
            .expected_direction_count
            .value
            .expect("required above");
        if reported > expected {
            return Err(NetworkViewValidationError::CountExceeds {
                field: "routes.reported_direction_count",
                value: reported,
                maximum: expected,
            });
        }

        if reachable == 0 {
            require_not_applicable("routes.direct_ratio_milli", &routes.direct_ratio_milli)?;
        } else {
            let numerator = u64::from(DIRECT_RATIO_MILLI_SCALE) * u64::from(direct);
            let expected_ratio = u32::try_from(numerator / u64::from(reachable))
                .expect("ratio is bounded by DIRECT_RATIO_MILLI_SCALE");
            if routes.direct_ratio_milli.value != Some(expected_ratio) {
                return Err(NetworkViewValidationError::DirectRatioMismatch {
                    expected: expected_ratio,
                    actual: routes.direct_ratio_milli.value,
                });
            }
        }

        Ok(())
    }

    fn validate_application_quality(&self) -> Result<(), NetworkViewValidationError> {
        let quality = &self.application_quality;
        for (field, fact) in [
            ("application_quality.sample_count", &quality.sample_count),
            (
                "application_quality.application_rtt_ms_median",
                &quality.application_rtt_ms_median,
            ),
            (
                "application_quality.application_rtt_ms_p95",
                &quality.application_rtt_ms_p95,
            ),
            (
                "application_quality.application_rtt_ms_worst",
                &quality.application_rtt_ms_worst,
            ),
            (
                "application_quality.application_loss_ppm_median",
                &quality.application_loss_ppm_median,
            ),
        ] {
            validate_fact(field, fact, self.served_at)?;
            if fact.value.is_some()
                && (fact.source != FactSource::Derived || fact.assurance != FactAssurance::Derived)
            {
                return Err(NetworkViewValidationError::InvalidFactProvenance { field });
            }
        }

        let metrics = [
            quality.application_rtt_ms_median.value,
            quality.application_rtt_ms_p95.value,
            quality.application_rtt_ms_worst.value,
            quality.application_loss_ppm_median.value,
        ];
        match quality.sample_count.value {
            Some(0) => {
                if metrics.iter().any(Option::is_some) {
                    return Err(NetworkViewValidationError::QualityWithoutSamples);
                }
            }
            Some(_) => {
                if metrics.iter().any(Option::is_none) {
                    return Err(NetworkViewValidationError::IncompleteApplicationQuality);
                }
            }
            None => {
                if metrics.iter().any(Option::is_some) {
                    return Err(NetworkViewValidationError::QualityWithoutSamples);
                }
            }
        }

        for (field, value) in [
            (
                "application_quality.application_rtt_ms_median",
                quality.application_rtt_ms_median.value,
            ),
            (
                "application_quality.application_rtt_ms_p95",
                quality.application_rtt_ms_p95.value,
            ),
            (
                "application_quality.application_rtt_ms_worst",
                quality.application_rtt_ms_worst.value,
            ),
        ] {
            if value.is_some_and(|value| value > MAX_APPLICATION_RTT_MS) {
                return Err(NetworkViewValidationError::MetricOutOfRange {
                    field,
                    maximum: MAX_APPLICATION_RTT_MS,
                    value: value.expect("checked as Some"),
                });
            }
        }
        if let Some(loss) = quality.application_loss_ppm_median.value {
            if loss > MAX_APPLICATION_LOSS_PPM {
                return Err(NetworkViewValidationError::MetricOutOfRange {
                    field: "application_quality.application_loss_ppm_median",
                    maximum: MAX_APPLICATION_LOSS_PPM,
                    value: loss,
                });
            }
        }

        if let (Some(median), Some(p95), Some(worst)) = (
            quality.application_rtt_ms_median.value,
            quality.application_rtt_ms_p95.value,
            quality.application_rtt_ms_worst.value,
        ) {
            if median > p95 || p95 > worst {
                return Err(NetworkViewValidationError::InvalidRttOrdering { median, p95, worst });
            }
        }

        Ok(())
    }

    fn validate_authority(&self) -> Result<(), NetworkViewValidationError> {
        let authority = &self.authority;
        validate_fact(
            "authority.control_election",
            &authority.control_election,
            self.served_at,
        )?;
        validate_fact(
            "authority.last_accepted_heartbeat",
            &authority.last_accepted_heartbeat,
            self.served_at,
        )?;
        validate_fact(
            "authority.peer_reported_match_authority",
            &authority.peer_reported_match_authority,
            self.served_at,
        )?;

        if let Some(election) = &authority.control_election.value {
            require_known_provenance(
                "authority.control_election",
                &authority.control_election,
                FactSource::Derived,
                FactAssurance::Authoritative,
            )?;
            validate_formula_version(&election.formula_version)?;
            if election.score_milli > DIRECT_RATIO_MILLI_SCALE {
                return Err(NetworkViewValidationError::MetricOutOfRange {
                    field: "authority.control_election.score_milli",
                    maximum: DIRECT_RATIO_MILLI_SCALE,
                    value: election.score_milli,
                });
            }
            if matches!(
                election.input_assurance,
                FactAssurance::Authoritative | FactAssurance::Unknown
            ) {
                return Err(NetworkViewValidationError::InvalidAuthorityInputAssurance {
                    assurance: election.input_assurance,
                });
            }
            if authority.control_election.as_of != Some(election.evaluated_at) {
                return Err(NetworkViewValidationError::AuthorityTimestampMismatch {
                    field: "authority.control_election.evaluated_at",
                });
            }
        }

        if let Some(heartbeat) = authority.last_accepted_heartbeat.value {
            require_known_provenance(
                "authority.last_accepted_heartbeat",
                &authority.last_accepted_heartbeat,
                FactSource::ControlStore,
                FactAssurance::Authoritative,
            )?;
            if authority.last_accepted_heartbeat.received_at != Some(heartbeat.received_at) {
                return Err(NetworkViewValidationError::AuthorityTimestampMismatch {
                    field: "authority.last_accepted_heartbeat.received_at",
                });
            }
        }

        if let Some(peer_report) = authority.peer_reported_match_authority.value {
            require_known_provenance(
                "authority.peer_reported_match_authority",
                &authority.peer_reported_match_authority,
                FactSource::ParticipantReport,
                FactAssurance::Reported,
            )?;
            let classified = peer_report
                .agreement_count
                .checked_add(peer_report.conflict_count)
                .ok_or(NetworkViewValidationError::ArithmeticOverflow {
                    field: "authority.peer_reported_match_authority",
                })?;
            if classified > peer_report.fresh_reporter_count {
                return Err(NetworkViewValidationError::AuthorityReportCountMismatch {
                    fresh_reporter_count: peer_report.fresh_reporter_count,
                    agreement_count: peer_report.agreement_count,
                    conflict_count: peer_report.conflict_count,
                });
            }
        }

        Ok(())
    }

    fn validate_cleanup(&self) -> Result<(), NetworkViewValidationError> {
        let cleanup = &self.cleanup;
        if cleanup.network_lifecycle != self.backing.network_lifecycle {
            return Err(NetworkViewValidationError::CleanupLifecycleMismatch {
                backing: self.backing.network_lifecycle,
                cleanup: cleanup.network_lifecycle,
            });
        }
        if cleanup.participant_safe_reason == ParticipantCleanupReason::Unknown {
            return Err(NetworkViewValidationError::UnknownEnum {
                field: "cleanup.participant_safe_reason",
            });
        }

        for (field, fact) in [
            ("cleanup.requested_at", &cleanup.requested_at),
            (
                "cleanup.delete_acknowledged_at",
                &cleanup.delete_acknowledged_at,
            ),
            (
                "cleanup.absence_confirmed_at",
                &cleanup.absence_confirmed_at,
            ),
        ] {
            validate_fact(field, fact, self.served_at)?;
            if fact.value.is_some()
                && (fact.source != FactSource::ControlStore
                    || fact.assurance != FactAssurance::Authoritative)
            {
                return Err(NetworkViewValidationError::InvalidFactProvenance { field });
            }
        }

        let requested = cleanup.requested_at.value;
        let acknowledged = cleanup.delete_acknowledged_at.value;
        let absent = cleanup.absence_confirmed_at.value;
        if let (Some(requested), Some(acknowledged)) = (requested, acknowledged) {
            if acknowledged < requested {
                return Err(NetworkViewValidationError::CleanupTimestampOrder);
            }
        }
        if let (Some(acknowledged), Some(absent)) = (acknowledged, absent) {
            if absent < acknowledged {
                return Err(NetworkViewValidationError::CleanupTimestampOrder);
            }
        }
        if let (Some(requested), Some(absent)) = (requested, absent) {
            if absent < requested {
                return Err(NetworkViewValidationError::CleanupTimestampOrder);
            }
        }

        let reason_valid = match cleanup.network_lifecycle {
            NetworkLifecycle::Simulated => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::SimulatedNoTailnet
            }
            NetworkLifecycle::Reserved
            | NetworkLifecycle::Creating
            | NetworkLifecycle::Active
            | NetworkLifecycle::CreateRejected => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::NotRequested
            }
            NetworkLifecycle::CreateUnknown | NetworkLifecycle::ManualRemediation => {
                cleanup.participant_safe_reason
                    == ParticipantCleanupReason::ManualRemediationRequired
            }
            NetworkLifecycle::CleanupRequested => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::CleanupRequested
                    && requested.is_some()
            }
            NetworkLifecycle::CleanupPending => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::CleanupPending
                    && requested.is_some()
            }
            NetworkLifecycle::VerifyingAbsence => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::VerifyingExactAbsence
                    && requested.is_some()
                    && acknowledged.is_some()
                    && absent.is_none()
            }
            NetworkLifecycle::DedicatedAbsent => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::DedicatedTailnetAbsent
                    && absent.is_some()
            }
            NetworkLifecycle::SharedResourcesClean => {
                cleanup.participant_safe_reason == ParticipantCleanupReason::SharedResourcesClean
            }
            NetworkLifecycle::Unknown => false,
        };
        if !reason_valid {
            return Err(NetworkViewValidationError::CleanupReasonMismatch {
                lifecycle: cleanup.network_lifecycle,
                reason: cleanup.participant_safe_reason,
            });
        }

        if cleanup.network_lifecycle == NetworkLifecycle::Simulated {
            require_not_applicable("cleanup.requested_at", &cleanup.requested_at)?;
            require_not_applicable(
                "cleanup.delete_acknowledged_at",
                &cleanup.delete_acknowledged_at,
            )?;
            require_not_applicable(
                "cleanup.absence_confirmed_at",
                &cleanup.absence_confirmed_at,
            )?;
        }

        Ok(())
    }
}

fn require_dns_name_for_active_backing(
    backing: &NetworkBacking,
) -> Result<(), NetworkViewValidationError> {
    if matches!(
        backing.network_lifecycle,
        NetworkLifecycle::Active
            | NetworkLifecycle::CleanupRequested
            | NetworkLifecycle::CleanupPending
            | NetworkLifecycle::VerifyingAbsence
    ) && backing.tailnet_dns_name.value.is_none()
    {
        return Err(NetworkViewValidationError::RequiredKnownFact {
            field: "backing.tailnet_dns_name",
        });
    }
    Ok(())
}

fn validate_formula_version(value: &str) -> Result<(), NetworkViewValidationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(NetworkViewValidationError::InvalidFormulaVersion);
    }
    Ok(())
}

fn validate_fact<T>(
    field: &'static str,
    fact: &Fact<T>,
    served_at: UnixMillis,
) -> Result<(), NetworkViewValidationError> {
    fact.validate()
        .map_err(|error| NetworkViewValidationError::InvalidFact { field, error })?;
    if fact
        .received_at
        .is_some_and(|received_at| received_at > served_at)
    {
        return Err(NetworkViewValidationError::FactReceivedAfterServedAt { field });
    }
    Ok(())
}

fn require_known_provenance<T>(
    field: &'static str,
    fact: &Fact<T>,
    source: FactSource,
    assurance: FactAssurance,
) -> Result<(), NetworkViewValidationError> {
    if fact.value.is_some() && (fact.source != source || fact.assurance != assurance) {
        return Err(NetworkViewValidationError::InvalidFactProvenance { field });
    }
    Ok(())
}

fn require_not_applicable<T: PartialEq>(
    field: &'static str,
    fact: &Fact<T>,
) -> Result<(), NetworkViewValidationError> {
    if fact != &Fact::not_applicable() {
        return Err(NetworkViewValidationError::FactMustBeNotApplicable { field });
    }
    Ok(())
}

/// Invalid selected-lobby network inspection view.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum NetworkViewValidationError {
    /// Consumers must explicitly negotiate a different schema.
    #[error("unsupported lobby network schema version {value}")]
    UnsupportedSchemaVersion {
        /// Received schema version.
        value: u16,
    },
    /// Newer enum values deserialize safely but are not emitted as validated v1 output.
    #[error("{field} contains an enum value unknown to schema v1")]
    UnknownEnum {
        /// Invalid field path.
        field: &'static str,
    },
    /// A fact violates unknown, freshness, or assurance semantics.
    #[error("invalid {field}: {error}")]
    InvalidFact {
        /// Invalid field path.
        field: &'static str,
        /// Fact-level failure.
        #[source]
        error: FactValidationError,
    },
    /// Trusted receipt times cannot be later than response construction.
    #[error("{field}.received_at is later than served_at")]
    FactReceivedAfterServedAt {
        /// Invalid field path.
        field: &'static str,
    },
    /// A known fact was assigned the wrong source or assurance.
    #[error("{field} has invalid provenance")]
    InvalidFactProvenance {
        /// Invalid field path.
        field: &'static str,
    },
    /// A lifecycle or mode requires a value that is currently `null`.
    #[error("{field} must contain a known value")]
    RequiredKnownFact {
        /// Missing field path.
        field: &'static str,
    },
    /// The field must use the exact not-applicable envelope.
    #[error("{field} must be not_applicable")]
    FactMustBeNotApplicable {
        /// Invalid field path.
        field: &'static str,
    },
    /// Fixed invariant facts must remain current.
    #[error("{field} freshness must be {expected:?}, got {actual:?}")]
    InvalidFactFreshness {
        /// Invalid field path.
        field: &'static str,
        /// Required freshness.
        expected: Freshness,
        /// Actual freshness.
        actual: Freshness,
    },
    /// Capabilities and reports cannot bind to generation zero.
    #[error("network_generation must be non-zero")]
    ZeroNetworkGeneration,
    /// Ownership does not permit control-service tailnet membership.
    #[error("control_service_member must be authoritative false")]
    ControlServiceMembershipMustBeFalse,
    /// Truth label, mode, simulated mode, isolation, and lifecycle disagree.
    #[error("backing mode, isolation, lifecycle, simulation, or truth label disagree")]
    BackingModeMismatch,
    /// A bounded count exceeds its contextual maximum.
    #[error("{field} value {value} exceeds {maximum}")]
    CountExceeds {
        /// Invalid field path.
        field: &'static str,
        /// Invalid value.
        value: u32,
        /// Contextual maximum.
        maximum: u32,
    },
    /// Aggregate arithmetic overflowed before comparison.
    #[error("{field} arithmetic overflow")]
    ArithmeticOverflow {
        /// Invalid field path.
        field: &'static str,
    },
    /// Directed roster matrix uses `n * (n - 1)`, never undirected pairs.
    #[error("roster_count {roster_count} requires {expected} directions, got {actual:?}")]
    ExpectedDirectionCountMismatch {
        /// Roster size.
        roster_count: u32,
        /// Ordered direction count.
        expected: u32,
        /// Supplied value, or `null`.
        actual: Option<u32>,
    },
    /// Route-class facts come from one coherent aggregate snapshot.
    #[error("route aggregate is only partially known")]
    IncompleteRouteAggregate,
    /// Reported directions must equal all five route-class counts.
    #[error("reported_direction_count {actual} must equal route-class sum {expected}")]
    ReportedDirectionCountMismatch {
        /// Sum of five route-class counts.
        expected: u32,
        /// Supplied aggregate count.
        actual: u32,
    },
    /// Reachable count excludes unavailable and unknown directions.
    #[error("reachable_known_count {actual} must equal reachable route sum {expected}")]
    ReachableCountMismatch {
        /// Sum of direct, Peer Relay, and DERP Relay.
        expected: u32,
        /// Supplied aggregate count.
        actual: u32,
    },
    /// Ratio uses only reachable known directions as its denominator.
    #[error("direct_ratio_milli {actual:?} must equal {expected}")]
    DirectRatioMismatch {
        /// Exact floored ratio.
        expected: u32,
        /// Supplied ratio, or `null`.
        actual: Option<u32>,
    },
    /// Application metrics cannot exist without accepted samples.
    #[error("application quality contains metrics without samples")]
    QualityWithoutSamples,
    /// A positive sample count carries every contracted application metric.
    #[error("application quality is only partially known")]
    IncompleteApplicationQuality,
    /// Bounded application/election metric is out of range.
    #[error("{field} value {value} exceeds {maximum}")]
    MetricOutOfRange {
        /// Invalid field path.
        field: &'static str,
        /// Inclusive maximum.
        maximum: u32,
        /// Invalid value.
        value: u32,
    },
    /// RTT order statistics must be monotonic.
    #[error("application RTT ordering is invalid: median={median}, p95={p95}, worst={worst}")]
    InvalidRttOrdering {
        /// Median RTT.
        median: u32,
        /// P95 RTT.
        p95: u32,
        /// Worst RTT.
        worst: u32,
    },
    /// Authority formula labels are bounded safe identifiers.
    #[error("authority formula_version must be 1..=64 safe ASCII identifier bytes")]
    InvalidFormulaVersion,
    /// Formula output is not gameplay-authoritative input truth.
    #[error("control election input assurance {assurance:?} is invalid")]
    InvalidAuthorityInputAssurance {
        /// Invalid assurance.
        assurance: FactAssurance,
    },
    /// Nested authority event time must equal its fact-envelope time.
    #[error("{field} disagrees with its fact envelope")]
    AuthorityTimestampMismatch {
        /// Invalid field path.
        field: &'static str,
    },
    /// Agreement and conflict rows cannot exceed fresh reporters.
    #[error(
        "authority report counts agreement={agreement_count} conflict={conflict_count} exceed fresh reporters={fresh_reporter_count}"
    )]
    AuthorityReportCountMismatch {
        /// Fresh reporters considered.
        fresh_reporter_count: u32,
        /// Agreeing reporters.
        agreement_count: u32,
        /// Conflicting reporters.
        conflict_count: u32,
    },
    /// Cleanup repeats the independent backing lifecycle exactly.
    #[error("cleanup lifecycle {cleanup:?} disagrees with backing lifecycle {backing:?}")]
    CleanupLifecycleMismatch {
        /// Backing lifecycle.
        backing: NetworkLifecycle,
        /// Cleanup lifecycle.
        cleanup: NetworkLifecycle,
    },
    /// Cleanup event timestamps must be monotonic when present.
    #[error("cleanup timestamps are out of order")]
    CleanupTimestampOrder,
    /// Participant-safe reason must describe the exact lifecycle without overclaiming.
    #[error("cleanup reason {reason:?} does not describe lifecycle {lifecycle:?}")]
    CleanupReasonMismatch {
        /// Network lifecycle.
        lifecycle: NetworkLifecycle,
        /// Supplied safe reason.
        reason: ParticipantCleanupReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOBBY_ID: &str = "00000000-0000-4000-8000-000000000001";
    const PLAYER_ID: &str = "00000000-0000-4000-8000-000000000002";
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn lobby_id() -> LobbyId {
        LobbyId::parse(LOBBY_ID).unwrap()
    }

    fn player_id() -> PlayerId {
        PlayerId::parse(PLAYER_ID).unwrap()
    }

    fn input_hash() -> InputHash {
        HASH.parse().unwrap()
    }

    fn control<T>(value: T, now: UnixMillis) -> Fact<T> {
        Fact::known(
            value,
            FactSource::ControlStore,
            FactAssurance::Authoritative,
            Some(now),
            now,
            Freshness::Current,
        )
    }

    fn derived<T>(value: T, now: UnixMillis) -> Fact<T> {
        Fact::known(
            value,
            FactSource::Derived,
            FactAssurance::Derived,
            Some(now),
            now,
            Freshness::Fresh,
        )
    }

    fn unknown<T>() -> Fact<T> {
        Fact::unknown(FactSource::None, UnknownReason::NeverObserved, None)
    }

    fn zero_route_aggregate(expected: u32, now: UnixMillis) -> RouteAggregate {
        RouteAggregate {
            expected_direction_count: derived(expected, now),
            reported_direction_count: derived(0, now),
            direct_count: derived(0, now),
            peer_relay_count: derived(0, now),
            derp_relay_count: derived(0, now),
            unavailable_count: derived(0, now),
            unknown_count: derived(0, now),
            reachable_known_count: derived(0, now),
            direct_ratio_milli: Fact::not_applicable(),
        }
    }

    fn no_application_quality(now: UnixMillis) -> ApplicationQuality {
        ApplicationQuality {
            sample_count: derived(0, now),
            application_rtt_ms_median: Fact::not_applicable(),
            application_rtt_ms_p95: Fact::not_applicable(),
            application_rtt_ms_worst: Fact::not_applicable(),
            application_loss_ppm_median: Fact::not_applicable(),
        }
    }

    fn no_authority() -> NetworkAuthority {
        NetworkAuthority {
            control_election: unknown(),
            last_accepted_heartbeat: unknown(),
            peer_reported_match_authority: unknown(),
        }
    }

    fn simulated_view() -> LobbyNetworkView {
        let now = UnixMillis::new(10_000);
        LobbyNetworkView {
            schema_version: LOBBY_NETWORK_SCHEMA_VERSION,
            lobby_id: lobby_id(),
            served_at: now,
            truth_label: NetworkTruthLabel::SimulatedNoTailnet,
            backing: NetworkBacking::simulated(1, now),
            lobby_lifecycle: control(InspectedLobbyLifecycle::Forming, now),
            counts: NetworkCounts {
                roster_count: control(0, now),
                provider_enrolled_device_count: Fact::not_applicable(),
                provider_online_device_count: Fact::not_applicable(),
                fresh_reporter_count: derived(0, now),
                fresh_directional_observation_count: derived(0, now),
            },
            routes: zero_route_aggregate(0, now),
            application_quality: no_application_quality(now),
            authority: no_authority(),
            cleanup: NetworkCleanup {
                network_lifecycle: NetworkLifecycle::Simulated,
                requested_at: Fact::not_applicable(),
                delete_acknowledged_at: Fact::not_applicable(),
                absence_confirmed_at: Fact::not_applicable(),
                participant_safe_reason: ParticipantCleanupReason::SimulatedNoTailnet,
            },
        }
    }

    fn dedicated_view() -> LobbyNetworkView {
        let now = UnixMillis::new(20_000);
        let mut view = simulated_view();
        view.served_at = now;
        view.truth_label = NetworkTruthLabel::RealDedicatedTailnet;
        view.backing = NetworkBacking {
            backing_mode: BackingMode::TailnetPerLobby,
            simulates_mode: None,
            isolation: NetworkIsolation::Dedicated,
            network_generation: 7,
            network_lifecycle: NetworkLifecycle::Active,
            tailnet_dns_name: Fact::known(
                TailnetDnsName::parse("Tail9A1C23.TS.NET.").unwrap(),
                FactSource::ProviderApi,
                FactAssurance::Authoritative,
                Some(now),
                now,
                Freshness::Fresh,
            ),
            control_service_member: control(false, now),
        };
        view.lobby_lifecycle = control(InspectedLobbyLifecycle::InMatch, now);
        view.counts = NetworkCounts {
            roster_count: control(3, now),
            provider_enrolled_device_count: Fact::known(
                3,
                FactSource::ProviderApi,
                FactAssurance::Observed,
                Some(now),
                now,
                Freshness::Fresh,
            ),
            provider_online_device_count: Fact::unknown(
                FactSource::ProviderApi,
                UnknownReason::Unsupported,
                Some(now),
            ),
            fresh_reporter_count: derived(2, now),
            fresh_directional_observation_count: derived(5, now),
        };
        view.routes = RouteAggregate {
            expected_direction_count: derived(6, now),
            reported_direction_count: derived(5, now),
            direct_count: derived(2, now),
            peer_relay_count: derived(1, now),
            derp_relay_count: derived(1, now),
            unavailable_count: derived(1, now),
            unknown_count: derived(0, now),
            reachable_known_count: derived(4, now),
            direct_ratio_milli: derived(500, now),
        };
        view.application_quality = ApplicationQuality {
            sample_count: derived(4, now),
            application_rtt_ms_median: derived(24, now),
            application_rtt_ms_p95: derived(55, now),
            application_rtt_ms_worst: derived(80, now),
            application_loss_ppm_median: derived(1_000, now),
        };
        view.authority = NetworkAuthority {
            control_election: Fact::known(
                ControlElectionReference {
                    formula_version: "election_v1".into(),
                    winner_player_id: player_id(),
                    score_milli: 820,
                    input_hash: input_hash(),
                    evaluated_at: now,
                    input_assurance: FactAssurance::Reported,
                    degraded: false,
                },
                FactSource::Derived,
                FactAssurance::Authoritative,
                Some(now),
                now,
                Freshness::Current,
            ),
            last_accepted_heartbeat: Fact::known(
                AcceptedHeartbeatReference {
                    player_id: player_id(),
                    epoch: 2,
                    input_hash: input_hash(),
                    received_at: now,
                },
                FactSource::ControlStore,
                FactAssurance::Authoritative,
                Some(now),
                now,
                Freshness::Current,
            ),
            peer_reported_match_authority: Fact::known(
                PeerReportedMatchAuthority {
                    player_id: player_id(),
                    epoch: 2,
                    input_hash: input_hash(),
                    fresh_reporter_count: 2,
                    agreement_count: 1,
                    conflict_count: 1,
                },
                FactSource::ParticipantReport,
                FactAssurance::Reported,
                Some(now),
                now,
                Freshness::Fresh,
            ),
        };
        view.cleanup = NetworkCleanup {
            network_lifecycle: NetworkLifecycle::Active,
            requested_at: unknown(),
            delete_acknowledged_at: unknown(),
            absence_confirmed_at: unknown(),
            participant_safe_reason: ParticipantCleanupReason::NotRequested,
        };
        view
    }

    #[test]
    fn tailnet_dns_name_is_precise_canonical_fqdn() {
        let name = TailnetDnsName::parse("Tail9A1C23.TS.NET.").unwrap();
        assert_eq!(name.as_str(), "tail9a1c23.ts.net");
        assert_eq!(
            serde_json::to_string(&name).unwrap(),
            r#""tail9a1c23.ts.net""#
        );
        assert_eq!(
            serde_json::from_str::<TailnetDnsName>(r#""TAIL9A1C23.TS.NET.""#)
                .unwrap()
                .as_str(),
            "tail9a1c23.ts.net"
        );
        assert!(!format!("{name:?}").contains(name.as_str()));
    }

    #[test]
    fn tailnet_dns_name_rejects_provider_path_and_url_injection() {
        let invalid = [
            "",
            ".",
            "tailnet",
            "tail..ts.net",
            ".tail.ts.net",
            "tail.ts.net..",
            "-tail.ts.net",
            "tail-.ts.net",
            "tail_.ts.net",
            "tail/../other.ts.net",
            "tail%2fother.ts.net",
            "tail.ts.net?delete=other",
            "tail.ts.net#fragment",
            "user@tail.ts.net",
            "tail.ts.net:443",
            "tail\nts.net",
            "táil.ts.net",
        ];
        for value in invalid {
            assert!(TailnetDnsName::parse(value).is_err(), "accepted {value:?}");
            let json = serde_json::to_string(value).unwrap();
            assert!(serde_json::from_str::<TailnetDnsName>(&json).is_err());
        }

        let long_label = format!("{}.ts.net", "a".repeat(64));
        assert_eq!(
            TailnetDnsName::parse(&long_label),
            Err(TailnetDnsNameValidationError::LabelTooLong)
        );
        let long_name = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(62),
        ]
        .join(".");
        assert_eq!(long_name.len(), 254);
        assert_eq!(
            TailnetDnsName::parse(&long_name),
            Err(TailnetDnsNameValidationError::NameTooLong)
        );
    }

    #[test]
    fn all_new_enums_tolerate_additive_wire_values() {
        assert_eq!(
            serde_json::from_str::<BackingMode>(r#""mesh_per_lobby""#).unwrap(),
            BackingMode::Unknown
        );
        assert_eq!(
            serde_json::from_str::<NetworkIsolation>(r#""air_gapped""#).unwrap(),
            NetworkIsolation::Unknown
        );
        assert_eq!(
            serde_json::from_str::<NetworkLifecycle>(r#""QUARANTINED""#).unwrap(),
            NetworkLifecycle::Unknown
        );
        assert_eq!(
            serde_json::from_str::<NetworkTruthLabel>(r#""FUTURE""#).unwrap(),
            NetworkTruthLabel::Unknown
        );
        assert_eq!(
            serde_json::from_str::<InspectedLobbyLifecycle>(r#""MIGRATING""#).unwrap(),
            InspectedLobbyLifecycle::Unknown
        );
        assert_eq!(
            serde_json::from_str::<FactSource>(r#""witness""#).unwrap(),
            FactSource::Unknown
        );
        assert_eq!(
            serde_json::from_str::<FactAssurance>(r#""corroborated""#).unwrap(),
            FactAssurance::Unknown
        );
        assert_eq!(
            serde_json::from_str::<Freshness>(r#""expiring""#).unwrap(),
            Freshness::Unknown
        );
        assert_eq!(
            serde_json::from_str::<UnknownReason>(r#""new_reason""#).unwrap(),
            UnknownReason::Unknown
        );
        assert_eq!(
            serde_json::from_str::<InspectionRouteClass>(r#""moon_relay""#).unwrap(),
            InspectionRouteClass::Unknown
        );
        assert_eq!(
            serde_json::from_str::<ParticipantCleanupReason>(r#""quarantined""#).unwrap(),
            ParticipantCleanupReason::Unknown
        );
    }

    #[test]
    fn enum_wire_names_preserve_contract_terminology() {
        assert_eq!(
            serde_json::to_string(&BackingMode::TailnetPerLobby).unwrap(),
            r#""tailnet_per_lobby""#
        );
        assert_eq!(
            serde_json::to_string(&NetworkLifecycle::DedicatedAbsent).unwrap(),
            r#""DEDICATED_ABSENT""#
        );
        assert_eq!(
            serde_json::to_string(&InspectionRouteClass::PeerRelay).unwrap(),
            r#""peer_relay""#
        );
        assert_eq!(
            serde_json::to_string(&NetworkTruthLabel::RealDedicatedTailnet).unwrap(),
            r#""REAL — DEDICATED TAILNET""#
        );
    }

    #[test]
    fn fact_validation_preserves_unknown_and_stale_semantics() {
        let now = UnixMillis::new(1_000);
        assert!(control(false, now).validate().is_ok());
        assert!(
            Fact::<u32>::unknown(FactSource::ProviderApi, UnknownReason::Timeout, Some(now))
                .validate()
                .is_ok()
        );
        assert!(Fact::<u32>::not_applicable().validate().is_ok());

        let stale = Fact::known(
            4_u32,
            FactSource::ProviderApi,
            FactAssurance::Observed,
            Some(now),
            now,
            Freshness::Stale,
        );
        assert!(stale.validate().is_ok());
        assert_eq!(stale.value, Some(4));

        let mut invalid = Fact::<u32>::unknown(
            FactSource::ProviderApi,
            UnknownReason::SourceError,
            Some(now),
        );
        invalid.freshness = Freshness::Stale;
        assert_eq!(
            invalid.validate(),
            Err(FactValidationError::UnknownValueHasKnownFreshness {
                freshness: Freshness::Stale
            })
        );

        let mut false_zero = derived(0_u32, now);
        false_zero.unknown_reason = Some(UnknownReason::SourceError);
        assert_eq!(
            false_zero.validate(),
            Err(FactValidationError::KnownValueHasUnknownReason)
        );
    }

    #[test]
    fn fact_validation_rejects_truth_escalation() {
        let now = UnixMillis::new(1_000);
        let cases = [
            (
                Fact::known(
                    1_u32,
                    FactSource::ParticipantReport,
                    FactAssurance::Authoritative,
                    Some(now),
                    now,
                    Freshness::Fresh,
                ),
                FactValidationError::InvalidSourceAssurance {
                    fact_source: FactSource::ParticipantReport,
                    assurance: FactAssurance::Authoritative,
                },
            ),
            (
                Fact::known(
                    1_u32,
                    FactSource::None,
                    FactAssurance::Observed,
                    Some(now),
                    now,
                    Freshness::Fresh,
                ),
                FactValidationError::KnownValueHasNoUsableSource,
            ),
        ];
        for (fact, expected) in cases {
            assert_eq!(fact.validate(), Err(expected));
        }

        let malformed: Fact<u32> = serde_json::from_str(
            r#"{"value":null,"source":"provider_api","assurance":"observed","as_of":null,"received_at":1000,"freshness":"unknown","unknown_reason":"timeout"}"#,
        )
        .unwrap();
        assert_eq!(
            malformed.validate(),
            Err(FactValidationError::UnknownValueHasKnownAssurance)
        );
    }

    #[test]
    fn dry_run_view_is_simulated_null_fqdn_and_secret_free() {
        let view = simulated_view();
        view.validate().unwrap();
        let wire = serde_json::to_value(&view).unwrap();

        assert_eq!(wire["schema_version"], LOBBY_NETWORK_SCHEMA_VERSION);
        assert_eq!(wire["truth_label"], "SIMULATED — NO TAILNET EXISTS");
        assert_eq!(wire["backing"]["backing_mode"], "dry_run");
        assert_eq!(wire["backing"]["simulates_mode"], "tailnet_per_lobby");
        assert_eq!(wire["backing"]["isolation"], "simulated");
        assert_eq!(wire["backing"]["network_lifecycle"], "SIMULATED");
        assert!(wire["backing"]["tailnet_dns_name"]["value"].is_null());
        assert_eq!(
            wire["backing"]["tailnet_dns_name"]["freshness"],
            "not_applicable"
        );
        assert_eq!(wire["backing"]["control_service_member"]["value"], false);

        let text = serde_json::to_string(&view).unwrap();
        for forbidden in [
            "dry-run.invalid",
            "provider_tailnet_id",
            "device_id",
            "device_tags",
            "auth_key",
            "oauth",
            "private_endpoint",
            "100.64.0.1",
        ] {
            assert!(
                !text.contains(forbidden),
                "leaked forbidden field {forbidden}"
            );
        }
    }

    #[test]
    fn dedicated_view_round_trips_and_ignores_additive_fields() {
        let view = dedicated_view();
        view.validate().unwrap();
        let mut wire = serde_json::to_value(&view).unwrap();
        assert_eq!(
            wire["backing"]["tailnet_dns_name"]["value"],
            "tail9a1c23.ts.net"
        );
        assert!(wire.get("provider_tailnet_id").is_none());
        wire.as_object_mut()
            .unwrap()
            .insert("future_summary".into(), serde_json::json!({"safe": true}));
        wire["backing"]
            .as_object_mut()
            .unwrap()
            .insert("future_backing_field".into(), serde_json::json!(42));

        let decoded: LobbyNetworkView = serde_json::from_value(wire).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, view);
    }

    #[test]
    fn backing_invariants_prevent_simulation_or_membership_mislabeling() {
        let mut view = simulated_view();
        view.backing.control_service_member = control(true, view.served_at);
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::ControlServiceMembershipMustBeFalse)
        );

        let mut view = simulated_view();
        view.truth_label = NetworkTruthLabel::RealDedicatedTailnet;
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::BackingModeMismatch)
        );

        let mut view = dedicated_view();
        view.backing.tailnet_dns_name = Fact::not_applicable();
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::RequiredKnownFact {
                field: "backing.tailnet_dns_name"
            })
        );
    }

    #[test]
    fn counts_distinguish_roster_enrollment_online_and_reporters() {
        let mut view = dedicated_view();
        assert!(view.validate().is_ok());
        assert_eq!(view.counts.roster_count.value, Some(3));
        assert_eq!(view.counts.provider_enrolled_device_count.value, Some(3));
        assert_eq!(view.counts.provider_online_device_count.value, None);
        assert_eq!(
            view.counts.provider_online_device_count.unknown_reason,
            Some(UnknownReason::Unsupported)
        );

        view.counts.provider_online_device_count = Fact::known(
            4,
            FactSource::ProviderApi,
            FactAssurance::Observed,
            Some(view.served_at),
            view.served_at,
            Freshness::Fresh,
        );
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::CountExceeds {
                field: "counts.provider_online_device_count",
                value: 4,
                maximum: 3
            })
        );
    }

    #[test]
    fn directional_route_aggregate_never_infers_reverse_edges() {
        let mut view = dedicated_view();
        // Three players have six ordered directions, not three undirected pairs.
        assert_eq!(view.routes.expected_direction_count.value, Some(6));
        view.validate().unwrap();

        view.routes.expected_direction_count = derived(3, view.served_at);
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::ExpectedDirectionCountMismatch {
                roster_count: 3,
                expected: 6,
                actual: Some(3)
            })
        );
    }

    #[test]
    fn route_aggregate_uses_exact_class_sum_and_reachable_denominator() {
        let mut view = dedicated_view();
        view.routes = RouteAggregate {
            expected_direction_count: derived(6, view.served_at),
            reported_direction_count: derived(6, view.served_at),
            direct_count: derived(2, view.served_at),
            peer_relay_count: derived(1, view.served_at),
            derp_relay_count: derived(2, view.served_at),
            unavailable_count: derived(1, view.served_at),
            unknown_count: derived(0, view.served_at),
            reachable_known_count: derived(5, view.served_at),
            direct_ratio_milli: derived(400, view.served_at),
        };
        view.application_quality.sample_count = derived(4, view.served_at);
        view.validate().unwrap();

        view.routes.direct_ratio_milli = derived(333, view.served_at);
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::DirectRatioMismatch {
                expected: 400,
                actual: Some(333)
            })
        );

        let mut view = simulated_view();
        view.routes.direct_ratio_milli = derived(0, view.served_at);
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::FactMustBeNotApplicable {
                field: "routes.direct_ratio_milli"
            })
        );
    }

    #[test]
    fn application_quality_is_explicitly_bounded_and_null_when_unknown() {
        let mut view = simulated_view();
        view.validate().unwrap();
        let wire = serde_json::to_value(&view).unwrap();
        assert!(wire["application_quality"]["application_rtt_ms_median"]["value"].is_null());

        let now = view.served_at;
        view.application_quality = ApplicationQuality {
            sample_count: derived(1, now),
            application_rtt_ms_median: derived(100, now),
            application_rtt_ms_p95: derived(90, now),
            application_rtt_ms_worst: derived(110, now),
            application_loss_ppm_median: derived(0, now),
        };
        assert!(matches!(
            view.validate(),
            Err(NetworkViewValidationError::InvalidRttOrdering { .. })
        ));

        view.application_quality.application_rtt_ms_p95 = derived(100, now);
        view.application_quality.application_rtt_ms_worst =
            derived(MAX_APPLICATION_RTT_MS + 1, now);
        assert!(matches!(
            view.validate(),
            Err(NetworkViewValidationError::MetricOutOfRange {
                field: "application_quality.application_rtt_ms_worst",
                ..
            })
        ));
    }

    #[test]
    fn authority_formula_receipt_and_peer_report_keep_distinct_assurance() {
        let mut view = dedicated_view();
        view.validate().unwrap();
        assert_eq!(
            view.authority.control_election.assurance,
            FactAssurance::Authoritative
        );
        assert_eq!(
            view.authority.last_accepted_heartbeat.source,
            FactSource::ControlStore
        );
        assert_eq!(
            view.authority.peer_reported_match_authority.assurance,
            FactAssurance::Reported
        );

        view.authority
            .control_election
            .value
            .as_mut()
            .unwrap()
            .input_assurance = FactAssurance::Authoritative;
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::InvalidAuthorityInputAssurance {
                assurance: FactAssurance::Authoritative
            })
        );
    }

    #[test]
    fn cleanup_ack_and_destroyed_lobby_do_not_claim_tailnet_absence() {
        let mut view = dedicated_view();
        let now = view.served_at;
        view.lobby_lifecycle = control(InspectedLobbyLifecycle::Destroyed, now);
        view.backing.network_lifecycle = NetworkLifecycle::VerifyingAbsence;
        view.cleanup = NetworkCleanup {
            network_lifecycle: NetworkLifecycle::VerifyingAbsence,
            requested_at: control(UnixMillis::new(15_000), now),
            delete_acknowledged_at: control(UnixMillis::new(16_000), now),
            absence_confirmed_at: Fact::unknown(
                FactSource::ControlStore,
                UnknownReason::ReconciliationPending,
                Some(now),
            ),
            participant_safe_reason: ParticipantCleanupReason::VerifyingExactAbsence,
        };
        view.validate().unwrap();
        assert_eq!(
            view.backing.network_lifecycle,
            NetworkLifecycle::VerifyingAbsence
        );
        assert_eq!(view.cleanup.absence_confirmed_at.value, None);

        view.backing.network_lifecycle = NetworkLifecycle::DedicatedAbsent;
        view.backing.tailnet_dns_name = Fact::not_applicable();
        view.cleanup.network_lifecycle = NetworkLifecycle::DedicatedAbsent;
        view.cleanup.participant_safe_reason = ParticipantCleanupReason::DedicatedTailnetAbsent;
        assert!(matches!(
            view.validate(),
            Err(NetworkViewValidationError::CleanupReasonMismatch { .. })
        ));

        view.cleanup.absence_confirmed_at = control(UnixMillis::new(19_000), now);
        view.validate().unwrap();
    }

    #[test]
    fn trusted_receipt_cannot_be_after_served_at() {
        let mut view = simulated_view();
        view.counts.roster_count.received_at = Some(view.served_at.saturating_add(1));
        assert_eq!(
            view.validate(),
            Err(NetworkViewValidationError::FactReceivedAfterServedAt {
                field: "counts.roster_count"
            })
        );
    }

    #[test]
    fn network_schema_does_not_change_gameplay_wire_version() {
        assert_eq!(LOBBY_NETWORK_SCHEMA_VERSION, 1);
        assert_eq!(crate::WIRE_VERSION, crate::CURRENT_WIRE_VERSION);
    }
}
