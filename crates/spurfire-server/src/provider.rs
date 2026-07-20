//! Network-provider boundary and Tailscale/dry-run implementations.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use spurfire_control::{
    AuthKeyOpts, ChildPolicyEvidence, ChildTailnetPolicy, ChildTailscaleClient, ControlError,
    TailscaleClient,
};

use crate::vault::{ChildVaultIdentity, EncryptedChildVault, VaultError};
use spurfire_protocol::{
    CapabilitiesResponse, CapabilityModeStatus, CapabilityModes, LobbyId, NetworkLifecycle,
    PlannedAction, PlayerId, ProvisioningMode, ResponseMetadata, TailnetDnsName, UnixMillis,
    DRY_RUN_AUTH_KEY,
};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use zeroize::{Zeroize, Zeroizing};

#[cfg(not(test))]
const CHILD_POLICY_GATE_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const CHILD_POLICY_GATE_TIMEOUT: Duration = Duration::from_millis(100);

/// Provider request made while a lobby record is being prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareLobbyRequest {
    /// Public lobby identifier.
    pub lobby_id: LobbyId,
    /// Non-zero generation reserved before provider I/O.
    pub network_generation: u64,
    /// Requested backing mode.
    pub mode: ProvisioningMode,
    /// Whether this operation must be simulated.
    pub dry_run: bool,
}

/// Exact non-secret provider identity captured from a typed response.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderNetworkIdentity {
    /// Stable organization tailnet ID. Dedicated mode always requires it.
    pub provider_tailnet_id: Option<String>,
    /// Canonical complete tailnet DNS name/FQDN.
    pub tailnet_dns_name: TailnetDnsName,
}

impl ProviderNetworkIdentity {
    /// Validates a provider identity before it can become destructive input.
    pub fn validate_for_mode(&self, mode: ProvisioningMode) -> Result<(), ProviderError> {
        if mode == ProvisioningMode::TailnetPerLobby
            && self
                .provider_tailnet_id
                .as_deref()
                .is_none_or(|id| !valid_provider_tailnet_id(id))
        {
            return Err(ProviderError::IdentityMismatch);
        }
        Ok(())
    }
}

impl fmt::Debug for ProviderNetworkIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderNetworkIdentity")
            .field("provider_tailnet_id", &"<operator-metadata>")
            .field("tailnet_dns_name", &"<topology-metadata>")
            .finish()
    }
}

/// Non-secret provider state retained with a lobby.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedNetwork {
    /// Tailnet selector needed by later provider calls.
    pub tailnet: String,
    /// Exact provider identity; absent only in dry run.
    pub identity: Option<ProviderNetworkIdentity>,
    /// Digest-only evidence of a successful restrictive child-policy readback.
    pub child_policy_evidence: Option<ChildPolicyEvidence>,
    /// True when the provider suppressed all mutations.
    pub dry_run: bool,
    /// Response metadata for the create operation.
    pub metadata: ResponseMetadata,
}

/// Request to mint one player credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintCredentialRequest {
    /// Lobby receiving the player.
    pub lobby_id: LobbyId,
    /// Durable generation to which this issuance is bound.
    pub network_generation: u64,
    /// Exact dedicated identity, when applicable.
    pub identity: Option<ProviderNetworkIdentity>,
    /// Provisioning mode selected by the durable lobby record.
    pub mode: ProvisioningMode,
    /// Player receiving the credential.
    pub player_id: PlayerId,
    /// Provider tailnet selector.
    pub tailnet: String,
    /// Lobby-confined ownership tag.
    pub tag: String,
    /// Absolute key expiry.
    pub expires_at: UnixMillis,
    /// Whether the provider must perform no mutation.
    pub dry_run: bool,
}

/// A secret string that always redacts its diagnostic representation.
#[derive(PartialEq, Eq)]
pub struct SecretString(Option<Zeroizing<String>>);

impl SecretString {
    /// Wraps provider-returned secret material.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Some(Zeroizing::new(value.into())))
    }

    /// Transfers the protected allocation into the one allowed join response without copying it.
    #[must_use]
    pub fn into_zeroizing(mut self) -> Zeroizing<String> {
        self.0.take().expect("secret is present until transferred")
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Provider result for a first credential issue.
#[derive(Debug, PartialEq, Eq)]
pub struct MintedCredential {
    /// Non-secret receipt identifier.
    pub credential_id: String,
    /// Key material returned exactly once.
    pub auth_key: SecretString,
    /// Tailnet the client should enroll into.
    pub tailnet: String,
    /// Response metadata, including dry-run plans when applicable.
    pub metadata: ResponseMetadata,
}

/// One non-secret auth-key receipt considered during cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialCleanup {
    /// Provider key identifier, never key material.
    pub credential_id: String,
    /// Key expiry. An already-expired key needs no upstream revoke call.
    pub expires_at: UnixMillis,
}

/// Request for lobby resource cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupLobbyRequest {
    /// Lobby being cleaned.
    pub lobby_id: LobbyId,
    /// Durable generation being cleaned.
    pub network_generation: u64,
    /// Exact provider identity bound to this generation.
    pub identity: Option<ProviderNetworkIdentity>,
    /// Provisioning mode selected by the durable lobby record.
    pub mode: ProvisioningMode,
    /// Provider tailnet selector.
    pub tailnet: String,
    /// Tag used to discover only this lobby's devices.
    pub tag: String,
    /// Unconsumed key receipts, revoked before any device discovery.
    pub credentials: Vec<CredentialCleanup>,
    /// Whether lobby-tagged devices should be discovered and deleted.
    pub include_devices: bool,
    /// Deterministic cleanup time used to recognize expired credentials.
    pub now: UnixMillis,
    /// Whether all mutating work must be simulated.
    pub dry_run: bool,
}

/// Cleanup outcome. Individual device identifiers are intentionally absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupOutcome {
    /// True when capability-dependent cleanup should be retried.
    pub cleanup_pending: bool,
    /// Child-scoped delete returned success/404; this is not absence proof.
    pub delete_acknowledged: bool,
    /// Process-local child credential material was erased after delete.
    pub child_secret_erased: bool,
    /// Credential receipts successfully revoked or confirmed expired.
    pub revoked_credential_ids: Vec<String>,
    /// Number of device delete calls attempted.
    pub attempted_device_deletes: usize,
    /// Dry-run response metadata.
    pub metadata: ResponseMetadata,
}

/// Bounded read request for coarse provider enrollment metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveNetworkRequest {
    /// Exact selected lobby.
    pub lobby_id: LobbyId,
    /// Generation bound to the selected identity.
    pub network_generation: u64,
    /// Provisioning mode for the cached record.
    pub mode: ProvisioningMode,
    /// Exact identity for dedicated mode.
    pub identity: Option<ProviderNetworkIdentity>,
    /// Provider selector used by shared compatibility mode.
    pub tailnet: String,
    /// Lobby tag used only to count shared-tailnet devices.
    pub tag: String,
    /// Simulation records never call the provider.
    pub dry_run: bool,
}

/// Coarse provider observation with no device identifiers, tags, or hostnames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDeviceObservation {
    /// Devices enrolled as of one successful scoped poll.
    pub enrolled_device_count: u32,
}

/// Exact stable-ID parent-organization presence check used only by cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TailnetPresenceRequest {
    /// Exact lobby whose durable identity is being reconciled.
    pub lobby_id: LobbyId,
    /// Durable generation being reconciled.
    pub network_generation: u64,
    /// Dedicated provider identity; stable ID and FQDN must both match.
    pub identity: ProviderNetworkIdentity,
}

/// Cached, non-secret capability verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// OAuth client-credentials exchange succeeded.
    pub oauth_token_ok: bool,
    /// Read-only organization tailnet listing succeeded.
    pub can_manage_organization_tailnets: bool,
    /// Non-mutating shared-tailnet auth-key scope probe succeeded.
    pub can_mint_auth_keys: bool,
    /// Shared-tailnet device-list probe succeeded.
    pub can_list_devices: bool,
    /// Shared-tailnet ACL policy probe succeeded.
    pub can_manage_acl: bool,
}

impl ProviderCapabilities {
    /// Conservative default used before probes complete.
    #[must_use]
    pub const fn blocked() -> Self {
        Self {
            oauth_token_ok: false,
            can_manage_organization_tailnets: false,
            can_mint_auth_keys: false,
            can_list_devices: false,
            can_manage_acl: false,
        }
    }

    /// Fully available capability set, primarily useful for verified adapters
    /// and deterministic tests.
    #[must_use]
    pub const fn available() -> Self {
        Self {
            oauth_token_ok: true,
            can_manage_organization_tailnets: true,
            can_mint_auth_keys: true,
            can_list_devices: true,
            can_manage_acl: true,
        }
    }

    /// Whether shared-tailnet creation may advance to `FORMING`.
    #[must_use]
    pub const fn shared_tailnet_available(self) -> bool {
        self.oauth_token_ok
            && self.can_mint_auth_keys
            && self.can_list_devices
            && self.can_manage_acl
    }

    /// Whether organization-tailnet creation may advance to `FORMING`.
    #[must_use]
    pub const fn tailnet_per_lobby_available(self) -> bool {
        self.oauth_token_ok && self.can_manage_organization_tailnets
    }

    /// Whether the requested real provisioning mode is ready.
    #[must_use]
    pub const fn mode_available(self, mode: ProvisioningMode) -> bool {
        match mode {
            ProvisioningMode::SharedTailnet => self.shared_tailnet_available(),
            ProvisioningMode::TailnetPerLobby => self.tailnet_per_lobby_available(),
            ProvisioningMode::DryRun => true,
        }
    }

    /// Stable reason for a fail-closed provisioning transition.
    #[must_use]
    pub const fn blocked_state_reason(self, mode: ProvisioningMode) -> &'static str {
        if !self.oauth_token_ok {
            "token_fetch_failed"
        } else if matches!(mode, ProvisioningMode::TailnetPerLobby) {
            "provisioning_blocked_organization_tailnets"
        } else if !self.can_mint_auth_keys {
            "provisioning_blocked_auth_keys_403"
        } else if !self.can_list_devices {
            "provisioning_blocked_devices_403"
        } else {
            "provisioning_blocked_acl_403"
        }
    }

    /// Converts the cache into the public capability response.
    #[must_use]
    pub fn response(self, metadata: ResponseMetadata) -> CapabilitiesResponse {
        CapabilitiesResponse {
            real_lobby_creation_authorized: false,
            real_lobby_join_authorized: false,
            oauth_token_ok: self.oauth_token_ok,
            can_manage_organization_tailnets: self.can_manage_organization_tailnets,
            can_mint_auth_keys: self.can_mint_auth_keys,
            can_list_devices: self.can_list_devices,
            can_manage_acl: self.can_manage_acl,
            modes: CapabilityModes {
                shared_tailnet: if self.shared_tailnet_available() {
                    CapabilityModeStatus::Available
                } else {
                    CapabilityModeStatus::BlockedScopes
                },
                tailnet_per_lobby: if self.tailnet_per_lobby_available() {
                    CapabilityModeStatus::Available
                } else {
                    CapabilityModeStatus::BlockedOrganizationAccess
                },
            },
            metadata,
        }
    }
}

/// Safe provider failure classification. Upstream bodies are always discarded.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// The deployment-wide default-off gate rejected a real provider mutation.
    #[error("real provider mutations are disabled")]
    RealMutationsDisabled,
    /// Durable lobby/generation/stable-ID/FQDN and provider custody disagree.
    #[error("provider identity tuple mismatch; manual remediation is required")]
    IdentityMismatch,
    /// A child-tailnet record survived but its exact encrypted/process-local OAuth custody did not.
    #[error("child tailnet credentials are unavailable; manual remediation is required")]
    ChildSecretUnavailable,
    /// A created child failed the restrictive policy/readback gate. Exact identity and digest-only
    /// evidence are retained so cleanup can continue without a display-name lookup.
    #[error("child-tailnet restrictive policy gate failed closed")]
    ChildPolicyGate {
        /// Exact provider identity captured before policy application.
        identity: ProviderNetworkIdentity,
        /// Expected normalized semantic digest; contains no provider body.
        expected_digest: String,
        /// Stable non-secret failure status.
        status: ChildPolicyStatus,
        /// Whether exact child-scoped deletion returned success/404.
        delete_acknowledged: bool,
    },
    /// OAuth scopes denied a required operation.
    #[error("provider scopes are insufficient for {operation}")]
    InsufficientScopes {
        /// Stable operation label, never request data.
        operation: &'static str,
    },
    /// Upstream returned a non-success status other than a scope denial.
    #[error("provider returned HTTP {status} for {operation}")]
    Upstream {
        /// Stable operation label.
        operation: &'static str,
        /// Numeric upstream status only.
        status: u16,
    },
    /// Transport, environment, or response decoding failed.
    #[error("provider is unavailable for {operation}")]
    Unavailable {
        /// Stable operation label.
        operation: &'static str,
    },
}

/// Safe child-policy gate status persisted/exposed only as coarse evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildPolicyStatus {
    /// Provider readback differed semantically from the generated policy.
    Mismatch,
    /// Child scope was denied policy write or read.
    Denied,
    /// Transport, timeout, or decoding prevented a matching readback.
    Unavailable,
}

impl ChildPolicyStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mismatch => "mismatch",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
        }
    }
}

impl ProviderError {
    /// Machine-readable lobby reason suitable for a public response.
    #[must_use]
    pub fn state_reason(&self) -> &'static str {
        match self {
            Self::RealMutationsDisabled => "real_mutations_disabled",
            Self::IdentityMismatch => "provider_identity_mismatch_manual_remediation",
            Self::ChildSecretUnavailable => "child_secret_unavailable_manual_remediation",
            Self::ChildPolicyGate {
                status: ChildPolicyStatus::Mismatch,
                ..
            } => "child_policy_readback_mismatch",
            Self::ChildPolicyGate {
                status: ChildPolicyStatus::Denied,
                ..
            } => "child_policy_blocked_403",
            Self::ChildPolicyGate {
                status: ChildPolicyStatus::Unavailable,
                ..
            } => "child_policy_unavailable",
            Self::InsufficientScopes {
                operation: "organization_tailnet_create",
            } => "provisioning_blocked_organization_tailnets",
            Self::InsufficientScopes {
                operation: "auth_keys",
            } => "provisioning_blocked_auth_keys_403",
            Self::InsufficientScopes {
                operation: "devices",
            } => "cleanup_blocked_devices_403",
            Self::InsufficientScopes { operation: "acl" } => "provisioning_blocked_acl_403",
            Self::InsufficientScopes { .. } => "provisioning_blocked_scopes_403",
            Self::Upstream { .. } => "provider_upstream_error",
            Self::Unavailable {
                operation: "startup" | "settings",
            } => "token_fetch_failed",
            Self::Unavailable { .. } => "provider_unavailable",
        }
    }
}

/// Lobby-network operations used by the HTTP layer.
#[async_trait]
pub trait NetworkProvider: Send + Sync {
    /// Returns the latest cached, non-mutating capability verdict.
    fn cached_capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::blocked()
    }

    /// Refreshes capability evidence. Implementations must not mutate upstream state.
    async fn refresh_capabilities(&self) -> ProviderCapabilities {
        self.cached_capabilities()
    }

    /// Returns a safe fail-closed error when process-local child credentials are unavailable.
    fn lobby_access_error(
        &self,
        _lobby_id: LobbyId,
        _mode: ProvisioningMode,
        _network_lifecycle: NetworkLifecycle,
        _dry_run: bool,
    ) -> Option<ProviderError> {
        None
    }

    /// Validates exact generation-bound child custody without provider I/O.
    fn validate_child_custody(
        &self,
        _lobby_id: LobbyId,
        _network_generation: u64,
        _identity: &ProviderNetworkIdentity,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    /// Read-only startup comparison of expected exact identities and provider-owned
    /// `spurfire-*` children. False means missing/mismatched state or an orphan.
    async fn reconcile_upstream_identities(
        &self,
        _expected: Vec<ProviderNetworkIdentity>,
    ) -> Result<bool, ProviderError> {
        Ok(true)
    }

    /// Prepares non-secret backing metadata. Shared mode performs no mutating call here.
    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError>;

    /// Mints one exactly-one-use, ephemeral, preauthorized credential.
    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError>;

    /// Revokes unconsumed credentials before attempting lobby-scoped devices.
    async fn cleanup_lobby(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError>;

    /// Performs one bounded, scoped device-list read. HTTP GET handlers never
    /// call this method; background/operator refreshers populate a cache.
    async fn observe_network(
        &self,
        _request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError> {
        Err(ProviderError::Unavailable {
            operation: "device_inventory",
        })
    }

    /// Erases the exact generation-bound child secret only after absence proof.
    async fn erase_child_secret(
        &self,
        _request: TailnetPresenceRequest,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unavailable {
            operation: "child_secret_erasure",
        })
    }

    /// Checks exact stable-ID presence through the parent organization listing.
    async fn tailnet_present(
        &self,
        _request: TailnetPresenceRequest,
    ) -> Result<bool, ProviderError> {
        Err(ProviderError::Unavailable {
            operation: "organization_tailnet_presence",
        })
    }
}

/// Credential-free transport implemented by authenticated broker IPC. Every
/// method uses existing typed provider DTOs; OAuth and vault material are not
/// representable on this boundary.
#[async_trait]
pub trait BrokerProviderTransport: Send + Sync {
    fn cached_capabilities(&self) -> ProviderCapabilities;
    async fn prepare(&self, request: PrepareLobbyRequest)
        -> Result<PreparedNetwork, ProviderError>;
    async fn mint(&self, request: MintCredentialRequest)
        -> Result<MintedCredential, ProviderError>;
    async fn cleanup(&self, request: CleanupLobbyRequest) -> Result<CleanupOutcome, ProviderError>;
    async fn observe(
        &self,
        request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError>;
    async fn present(&self, request: TailnetPresenceRequest) -> Result<bool, ProviderError>;
    async fn erase(&self, request: TailnetPresenceRequest) -> Result<(), ProviderError>;
}

/// Worker-side `NetworkProvider` client. It is permanently bound to one lobby
/// generation and contains no constructor accepting credentials, environment
/// names, vault paths, tailnet selectors, or provider clients.
pub struct BrokerProvider {
    transport: Arc<dyn BrokerProviderTransport>,
    lobby_id: LobbyId,
    generation: u64,
}

impl BrokerProvider {
    #[must_use]
    pub fn new(
        transport: Arc<dyn BrokerProviderTransport>,
        lobby_id: LobbyId,
        generation: u64,
    ) -> Self {
        Self {
            transport,
            lobby_id,
            generation,
        }
    }
    fn exact(&self, lobby_id: LobbyId, generation: u64) -> Result<(), ProviderError> {
        if lobby_id == self.lobby_id && generation == self.generation && generation != 0 {
            Ok(())
        } else {
            Err(ProviderError::IdentityMismatch)
        }
    }
}

impl fmt::Debug for BrokerProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrokerProvider")
            .field("lobby_id", &self.lobby_id)
            .field("generation", &self.generation)
            .field("transport", &"<authenticated-ipc>")
            .finish()
    }
}

#[async_trait]
impl NetworkProvider for BrokerProvider {
    fn cached_capabilities(&self) -> ProviderCapabilities {
        self.transport.cached_capabilities()
    }
    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        self.exact(request.lobby_id, request.network_generation)?;
        self.transport.prepare(request).await
    }
    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        self.exact(request.lobby_id, request.network_generation)?;
        self.transport.mint(request).await
    }
    async fn cleanup_lobby(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        self.exact(request.lobby_id, request.network_generation)?;
        self.transport.cleanup(request).await
    }
    async fn observe_network(
        &self,
        request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError> {
        self.exact(request.lobby_id, request.network_generation)?;
        self.transport.observe(request).await
    }
    async fn tailnet_present(
        &self,
        request: TailnetPresenceRequest,
    ) -> Result<bool, ProviderError> {
        self.exact(request.lobby_id, request.network_generation)?;
        self.transport.present(request).await
    }
    async fn erase_child_secret(
        &self,
        request: TailnetPresenceRequest,
    ) -> Result<(), ProviderError> {
        self.exact(request.lobby_id, request.network_generation)?;
        self.transport.erase(request).await
    }
}

/// Central provider facade enforcing the deployment-wide mutation gate.
///
/// Every provider mutation crosses this boundary. Read-only capability and
/// observation calls remain available for reconciliation evidence, while
/// simulated requests continue to work with the real gate disabled.
pub struct MutationGatedProvider {
    inner: Arc<dyn NetworkProvider>,
    authorization: MutationAuthorization,
}

#[derive(Clone, Copy, Debug)]
enum MutationAuthorization {
    DenyAll,
    DeploymentWide,
    ProtectedAlpha { lobby_id: LobbyId, generation: u64 },
}

impl MutationGatedProvider {
    /// Legacy deployment-wide wrapper retained for tests and non-binary embedders.
    /// The ordinary `spurfire-server` binary uses [`Self::deny_all`].
    #[must_use]
    pub fn new(inner: Arc<dyn NetworkProvider>, real_mutations_enabled: bool) -> Self {
        Self {
            inner,
            authorization: if real_mutations_enabled {
                MutationAuthorization::DeploymentWide
            } else {
                MutationAuthorization::DenyAll
            },
        }
    }

    /// Constructs the immutable gate used by the ordinary server binary.
    #[must_use]
    pub fn deny_all(inner: Arc<dyn NetworkProvider>) -> Self {
        Self {
            inner,
            authorization: MutationAuthorization::DenyAll,
        }
    }

    /// Exact one-lobby gate used only after protected receipt verification.
    #[must_use]
    pub fn protected_alpha(
        inner: Arc<dyn NetworkProvider>,
        lobby_id: LobbyId,
        generation: u64,
    ) -> Self {
        Self {
            inner,
            authorization: MutationAuthorization::ProtectedAlpha {
                lobby_id,
                generation,
            },
        }
    }

    fn require_exact(
        &self,
        dry_run: bool,
        lobby_id: LobbyId,
        network_generation: u64,
        _new_work: bool,
    ) -> Result<(), ProviderError> {
        if dry_run {
            return Ok(());
        }
        match self.authorization {
            MutationAuthorization::DeploymentWide => Ok(()),
            MutationAuthorization::ProtectedAlpha {
                lobby_id: exact,
                generation,
            } if lobby_id == exact && network_generation == generation => Ok(()),
            MutationAuthorization::ProtectedAlpha { .. } | MutationAuthorization::DenyAll => {
                Err(ProviderError::RealMutationsDisabled)
            }
        }
    }
}

impl fmt::Debug for MutationGatedProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MutationGatedProvider")
            .field("authorization", &self.authorization)
            .field("inner", &"<network-provider>")
            .finish()
    }
}

#[async_trait]
impl NetworkProvider for MutationGatedProvider {
    fn cached_capabilities(&self) -> ProviderCapabilities {
        self.inner.cached_capabilities()
    }

    async fn refresh_capabilities(&self) -> ProviderCapabilities {
        self.inner.refresh_capabilities().await
    }

    fn lobby_access_error(
        &self,
        lobby_id: LobbyId,
        mode: ProvisioningMode,
        network_lifecycle: NetworkLifecycle,
        dry_run: bool,
    ) -> Option<ProviderError> {
        self.inner
            .lobby_access_error(lobby_id, mode, network_lifecycle, dry_run)
    }

    fn validate_child_custody(
        &self,
        lobby_id: LobbyId,
        network_generation: u64,
        identity: &ProviderNetworkIdentity,
    ) -> Result<(), ProviderError> {
        self.inner
            .validate_child_custody(lobby_id, network_generation, identity)
    }

    async fn reconcile_upstream_identities(
        &self,
        expected: Vec<ProviderNetworkIdentity>,
    ) -> Result<bool, ProviderError> {
        self.inner.reconcile_upstream_identities(expected).await
    }

    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        self.require_exact(
            request.dry_run,
            request.lobby_id,
            request.network_generation,
            true,
        )?;
        self.inner.prepare_lobby(request).await
    }

    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        self.require_exact(
            request.dry_run,
            request.lobby_id,
            request.network_generation,
            true,
        )?;
        self.inner.mint_credential(request).await
    }

    async fn cleanup_lobby(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        self.require_exact(
            request.dry_run,
            request.lobby_id,
            request.network_generation,
            false,
        )?;
        self.inner.cleanup_lobby(request).await
    }

    async fn observe_network(
        &self,
        request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError> {
        self.inner.observe_network(request).await
    }

    async fn erase_child_secret(
        &self,
        request: TailnetPresenceRequest,
    ) -> Result<(), ProviderError> {
        self.require_exact(false, request.lobby_id, request.network_generation, false)?;
        self.inner.erase_child_secret(request).await
    }

    async fn tailnet_present(
        &self,
        request: TailnetPresenceRequest,
    ) -> Result<bool, ProviderError> {
        self.inner.tailnet_present(request).await
    }
}

/// Explicit simulation provider for tests and local development.
#[derive(Default)]
pub struct DryRunProvider {
    simulated_mints: AtomicU64,
    simulated_cleanups: AtomicU64,
}

impl DryRunProvider {
    /// Creates a provider with zero simulated calls.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            simulated_mints: AtomicU64::new(0),
            simulated_cleanups: AtomicU64::new(0),
        }
    }

    /// Number of simulated key-mint plans produced.
    #[must_use]
    pub fn mint_count(&self) -> u64 {
        self.simulated_mints.load(Ordering::SeqCst)
    }

    /// Number of simulated cleanup plans produced.
    #[must_use]
    pub fn cleanup_count(&self) -> u64 {
        self.simulated_cleanups.load(Ordering::SeqCst)
    }

    /// Dry-run never makes a mutating provider call.
    #[must_use]
    pub const fn mutating_call_count(&self) -> u64 {
        0
    }
}

impl fmt::Debug for DryRunProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DryRunProvider")
            .field("simulated_mints", &self.mint_count())
            .field("simulated_cleanups", &self.cleanup_count())
            .field("mutating_calls", &0)
            .finish()
    }
}

#[async_trait]
impl NetworkProvider for DryRunProvider {
    fn cached_capabilities(&self) -> ProviderCapabilities {
        // Dry-run proves simulation safety, not real Tailscale permissions.
        ProviderCapabilities::blocked()
    }

    async fn prepare_lobby(
        &self,
        _request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        Ok(dry_prepared_network())
    }

    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        let sequence = self.simulated_mints.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(dry_minted_credential(&request, sequence))
    }

    async fn cleanup_lobby(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        self.simulated_cleanups.fetch_add(1, Ordering::SeqCst);
        Ok(dry_cleanup_outcome(&request))
    }
}

struct ChildTailnetAccess {
    network_generation: u64,
    provider_tailnet_id: String,
    dns_name: TailnetDnsName,
    client: Arc<ChildTailscaleClient>,
}

/// Adapter around [`spurfire_control::TailscaleClient`]. Child OAuth material exists only in the
/// process-local `child_vault`; it is never returned to the service store.
pub struct TailscaleProvider {
    client: Arc<TailscaleClient>,
    shared_tailnet: String,
    simulated_mints: AtomicU64,
    capabilities: RwLock<ProviderCapabilities>,
    child_vault: RwLock<BTreeMap<LobbyId, ChildTailnetAccess>>,
    durable_child_vault: Option<Arc<EncryptedChildVault>>,
    child_create_lock: AsyncMutex<()>,
}

impl TailscaleProvider {
    /// Wraps an existing client, useful with a mock HTTP transport.
    #[must_use]
    pub fn new(client: TailscaleClient, shared_tailnet: impl Into<String>) -> Self {
        Self {
            client: Arc::new(client),
            shared_tailnet: shared_tailnet.into(),
            simulated_mints: AtomicU64::new(0),
            capabilities: RwLock::new(ProviderCapabilities::blocked()),
            child_vault: RwLock::new(BTreeMap::new()),
            durable_child_vault: None,
            child_create_lock: AsyncMutex::new(()),
        }
    }

    async fn child_access(
        &self,
        lobby_id: LobbyId,
        network_generation: u64,
        expected: &ProviderNetworkIdentity,
    ) -> Result<Arc<ChildTailscaleClient>, ProviderError> {
        expected.validate_for_mode(ProvisioningMode::TailnetPerLobby)?;
        let expected_id = expected
            .provider_tailnet_id
            .as_deref()
            .ok_or(ProviderError::IdentityMismatch)?;
        if let Ok(vault) = self.child_vault.read() {
            if let Some(access) = vault.get(&lobby_id) {
                if access.network_generation != network_generation
                    || access.provider_tailnet_id != expected_id
                    || access.dns_name != expected.tailnet_dns_name
                {
                    return Err(ProviderError::IdentityMismatch);
                }
                return Ok(Arc::clone(&access.client));
            }
        }
        let durable = self
            .durable_child_vault
            .as_ref()
            .ok_or(ProviderError::ChildSecretUnavailable)?;
        let identity = ChildVaultIdentity {
            lobby_id,
            network_generation,
            provider_tailnet_id: expected_id.to_owned(),
            tailnet_dns_name: expected.tailnet_dns_name.clone(),
        };
        let (credentials, _) = durable.get_exact(&identity).map_err(map_vault_error)?;
        let client = Arc::new(self.client.child_scoped(credentials));
        self.child_vault
            .write()
            .map_err(|_| ProviderError::Unavailable {
                operation: "child_secret_vault",
            })?
            .insert(
                lobby_id,
                ChildTailnetAccess {
                    network_generation,
                    provider_tailnet_id: expected_id.to_owned(),
                    dns_name: expected.tailnet_dns_name.clone(),
                    client: Arc::clone(&client),
                },
            );
        Ok(client)
    }

    #[cfg(test)]
    fn child_secret_count(&self) -> usize {
        self.child_vault.read().map_or(0, |vault| vault.len())
    }

    async fn gate_child_policy_or_cleanup(
        &self,
        lobby_id: LobbyId,
        identity: &ProviderNetworkIdentity,
        child: &ChildTailscaleClient,
        apply_first: bool,
    ) -> Result<ChildPolicyEvidence, ProviderError> {
        let policy = ChildTailnetPolicy::restrictive_riders(&dedicated_rider_tag(lobby_id))
            .map_err(|_| ProviderError::IdentityMismatch)?;
        let result = tokio::time::timeout(CHILD_POLICY_GATE_TIMEOUT, async {
            if apply_first {
                child
                    .apply_and_verify_policy(identity.tailnet_dns_name.as_str(), &policy)
                    .await
            } else {
                child
                    .verify_policy(identity.tailnet_dns_name.as_str(), &policy)
                    .await
            }
        })
        .await;
        match result {
            Ok(Ok(evidence)) => Ok(evidence),
            result => {
                let status = match result {
                    Ok(Err(ControlError::PolicyMismatch)) => ChildPolicyStatus::Mismatch,
                    Ok(Err(ControlError::Http { status: 403 })) => ChildPolicyStatus::Denied,
                    Ok(Err(_)) | Err(_) => ChildPolicyStatus::Unavailable,
                    Ok(Ok(_)) => unreachable!("successful policy evidence returned above"),
                };
                // The exact typed FQDN and child scope are already available. Best-effort delete
                // occurs before returning failure; encrypted custody is deliberately retained for
                // the normal exact-absence proof and CAS erasure path.
                let delete_acknowledged = child
                    .delete_tailnet(identity.tailnet_dns_name.as_str())
                    .await
                    .is_ok();
                Err(ProviderError::ChildPolicyGate {
                    identity: identity.clone(),
                    expected_digest: policy.semantic_digest(),
                    status,
                    delete_acknowledged,
                })
            }
        }
    }

    /// Builds the control adapter from `TS_API_BASE`, `TS_CLIENT_ID`, and
    /// `TS_CLIENT_SECRET`. The values remain inside `spurfire-control`.
    pub async fn from_env(shared_tailnet: impl Into<String>) -> Result<Self, ProviderError> {
        let client = TailscaleClient::from_env()
            .await
            .map_err(|error| map_control_error(error, "startup"))?;
        Ok(Self::new(client, shared_tailnet))
    }

    /// Broker-only constructor for SOPS-mounted credentials. Secret values are
    /// accepted as owned zeroizing strings and never enter argv or environment.
    pub fn from_mounted_credentials_with_vault(
        api_base: String,
        mut client_id: Zeroizing<String>,
        mut client_secret: Zeroizing<String>,
        shared_tailnet: impl Into<String>,
        vault: Arc<EncryptedChildVault>,
    ) -> Self {
        let client = TailscaleClient::new(
            api_base,
            std::mem::take(&mut *client_id),
            std::mem::take(&mut *client_secret),
        );
        client_id.zeroize();
        client_secret.zeroize();
        let mut provider = Self::new(client, shared_tailnet);
        provider.durable_child_vault = Some(vault);
        provider
    }

    /// Builds the production adapter with encrypted restart-recoverable child custody.
    pub async fn from_env_with_vault(
        shared_tailnet: impl Into<String>,
        vault: Arc<EncryptedChildVault>,
    ) -> Result<Self, ProviderError> {
        let client = TailscaleClient::from_env()
            .await
            .map_err(|error| map_control_error(error, "startup"))?;
        let mut provider = Self::new(client, shared_tailnet);
        provider.durable_child_vault = Some(vault);
        Ok(provider)
    }
}

impl fmt::Debug for TailscaleProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TailscaleProvider")
            .field("client", &"<redacted>")
            .field("shared_tailnet", &"<configured>")
            .field("child_secret_vault", &"<redacted>")
            .field("capabilities", &self.cached_capabilities())
            .finish()
    }
}

#[async_trait]
impl NetworkProvider for TailscaleProvider {
    fn cached_capabilities(&self) -> ProviderCapabilities {
        self.capabilities
            .read()
            .map_or_else(|_| ProviderCapabilities::blocked(), |cache| *cache)
    }

    fn lobby_access_error(
        &self,
        lobby_id: LobbyId,
        mode: ProvisioningMode,
        network_lifecycle: NetworkLifecycle,
        dry_run: bool,
    ) -> Option<ProviderError> {
        if dry_run
            || mode != ProvisioningMode::TailnetPerLobby
            || matches!(
                network_lifecycle,
                NetworkLifecycle::VerifyingAbsence
                    | NetworkLifecycle::DedicatedAbsent
                    | NetworkLifecycle::CreateRejected
            )
        {
            return None;
        }
        match self.child_vault.read() {
            Ok(vault) if vault.contains_key(&lobby_id) => None,
            Ok(_)
                if self
                    .durable_child_vault
                    .as_ref()
                    .is_some_and(|vault| vault.contains_lobby(lobby_id)) =>
            {
                None
            }
            Ok(_) => Some(ProviderError::ChildSecretUnavailable),
            Err(_) => Some(ProviderError::Unavailable {
                operation: "child_secret_vault",
            }),
        }
    }

    fn validate_child_custody(
        &self,
        lobby_id: LobbyId,
        network_generation: u64,
        identity: &ProviderNetworkIdentity,
    ) -> Result<(), ProviderError> {
        identity.validate_for_mode(ProvisioningMode::TailnetPerLobby)?;
        let expected_id = identity
            .provider_tailnet_id
            .as_deref()
            .ok_or(ProviderError::IdentityMismatch)?;
        if let Ok(vault) = self.child_vault.read() {
            if let Some(access) = vault.get(&lobby_id) {
                return if access.network_generation == network_generation
                    && access.provider_tailnet_id == expected_id
                    && access.dns_name == identity.tailnet_dns_name
                {
                    Ok(())
                } else {
                    Err(ProviderError::IdentityMismatch)
                };
            }
        }
        let durable = self
            .durable_child_vault
            .as_ref()
            .ok_or(ProviderError::ChildSecretUnavailable)?;
        let exact = ChildVaultIdentity {
            lobby_id,
            network_generation,
            provider_tailnet_id: expected_id.to_owned(),
            tailnet_dns_name: identity.tailnet_dns_name.clone(),
        };
        match durable.get_exact(&exact) {
            Ok(_) => Ok(()),
            Err(VaultError::Missing) if durable.has_erasure_receipt(&exact).unwrap_or(false) => {
                // A crash after exact CAS erasure but before the lobby commit
                // must retain cleanup-only recovery authority.
                Ok(())
            }
            Err(error) => Err(map_vault_error(error)),
        }
    }

    async fn reconcile_upstream_identities(
        &self,
        expected: Vec<ProviderNetworkIdentity>,
    ) -> Result<bool, ProviderError> {
        let upstream = self
            .client
            .list_organization_tailnets()
            .await
            .map_err(|error| map_control_error(error, "startup_reconciliation"))?;
        for identity in &expected {
            let stable_id = identity
                .provider_tailnet_id
                .as_deref()
                .ok_or(ProviderError::IdentityMismatch)?;
            let Some(found) = upstream.iter().find(|tailnet| tailnet.id == stable_id) else {
                return Ok(false);
            };
            if found
                .dns_name
                .as_ref()
                .map(spurfire_control::TailnetDnsName::as_str)
                != Some(identity.tailnet_dns_name.as_str())
            {
                return Err(ProviderError::IdentityMismatch);
            }
        }
        let expected_ids: std::collections::BTreeSet<&str> = expected
            .iter()
            .filter_map(|identity| identity.provider_tailnet_id.as_deref())
            .collect();
        Ok(!upstream.iter().any(|tailnet| {
            tailnet.display_name.starts_with("spurfire-")
                && !expected_ids.contains(tailnet.id.as_str())
        }))
    }

    async fn refresh_capabilities(&self) -> ProviderCapabilities {
        // Every startup probe is read-only. Organization-tailnet access is deliberately reported
        // separately from the shared-tailnet key/device/ACL scopes.
        let oauth_token_ok = self.client.probe_oauth_token().await.is_ok();
        let can_manage_organization_tailnets =
            oauth_token_ok && self.client.list_organization_tailnets().await.is_ok();
        let can_mint_auth_keys = oauth_token_ok
            && self
                .client
                .probe_auth_keys(&self.shared_tailnet)
                .await
                .is_ok();
        let can_list_devices =
            oauth_token_ok && self.client.list_devices(&self.shared_tailnet).await.is_ok();
        let can_manage_acl =
            oauth_token_ok && self.client.probe_acl(&self.shared_tailnet).await.is_ok();
        let refreshed = ProviderCapabilities {
            oauth_token_ok,
            can_manage_organization_tailnets,
            can_mint_auth_keys,
            can_list_devices,
            can_manage_acl,
        };
        if let Ok(mut cache) = self.capabilities.write() {
            *cache = refreshed;
        }
        refreshed
    }

    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        if request.dry_run || request.mode == ProvisioningMode::DryRun {
            return Ok(dry_prepared_network());
        }
        if request.mode == ProvisioningMode::SharedTailnet {
            let tailnet_dns_name = TailnetDnsName::parse(&self.shared_tailnet)
                .map_err(|_| ProviderError::IdentityMismatch)?;
            return Ok(PreparedNetwork {
                tailnet: tailnet_dns_name.as_str().to_owned(),
                identity: Some(ProviderNetworkIdentity {
                    provider_tailnet_id: None,
                    tailnet_dns_name,
                }),
                child_policy_evidence: None,
                dry_run: false,
                metadata: ResponseMetadata::default(),
            });
        }

        // Serialize the check-create-insert sequence. The HTTP store normally serializes lobby
        // creation, but the provider boundary must also be safe when called directly or by a
        // future multi-request adapter.
        let _create_guard = self.child_create_lock.lock().await;
        let cached_access = self.child_vault.read().ok().and_then(|vault| {
            vault.get(&request.lobby_id).map(|access| {
                (
                    access.network_generation,
                    access.provider_tailnet_id.clone(),
                    access.dns_name.clone(),
                    Arc::clone(&access.client),
                )
            })
        });
        if let Some((generation, provider_tailnet_id, dns_name, child)) = cached_access {
            if generation != request.network_generation {
                return Err(ProviderError::IdentityMismatch);
            }
            let identity = ProviderNetworkIdentity {
                provider_tailnet_id: Some(provider_tailnet_id),
                tailnet_dns_name: dns_name.clone(),
            };
            let evidence = self
                .gate_child_policy_or_cleanup(request.lobby_id, &identity, &child, false)
                .await?;
            return Ok(PreparedNetwork {
                tailnet: dns_name.as_str().to_owned(),
                identity: Some(identity),
                child_policy_evidence: Some(evidence),
                dry_run: false,
                metadata: ResponseMetadata::default(),
            });
        }

        if let Some(durable) = &self.durable_child_vault {
            if let Some(identity) = durable
                .identities()
                .map_err(map_vault_error)?
                .into_iter()
                .find(|identity| identity.lobby_id == request.lobby_id)
            {
                if identity.network_generation != request.network_generation {
                    return Err(ProviderError::IdentityMismatch);
                }
                let (credentials, _) = durable.get_exact(&identity).map_err(map_vault_error)?;
                let child = Arc::new(self.client.child_scoped(credentials));
                let provider_identity = ProviderNetworkIdentity {
                    provider_tailnet_id: Some(identity.provider_tailnet_id.clone()),
                    tailnet_dns_name: identity.tailnet_dns_name.clone(),
                };
                self.child_vault
                    .write()
                    .map_err(|_| ProviderError::Unavailable {
                        operation: "child_secret_vault",
                    })?
                    .insert(
                        request.lobby_id,
                        ChildTailnetAccess {
                            network_generation: identity.network_generation,
                            provider_tailnet_id: identity.provider_tailnet_id,
                            dns_name: identity.tailnet_dns_name.clone(),
                            client: Arc::clone(&child),
                        },
                    );
                let evidence = self
                    .gate_child_policy_or_cleanup(
                        request.lobby_id,
                        &provider_identity,
                        &child,
                        false,
                    )
                    .await?;
                return Ok(PreparedNetwork {
                    tailnet: identity.tailnet_dns_name.as_str().to_owned(),
                    identity: Some(provider_identity),
                    child_policy_evidence: Some(evidence),
                    dry_run: false,
                    metadata: ResponseMetadata::default(),
                });
            }
        }

        // The verified displayName grammar is ASCII and at most 50 bytes. A UUIDv4 plus this
        // prefix is 45 bytes and does not include user-controlled lobby text.
        let display_name = format!("spurfire-{}", request.lobby_id);
        let tailnet = self
            .client
            .create_tailnet(&display_name)
            .await
            .map_err(|error| map_control_error(error, "organization_tailnet_create"))?;
        let (provider_tailnet_id, provider_dns_name, _display_name, child_credentials) =
            tailnet.into_parts();
        let dns_name = TailnetDnsName::parse(provider_dns_name.as_str())
            .map_err(|_| ProviderError::IdentityMismatch)?;
        if !valid_provider_tailnet_id(&provider_tailnet_id) || request.network_generation == 0 {
            return Err(ProviderError::IdentityMismatch);
        }
        if let Some(vault) = &self.durable_child_vault {
            vault
                .put_if_absent(
                    ChildVaultIdentity {
                        lobby_id: request.lobby_id,
                        network_generation: request.network_generation,
                        provider_tailnet_id: provider_tailnet_id.clone(),
                        tailnet_dns_name: dns_name.clone(),
                    },
                    child_credentials.clone(),
                )
                .await
                .map_err(map_vault_error)?;
        }
        let child = Arc::new(self.client.child_scoped(child_credentials));
        self.child_vault
            .write()
            .map_err(|_| ProviderError::Unavailable {
                operation: "child_secret_vault",
            })?
            .insert(
                request.lobby_id,
                ChildTailnetAccess {
                    network_generation: request.network_generation,
                    provider_tailnet_id: provider_tailnet_id.clone(),
                    dns_name: dns_name.clone(),
                    client: Arc::clone(&child),
                },
            );
        let identity = ProviderNetworkIdentity {
            provider_tailnet_id: Some(provider_tailnet_id.clone()),
            tailnet_dns_name: dns_name.clone(),
        };
        let evidence = self
            .gate_child_policy_or_cleanup(request.lobby_id, &identity, &child, true)
            .await?;
        if let Ok(mut cache) = self.capabilities.write() {
            cache.oauth_token_ok = true;
            cache.can_manage_organization_tailnets = true;
        }
        Ok(PreparedNetwork {
            tailnet: dns_name.as_str().to_owned(),
            identity: Some(identity),
            child_policy_evidence: Some(evidence),
            dry_run: false,
            metadata: ResponseMetadata::default(),
        })
    }

    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        if request.dry_run || request.mode == ProvisioningMode::DryRun {
            let sequence = self.simulated_mints.fetch_add(1, Ordering::SeqCst) + 1;
            return Ok(dry_minted_credential(&request, sequence));
        }

        let options = AuthKeyOpts {
            ephemeral: true,
            preauthorized: true,
            reusable: false,
            tags: vec![request.tag.clone()],
            ttl_secs: 300,
        };
        let key = match request.mode {
            ProvisioningMode::SharedTailnet => {
                self.client
                    .create_auth_key(&request.tailnet, &options)
                    .await
            }
            ProvisioningMode::TailnetPerLobby => {
                let identity = request
                    .identity
                    .as_ref()
                    .ok_or(ProviderError::IdentityMismatch)?;
                if identity.tailnet_dns_name.as_str() != request.tailnet
                    || request.tag != dedicated_rider_tag(request.lobby_id)
                {
                    return Err(ProviderError::IdentityMismatch);
                }
                let child = self
                    .child_access(request.lobby_id, request.network_generation, identity)
                    .await?;
                // Re-read immediately before every mint. A cached earlier success cannot authorize
                // a key after out-of-band policy drift.
                self.gate_child_policy_or_cleanup(
                    request.lobby_id,
                    identity,
                    child.as_ref(),
                    false,
                )
                .await?;
                child.create_auth_key(&request.tailnet, &options).await
            }
            ProvisioningMode::DryRun => unreachable!("dry-run returned above"),
        };
        let key = match key {
            Ok(key) => key,
            Err(error) => {
                let error = map_control_error(error, "auth_keys");
                if request.mode == ProvisioningMode::SharedTailnet
                    && matches!(error, ProviderError::InsufficientScopes { .. })
                {
                    if let Ok(mut cache) = self.capabilities.write() {
                        cache.can_mint_auth_keys = false;
                    }
                }
                return Err(error);
            }
        };
        Ok(MintedCredential {
            credential_id: key.id,
            auth_key: SecretString::new(key.key),
            tailnet: request.tailnet,
            metadata: ResponseMetadata::default(),
        })
    }

    async fn cleanup_lobby(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        if request.dry_run || request.mode == ProvisioningMode::DryRun {
            return Ok(dry_cleanup_outcome(&request));
        }
        let identity = request
            .identity
            .as_ref()
            .ok_or(ProviderError::IdentityMismatch)?;
        identity.validate_for_mode(request.mode)?;
        if request.network_generation == 0 || identity.tailnet_dns_name.as_str() != request.tailnet
        {
            return Err(ProviderError::IdentityMismatch);
        }
        if request.mode == ProvisioningMode::TailnetPerLobby {
            return self.cleanup_child_tailnet(request).await;
        }
        self.cleanup_shared_tailnet(request).await
    }

    async fn observe_network(
        &self,
        request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError> {
        if request.dry_run || request.mode == ProvisioningMode::DryRun {
            return Err(ProviderError::IdentityMismatch);
        }
        let count = match request.mode {
            ProvisioningMode::TailnetPerLobby => {
                let identity = request
                    .identity
                    .as_ref()
                    .ok_or(ProviderError::IdentityMismatch)?;
                if identity.tailnet_dns_name.as_str() != request.tailnet {
                    return Err(ProviderError::IdentityMismatch);
                }
                let child = self
                    .child_access(request.lobby_id, request.network_generation, identity)
                    .await?;
                child
                    .list_devices(identity.tailnet_dns_name.as_str())
                    .await
                    .map_err(|error| map_control_error(error, "device_inventory"))?
                    .len()
            }
            ProvisioningMode::SharedTailnet => self
                .client
                .list_devices(&request.tailnet)
                .await
                .map_err(|error| map_control_error(error, "device_inventory"))?
                .into_iter()
                .filter(|device| device.tags.iter().any(|tag| tag == &request.tag))
                .count(),
            ProvisioningMode::DryRun => unreachable!("dry run returned above"),
        };
        Ok(ProviderDeviceObservation {
            enrolled_device_count: u32::try_from(count).unwrap_or(u32::MAX),
        })
    }

    async fn erase_child_secret(
        &self,
        request: TailnetPresenceRequest,
    ) -> Result<(), ProviderError> {
        request
            .identity
            .validate_for_mode(ProvisioningMode::TailnetPerLobby)?;
        let durable_identity = ChildVaultIdentity {
            lobby_id: request.lobby_id,
            network_generation: request.network_generation,
            provider_tailnet_id: request
                .identity
                .provider_tailnet_id
                .clone()
                .ok_or(ProviderError::IdentityMismatch)?,
            tailnet_dns_name: request.identity.tailnet_dns_name.clone(),
        };
        // Validate every in-memory custody layer before the irreversible durable
        // CAS deletion. A durable tombstone (record missing for this exact tuple)
        // is restart-idempotent proof that a prior erase completed before the
        // non-secret cleanup state commit.
        {
            let vault = self
                .child_vault
                .read()
                .map_err(|_| ProviderError::Unavailable {
                    operation: "child_secret_vault",
                })?;
            if let Some(access) = vault.get(&request.lobby_id) {
                if request.network_generation == 0
                    || access.network_generation != request.network_generation
                    || access.provider_tailnet_id != durable_identity.provider_tailnet_id
                    || access.dns_name != durable_identity.tailnet_dns_name
                {
                    return Err(ProviderError::IdentityMismatch);
                }
            } else if self.durable_child_vault.is_none() {
                return Err(ProviderError::ChildSecretUnavailable);
            }
        }
        if let Some(durable) = &self.durable_child_vault {
            match durable.get_exact(&durable_identity) {
                Ok((credentials, version)) => {
                    drop(credentials);
                    durable
                        .delete_cas(&durable_identity, version)
                        .await
                        .map_err(map_vault_error)?;
                    let verified_version = durable
                        .verify_erased(&durable_identity)
                        .await
                        .map_err(map_vault_error)?;
                    if verified_version != version {
                        return Err(ProviderError::IdentityMismatch);
                    }
                }
                Err(VaultError::Missing) => {
                    // Missing ciphertext alone is not proof that custody ever
                    // existed. Recovery requires the durable exact CAS receipt.
                    durable
                        .verify_erased(&durable_identity)
                        .await
                        .map_err(map_vault_error)?;
                }
                Err(error) => return Err(map_vault_error(error)),
            }
        }
        self.child_vault
            .write()
            .map_err(|_| ProviderError::Unavailable {
                operation: "child_secret_vault",
            })?
            .remove(&request.lobby_id);
        Ok(())
    }

    async fn tailnet_present(
        &self,
        request: TailnetPresenceRequest,
    ) -> Result<bool, ProviderError> {
        request
            .identity
            .validate_for_mode(ProvisioningMode::TailnetPerLobby)?;
        if request.network_generation == 0 {
            return Err(ProviderError::IdentityMismatch);
        }
        let stable_id = request
            .identity
            .provider_tailnet_id
            .as_deref()
            .ok_or(ProviderError::IdentityMismatch)?;
        let tailnets = self
            .client
            .list_organization_tailnets()
            .await
            .map_err(|error| map_control_error(error, "organization_tailnet_presence"))?;
        let Some(found) = tailnets.into_iter().find(|tailnet| tailnet.id == stable_id) else {
            return Ok(false);
        };
        if found
            .dns_name
            .as_ref()
            .is_some_and(|dns_name| dns_name.as_str() != request.identity.tailnet_dns_name.as_str())
        {
            return Err(ProviderError::IdentityMismatch);
        }
        Ok(true)
    }
}

impl TailscaleProvider {
    async fn cleanup_child_tailnet(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        let identity = request
            .identity
            .as_ref()
            .ok_or(ProviderError::IdentityMismatch)?;
        if identity.tailnet_dns_name.as_str() != request.tailnet {
            return Err(ProviderError::IdentityMismatch);
        }
        let child = self
            .child_access(request.lobby_id, request.network_generation, identity)
            .await?;
        if request.include_devices {
            child
                .delete_tailnet(identity.tailnet_dns_name.as_str())
                .await
                .map_err(|error| map_control_error(error, "child_tailnet_delete"))?;
            return Ok(CleanupOutcome {
                cleanup_pending: true,
                delete_acknowledged: true,
                child_secret_erased: false,
                revoked_credential_ids: request
                    .credentials
                    .into_iter()
                    .map(|credential| credential.credential_id)
                    .collect(),
                attempted_device_deletes: 0,
                metadata: ResponseMetadata::default(),
            });
        }

        let mut outcome = CleanupOutcome::default();
        for credential in &request.credentials {
            if credential.expires_at <= request.now {
                outcome
                    .revoked_credential_ids
                    .push(credential.credential_id.clone());
                continue;
            }
            match child
                .delete_auth_key(&request.tailnet, &credential.credential_id)
                .await
            {
                Ok(())
                | Err(ControlError::Http {
                    status: 400 | 404, ..
                }) => outcome
                    .revoked_credential_ids
                    .push(credential.credential_id.clone()),
                Err(_) => outcome.cleanup_pending = true,
            }
        }
        Ok(outcome)
    }

    async fn cleanup_shared_tailnet(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        let mut outcome = CleanupOutcome::default();
        for credential in &request.credentials {
            if credential.expires_at <= request.now {
                outcome
                    .revoked_credential_ids
                    .push(credential.credential_id.clone());
                continue;
            }
            match self
                .client
                .delete_auth_key(&request.tailnet, &credential.credential_id)
                .await
            {
                Ok(())
                | Err(ControlError::Http {
                    status: 400 | 404, ..
                }) => outcome
                    .revoked_credential_ids
                    .push(credential.credential_id.clone()),
                Err(ControlError::Http { status: 403, .. }) => {
                    outcome.cleanup_pending = true;
                    if let Ok(mut cache) = self.capabilities.write() {
                        cache.can_mint_auth_keys = false;
                    }
                }
                Err(_) => outcome.cleanup_pending = true,
            }
        }

        if !request.include_devices {
            return Ok(outcome);
        }
        let devices = match self.client.list_devices(&request.tailnet).await {
            Ok(devices) => devices,
            Err(error) => {
                outcome.cleanup_pending = true;
                if matches!(error, ControlError::Http { status: 403, .. }) {
                    if let Ok(mut cache) = self.capabilities.write() {
                        cache.can_list_devices = false;
                    }
                }
                return Ok(outcome);
            }
        };
        for device in devices
            .into_iter()
            .filter(|device| device.tags.iter().any(|tag| tag == &request.tag))
        {
            outcome.attempted_device_deletes += 1;
            if let Err(error) = self.client.delete_device(&device.id).await {
                // A later sweep lists the lobby tag again. No device identifier is retained.
                outcome.cleanup_pending = true;
                if matches!(error, ControlError::Http { status: 403, .. }) {
                    if let Ok(mut cache) = self.capabilities.write() {
                        cache.can_list_devices = false;
                    }
                }
            }
        }
        Ok(outcome)
    }
}

fn dry_prepared_network() -> PreparedNetwork {
    PreparedNetwork {
        tailnet: "dry-run.invalid".to_owned(),
        identity: None,
        child_policy_evidence: None,
        dry_run: true,
        metadata: ResponseMetadata {
            dry_run: true,
            planned_actions: Vec::new(),
        },
    }
}

fn dry_minted_credential(request: &MintCredentialRequest, sequence: u64) -> MintedCredential {
    MintedCredential {
        credential_id: format!("dry-{}-{}-{sequence}", request.lobby_id, request.player_id),
        auth_key: SecretString::new(DRY_RUN_AUTH_KEY),
        tailnet: "dry-run.invalid".to_owned(),
        metadata: ResponseMetadata {
            dry_run: true,
            planned_actions: vec![PlannedAction {
                method: "POST".to_owned(),
                path: "/tailnet/-/keys".to_owned(),
                description: "mint one-use ephemeral tagged join credential".to_owned(),
            }],
        },
    }
}

fn dry_cleanup_outcome(request: &CleanupLobbyRequest) -> CleanupOutcome {
    let mut planned_actions = Vec::new();
    if !request.credentials.is_empty() {
        planned_actions.push(PlannedAction {
            method: "DELETE".to_owned(),
            path: "/tailnet/-/keys/{credential_id}".to_owned(),
            description: "revoke each unconsumed lobby credential".to_owned(),
        });
    }
    if request.include_devices {
        planned_actions.extend([
            PlannedAction {
                method: "GET".to_owned(),
                path: "/tailnet/-/devices".to_owned(),
                description: "discover ephemeral devices carrying the lobby tag".to_owned(),
            },
            PlannedAction {
                method: "DELETE".to_owned(),
                path: "/device/{device_id}".to_owned(),
                description: "delete each discovered ephemeral lobby device".to_owned(),
            },
        ]);
    }
    CleanupOutcome {
        cleanup_pending: false,
        delete_acknowledged: false,
        child_secret_erased: false,
        revoked_credential_ids: request
            .credentials
            .iter()
            .map(|credential| credential.credential_id.clone())
            .collect(),
        attempted_device_deletes: 0,
        metadata: ResponseMetadata {
            dry_run: true,
            planned_actions,
        },
    }
}

fn map_vault_error(error: VaultError) -> ProviderError {
    match error {
        VaultError::Conflict | VaultError::Invalid => ProviderError::IdentityMismatch,
        VaultError::Missing => ProviderError::ChildSecretUnavailable,
        VaultError::Io | VaultError::Crypto => ProviderError::Unavailable {
            operation: "child_secret_vault",
        },
    }
}

/// The one generated rider tag shared by policy and tagged auth-key issuance.
#[must_use]
pub fn dedicated_rider_tag(lobby_id: LobbyId) -> String {
    format!("tag:spurfire-lobby-{lobby_id}")
}

pub(crate) fn dedicated_policy_digest(lobby_id: LobbyId) -> Result<String, ProviderError> {
    ChildTailnetPolicy::restrictive_riders(&dedicated_rider_tag(lobby_id))
        .map(|policy| policy.semantic_digest())
        .map_err(|_| ProviderError::IdentityMismatch)
}

fn valid_provider_tailnet_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn map_control_error(error: ControlError, operation: &'static str) -> ProviderError {
    match error {
        ControlError::Http { status: 403, .. } => ProviderError::InsufficientScopes { operation },
        ControlError::Http { status, .. } => ProviderError::Upstream { operation, status },
        ControlError::ProvisioningUnavailable(_) => ProviderError::Unavailable { operation },
        ControlError::InvalidTailnetName(_)
        | ControlError::InvalidProviderPath
        | ControlError::InvalidPolicy
        | ControlError::PolicyMismatch => ProviderError::IdentityMismatch,
        ControlError::Env(_)
        | ControlError::Reqwest(_)
        | ControlError::Json(_)
        | ControlError::IncompletePagination => ProviderError::Unavailable { operation },
    }
}

#[cfg(test)]
mod tests {
    use mockito::{Matcher, Mock, Server};

    use super::*;

    struct FakeBrokerTransport {
        prepares: AtomicU64,
    }

    #[async_trait]
    impl BrokerProviderTransport for FakeBrokerTransport {
        fn cached_capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::available()
        }
        async fn prepare(
            &self,
            _request: PrepareLobbyRequest,
        ) -> Result<PreparedNetwork, ProviderError> {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            Ok(dry_prepared_network())
        }
        async fn mint(
            &self,
            _request: MintCredentialRequest,
        ) -> Result<MintedCredential, ProviderError> {
            Err(ProviderError::Unavailable {
                operation: "fake_mint",
            })
        }
        async fn cleanup(
            &self,
            _request: CleanupLobbyRequest,
        ) -> Result<CleanupOutcome, ProviderError> {
            Err(ProviderError::Unavailable {
                operation: "fake_cleanup",
            })
        }
        async fn observe(
            &self,
            _request: ObserveNetworkRequest,
        ) -> Result<ProviderDeviceObservation, ProviderError> {
            Err(ProviderError::Unavailable {
                operation: "fake_observe",
            })
        }
        async fn present(&self, _request: TailnetPresenceRequest) -> Result<bool, ProviderError> {
            Err(ProviderError::Unavailable {
                operation: "fake_present",
            })
        }
        async fn erase(&self, _request: TailnetPresenceRequest) -> Result<(), ProviderError> {
            Err(ProviderError::Unavailable {
                operation: "fake_erase",
            })
        }
    }

    #[tokio::test]
    async fn broker_provider_is_credential_free_and_exact_fence_bound() {
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-0000000000bc").unwrap();
        let transport = Arc::new(FakeBrokerTransport {
            prepares: AtomicU64::new(0),
        });
        let provider = BrokerProvider::new(transport.clone(), lobby_id, 9);
        assert!(provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 8,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .is_err());
        assert_eq!(transport.prepares.load(Ordering::SeqCst), 0);
        assert!(provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 9,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .is_ok());
        assert_eq!(transport.prepares.load(Ordering::SeqCst), 1);
        let debug = format!("{provider:?}");
        assert!(!debug.contains("TS_CLIENT"));
        assert!(!debug.contains("secret"));
    }

    fn policy_readback(lobby_id: LobbyId) -> String {
        let tag = dedicated_rider_tag(lobby_id);
        serde_json::json!({
            "tagOwners": {(tag.clone()): []},
            "grants": [{"src":[tag.clone()], "dst":[tag], "ip":["udp:41643"]}]
        })
        .to_string()
    }

    async fn policy_fault_provider(
        server: &mut Server,
    ) -> (TailscaleProvider, LobbyId, Mock, Mock, Mock) {
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let organization_token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::UrlEncoded("client_id".into(), "policy-org".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"organization-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let create = server
            .mock("POST", "/organizations/-/tailnets")
            .match_header("authorization", "Bearer organization-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id":"TtPolicyFaultCNTRL",
                    "dnsName":"tail-policy-fault.ts.net",
                    "displayName":format!("spurfire-{lobby_id}"),
                    "oauthClient":{"id":"policy-child","secret":"policy-child-secret"}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let child_token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::UrlEncoded(
                "client_id".into(),
                "policy-child".into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"child-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        (
            TailscaleProvider::new(
                TailscaleClient::new(server.url(), "policy-org", "policy-org-secret"),
                "-",
            ),
            lobby_id,
            organization_token,
            create,
            child_token,
        )
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString::new("synthetic-auth-key-canary-secret");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert!(!format!("{secret:?}").contains("canary"));
    }

    #[tokio::test]
    async fn central_gate_blocks_every_real_mutation_and_allows_simulation() {
        let inner = Arc::new(DryRunProvider::new());
        let provider = MutationGatedProvider::new(inner.clone(), false);
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let player_id = PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap();

        assert_eq!(
            provider
                .prepare_lobby(PrepareLobbyRequest {
                    lobby_id,
                    network_generation: 1,
                    mode: ProvisioningMode::TailnetPerLobby,
                    dry_run: false,
                })
                .await
                .unwrap_err(),
            ProviderError::RealMutationsDisabled
        );
        assert_eq!(
            provider
                .mint_credential(MintCredentialRequest {
                    lobby_id,
                    network_generation: 1,
                    identity: None,
                    mode: ProvisioningMode::SharedTailnet,
                    player_id,
                    tailnet: "-".to_owned(),
                    tag: "tag:spurfire-lobby-test".to_owned(),
                    expires_at: UnixMillis::new(300_000),
                    dry_run: false,
                })
                .await
                .unwrap_err(),
            ProviderError::RealMutationsDisabled
        );
        assert_eq!(
            provider
                .cleanup_lobby(CleanupLobbyRequest {
                    lobby_id,
                    network_generation: 1,
                    identity: None,
                    mode: ProvisioningMode::SharedTailnet,
                    tailnet: "-".to_owned(),
                    tag: "tag:spurfire-lobby-test".to_owned(),
                    credentials: Vec::new(),
                    include_devices: true,
                    now: UnixMillis::new(0),
                    dry_run: false,
                })
                .await
                .unwrap_err(),
            ProviderError::RealMutationsDisabled
        );
        assert_eq!(inner.mint_count(), 0);
        assert_eq!(inner.cleanup_count(), 0);

        provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 1,
                mode: ProvisioningMode::DryRun,
                dry_run: true,
            })
            .await
            .unwrap();
        provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                network_generation: 1,
                identity: None,
                mode: ProvisioningMode::DryRun,
                player_id,
                tailnet: "-".to_owned(),
                tag: "tag:spurfire-lobby-test".to_owned(),
                expires_at: UnixMillis::new(300_000),
                dry_run: true,
            })
            .await
            .unwrap();
        assert_eq!(inner.mint_count(), 1);
        assert_eq!(inner.mutating_call_count(), 0);
    }

    #[test]
    fn blocked_capabilities_are_fail_closed() {
        let capabilities = ProviderCapabilities::blocked();
        assert!(!capabilities.shared_tailnet_available());
        assert_eq!(
            capabilities.blocked_state_reason(ProvisioningMode::TailnetPerLobby),
            "token_fetch_failed"
        );
        assert_eq!(
            capabilities
                .response(ResponseMetadata::default())
                .modes
                .shared_tailnet,
            CapabilityModeStatus::BlockedScopes
        );
    }

    #[tokio::test]
    async fn organization_capability_is_independent_of_shared_scopes() {
        let mut server = Server::new_async().await;
        let token = server
            .mock("POST", "/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"token-canary","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let organization = server
            .mock("GET", "/organizations/-/tailnets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tailnets":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let keys = server
            .mock("GET", "/tailnet/-/keys")
            .with_status(403)
            .expect(1)
            .create_async()
            .await;
        let devices = server
            .mock("GET", "/tailnet/-/devices")
            .with_status(403)
            .expect(1)
            .create_async()
            .await;
        let acl = server
            .mock("GET", "/tailnet/-/acl")
            .with_status(403)
            .expect(1)
            .create_async()
            .await;
        let provider = TailscaleProvider::new(
            TailscaleClient::new(server.url(), "client-canary", "secret-canary"),
            "-",
        );

        let capabilities = provider.refresh_capabilities().await;
        let response = capabilities.response(ResponseMetadata::default());

        assert!(capabilities.tailnet_per_lobby_available());
        assert!(!capabilities.shared_tailnet_available());
        assert_eq!(
            response.modes.tailnet_per_lobby,
            CapabilityModeStatus::Available
        );
        assert_eq!(
            response.modes.shared_tailnet,
            CapabilityModeStatus::BlockedScopes
        );
        token.assert_async().await;
        organization.assert_async().await;
        keys.assert_async().await;
        devices.assert_async().await;
        acl.assert_async().await;
    }

    #[tokio::test]
    async fn tailscale_adapter_dry_run_never_contacts_http_transport() {
        let mut server = Server::new_async().await;
        let no_posts = server
            .mock("POST", Matcher::Regex(".*".to_owned()))
            .expect(0)
            .create_async()
            .await;
        let no_deletes = server
            .mock("DELETE", Matcher::Regex(".*".to_owned()))
            .expect(0)
            .create_async()
            .await;
        let no_gets = server
            .mock("GET", Matcher::Regex(".*".to_owned()))
            .expect(0)
            .create_async()
            .await;
        let client = TailscaleClient::new(
            server.url(),
            "oauth-client-canary",
            "oauth-secret-canary-value",
        );
        let provider = TailscaleProvider::new(client, "-");
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let player_id = PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap();

        provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 1,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: true,
            })
            .await
            .unwrap();
        let minted = provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                network_generation: 1,
                identity: None,
                mode: ProvisioningMode::DryRun,
                player_id,
                tailnet: "-".to_owned(),
                tag: "tag:spurfire-lobby-test".to_owned(),
                expires_at: UnixMillis::new(300_000),
                dry_run: true,
            })
            .await
            .unwrap();
        assert_eq!(minted.auth_key.into_zeroizing().as_str(), DRY_RUN_AUTH_KEY);
        provider
            .cleanup_lobby(CleanupLobbyRequest {
                lobby_id,
                network_generation: 1,
                identity: None,
                mode: ProvisioningMode::DryRun,
                tailnet: "-".to_owned(),
                tag: "tag:spurfire-lobby-test".to_owned(),
                credentials: vec![CredentialCleanup {
                    credential_id: "fake-receipt".to_owned(),
                    expires_at: UnixMillis::new(300_000),
                }],
                include_devices: true,
                now: UnixMillis::new(0),
                dry_run: true,
            })
            .await
            .unwrap();

        assert_eq!(provider.child_secret_count(), 0);
        no_posts.assert_async().await;
        no_deletes.assert_async().await;
        no_gets.assert_async().await;
    }

    #[tokio::test]
    async fn concurrent_tailnet_prepare_is_idempotent_then_child_scope_deletes_and_evicts() {
        const CHILD_ID: &str = "child-client-provider-canary";
        const CHILD_SECRET: &str = "child-secret-provider-canary";
        let mut server = Server::new_async().await;
        let organization_token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("client_id".into(), "org-client".into()),
                Matcher::UrlEncoded("client_secret".into(), "org-secret".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"organization-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let create = server
            .mock("POST", "/organizations/-/tailnets")
            .match_header("authorization", "Bearer organization-token")
            .match_body(Matcher::Json(serde_json::json!({
                "displayName":"spurfire-00000000-0000-4000-8000-000000000001"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id":"TtProviderCNTRL",
                    "dnsName":"tail-provider.ts.net",
                    "displayName":"spurfire-00000000-0000-4000-8000-000000000001",
                    "oauthClient":{"id":CHILD_ID,"secret":CHILD_SECRET}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let child_token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("client_id".into(), CHILD_ID.into()),
                Matcher::UrlEncoded("client_secret".into(), CHILD_SECRET.into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"child-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let policy_write = server
            .mock("POST", "/tailnet/tail-provider.ts.net/acl")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let policy_read = server
            .mock("GET", "/tailnet/tail-provider.ts.net/acl")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(policy_readback(lobby_id))
            .expect(3)
            .create_async()
            .await;
        let key = server
            .mock("POST", "/tailnet/tail-provider.ts.net/keys")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"key-receipt","key":"synthetic-auth-key-join-secret"}"#)
            .expect(1)
            .create_async()
            .await;
        let delete = server
            .mock("DELETE", "/tailnet/tail-provider.ts.net")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let provider = Arc::new(TailscaleProvider::new(
            TailscaleClient::new(server.url(), "org-client", "org-secret"),
            "-",
        ));
        let player_id = PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap();
        let prepare_request = PrepareLobbyRequest {
            lobby_id,
            network_generation: 1,
            mode: ProvisioningMode::TailnetPerLobby,
            dry_run: false,
        };

        let (first, second) = tokio::join!(
            provider.prepare_lobby(prepare_request),
            provider.prepare_lobby(prepare_request)
        );
        let prepared = first.unwrap();
        assert_eq!(second.unwrap().tailnet, prepared.tailnet);
        assert_eq!(prepared.tailnet, "tail-provider.ts.net");
        assert_eq!(provider.child_secret_count(), 1);
        let provider_debug = format!("{provider:?}");
        assert!(!provider_debug.contains(CHILD_ID));
        assert!(!provider_debug.contains(CHILD_SECRET));

        let identity = prepared.identity.clone();
        let minted = provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                network_generation: 1,
                identity: identity.clone(),
                mode: ProvisioningMode::TailnetPerLobby,
                player_id,
                tailnet: prepared.tailnet.clone(),
                tag: dedicated_rider_tag(lobby_id),
                expires_at: UnixMillis::new(300_000),
                dry_run: false,
            })
            .await
            .unwrap();
        assert_eq!(minted.credential_id, "key-receipt");
        assert_eq!(
            minted.auth_key.into_zeroizing().as_str(),
            "synthetic-auth-key-join-secret"
        );

        let outcome = provider
            .cleanup_lobby(CleanupLobbyRequest {
                lobby_id,
                network_generation: 1,
                identity: identity.clone(),
                mode: ProvisioningMode::TailnetPerLobby,
                tailnet: prepared.tailnet,
                tag: "tag:spurfire-lobby-test".to_owned(),
                credentials: vec![CredentialCleanup {
                    credential_id: "key-receipt".to_owned(),
                    expires_at: UnixMillis::new(300_000),
                }],
                include_devices: true,
                now: UnixMillis::new(0),
                dry_run: false,
            })
            .await
            .unwrap();
        assert!(outcome.cleanup_pending);
        assert!(outcome.delete_acknowledged);
        assert!(!outcome.child_secret_erased);
        assert_eq!(outcome.revoked_credential_ids, ["key-receipt"]);
        assert_eq!(provider.child_secret_count(), 1);
        provider
            .erase_child_secret(TailnetPresenceRequest {
                lobby_id,
                network_generation: 1,
                identity: identity.unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(provider.child_secret_count(), 0);

        organization_token.assert_async().await;
        create.assert_async().await;
        child_token.assert_async().await;
        policy_write.assert_async().await;
        policy_read.assert_async().await;
        key.assert_async().await;
        delete.assert_async().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn policy_mismatch_deletes_before_mint_and_retains_encrypted_custody() {
        let root = std::env::temp_dir().join(format!(
            "spurfire-policy-mismatch-vault-{}",
            std::process::id()
        ));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let key_path = root.join("key");
        tokio::fs::write(&key_path, [17_u8; 32]).await.unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .await
                .unwrap();
            tokio::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        let durable = Arc::new(
            EncryptedChildVault::open(root.join("vault.json"), &key_path)
                .await
                .unwrap(),
        );
        let mut server = Server::new_async().await;
        let (mut provider, lobby_id, organization_token, create, child_token) =
            policy_fault_provider(&mut server).await;
        provider.durable_child_vault = Some(Arc::clone(&durable));
        let tag = dedicated_rider_tag(lobby_id);
        let policy_write = server
            .mock("POST", "/tailnet/tail-policy-fault.ts.net/acl")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let policy_read = server
            .mock("GET", "/tailnet/tail-policy-fault.ts.net/acl")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "tagOwners":{(tag.clone()):[]},
                    "grants":[{"src":[tag.clone()],"dst":[tag],"ip":["tcp:41643"]}]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let delete = server
            .mock("DELETE", "/tailnet/tail-policy-fault.ts.net")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let no_key = server
            .mock("POST", "/tailnet/tail-policy-fault.ts.net/keys")
            .expect(0)
            .create_async()
            .await;

        let error = provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 1,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .unwrap_err();

        match error {
            ProviderError::ChildPolicyGate {
                expected_digest,
                status,
                delete_acknowledged,
                ..
            } => {
                assert_eq!(expected_digest.len(), 64);
                assert_eq!(status, ChildPolicyStatus::Mismatch);
                assert!(delete_acknowledged);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(provider.child_secret_count(), 1);
        assert!(durable.contains_lobby(lobby_id));
        organization_token.assert_async().await;
        create.assert_async().await;
        child_token.assert_async().await;
        policy_write.assert_async().await;
        policy_read.assert_async().await;
        delete.assert_async().await;
        no_key.assert_async().await;
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn policy_403_deletes_before_mint_and_exposes_only_safe_status() {
        let mut server = Server::new_async().await;
        let (provider, lobby_id, organization_token, create, child_token) =
            policy_fault_provider(&mut server).await;
        let denied = server
            .mock("POST", "/tailnet/tail-policy-fault.ts.net/acl")
            .with_status(403)
            .with_body(r#"{"secret":"policy-provider-body-canary"}"#)
            .expect(1)
            .create_async()
            .await;
        let delete = server
            .mock("DELETE", "/tailnet/tail-policy-fault.ts.net")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let no_read = server
            .mock("GET", "/tailnet/tail-policy-fault.ts.net/acl")
            .expect(0)
            .create_async()
            .await;
        let no_key = server
            .mock("POST", "/tailnet/tail-policy-fault.ts.net/keys")
            .expect(0)
            .create_async()
            .await;

        let error = provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 1,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .unwrap_err();
        let diagnostic = format!("{error:?}");
        assert!(!diagnostic.contains("policy-provider-body-canary"));
        assert!(matches!(
            error,
            ProviderError::ChildPolicyGate {
                status: ChildPolicyStatus::Denied,
                delete_acknowledged: true,
                ..
            }
        ));
        assert_eq!(provider.child_secret_count(), 1);
        organization_token.assert_async().await;
        create.assert_async().await;
        child_token.assert_async().await;
        denied.assert_async().await;
        delete.assert_async().await;
        no_read.assert_async().await;
        no_key.assert_async().await;
    }

    #[tokio::test]
    async fn policy_timeout_is_bounded_then_deletes_before_mint() {
        let mut server = Server::new_async().await;
        let (provider, lobby_id, organization_token, create, child_token) =
            policy_fault_provider(&mut server).await;
        let policy_write = server
            .mock("POST", "/tailnet/tail-policy-fault.ts.net/acl")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let readback = policy_readback(lobby_id);
        let delayed_read = server
            .mock("GET", "/tailnet/tail-policy-fault.ts.net/acl")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_chunked_body(move |writer| {
                std::thread::sleep(Duration::from_millis(250));
                writer.write_all(readback.as_bytes())
            })
            .expect(1)
            .create_async()
            .await;
        let delete = server
            .mock("DELETE", "/tailnet/tail-policy-fault.ts.net")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let no_key = server
            .mock("POST", "/tailnet/tail-policy-fault.ts.net/keys")
            .expect(0)
            .create_async()
            .await;

        let error = provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 1,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::ChildPolicyGate {
                status: ChildPolicyStatus::Unavailable,
                delete_acknowledged: true,
                ..
            }
        ));
        assert_eq!(provider.child_secret_count(), 1);
        organization_token.assert_async().await;
        create.assert_async().await;
        child_token.assert_async().await;
        policy_write.assert_async().await;
        delayed_read.assert_async().await;
        delete.assert_async().await;
        no_key.assert_async().await;
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn unsupported_platform_cannot_construct_provider_encrypted_custody() {
        assert!(matches!(
            EncryptedChildVault::open("unused-provider-vault", "unused-provider-key").await,
            Err(VaultError::Invalid)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn encrypted_vault_recovers_prepare_after_restart_without_second_create() {
        let root =
            std::env::temp_dir().join(format!("spurfire-provider-vault-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let key_path = root.join("key");
        tokio::fs::write(&key_path, [9_u8; 32]).await.unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .await
                .unwrap();
            tokio::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        let durable = Arc::new(
            EncryptedChildVault::open(root.join("vault.json"), &key_path)
                .await
                .unwrap(),
        );
        let mut server = Server::new_async().await;
        let token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::UrlEncoded("client_id".into(), "org".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"organization-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let child_token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::UrlEncoded(
                "client_id".into(),
                "child-restart-id".into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"child-token","expires_in":3600}"#)
            .expect(2)
            .create_async()
            .await;
        let create = server
            .mock("POST", "/organizations/-/tailnets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id":"TtRestartVaultCNTRL", "dnsName":"restart-vault.ts.net",
                    "displayName":"spurfire-00000000-0000-4000-8000-000000000001",
                    "oauthClient":{"id":"child-restart-id","secret":"child-restart-secret"}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let request = PrepareLobbyRequest {
            lobby_id,
            network_generation: 7,
            mode: ProvisioningMode::TailnetPerLobby,
            dry_run: false,
        };
        let policy_write = server
            .mock("POST", "/tailnet/restart-vault.ts.net/acl")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let policy_read = server
            .mock("GET", "/tailnet/restart-vault.ts.net/acl")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(policy_readback(lobby_id))
            .expect(2)
            .create_async()
            .await;
        let mut first =
            TailscaleProvider::new(TailscaleClient::new(server.url(), "org", "secret"), "-");
        first.durable_child_vault = Some(Arc::clone(&durable));
        let prepared = first.prepare_lobby(request).await.unwrap();
        drop(first);
        let mut restarted =
            TailscaleProvider::new(TailscaleClient::new(server.url(), "org", "secret"), "-");
        restarted.durable_child_vault = Some(durable);
        let recovered = restarted.prepare_lobby(request).await.unwrap();
        assert_eq!(recovered.tailnet, prepared.tailnet);
        assert_eq!(recovered.identity, prepared.identity);
        let erase = TailnetPresenceRequest {
            lobby_id,
            network_generation: 7,
            identity: recovered.identity.unwrap(),
        };
        restarted.erase_child_secret(erase.clone()).await.unwrap();
        // Models startup after durable erase but before the non-secret cleanup
        // state commit: custody validation must preserve cleanup-only recovery.
        restarted
            .validate_child_custody(lobby_id, 7, &erase.identity)
            .unwrap();
        // The retry must treat the exact missing tuple plus receipt as erased.
        restarted.erase_child_secret(erase).await.unwrap();
        token.assert_async().await;
        child_token.assert_async().await;
        create.assert_async().await;
        policy_write.assert_async().await;
        policy_read.assert_async().await;
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn generation_mismatch_blocks_child_deletion_before_destructive_io() {
        let mut server = Server::new_async().await;
        let organization_token = server
            .mock("POST", "/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"organization-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let create = server
            .mock("POST", "/organizations/-/tailnets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id":"TtExactCNTRL",
                    "dnsName":"tail-exact.ts.net",
                    "displayName":"spurfire-00000000-0000-4000-8000-000000000001",
                    "oauthClient":{"id":"child-id","secret":"child-secret"}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let child_token = server
            .mock("POST", "/oauth/token")
            .match_body(Matcher::UrlEncoded("client_id".into(), "child-id".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"child-token","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let policy_write = server
            .mock("POST", "/tailnet/tail-exact.ts.net/acl")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let policy_read = server
            .mock("GET", "/tailnet/tail-exact.ts.net/acl")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(policy_readback(lobby_id))
            .expect(1)
            .create_async()
            .await;
        let no_delete = server
            .mock("DELETE", Matcher::Regex(".*".to_owned()))
            .expect(0)
            .create_async()
            .await;
        let provider = TailscaleProvider::new(
            TailscaleClient::new(server.url(), "org-client", "org-secret"),
            "shared.ts.net",
        );
        let prepared = provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                network_generation: 7,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .unwrap();

        let result = provider
            .cleanup_lobby(CleanupLobbyRequest {
                lobby_id,
                network_generation: 8,
                identity: prepared.identity,
                mode: ProvisioningMode::TailnetPerLobby,
                tailnet: prepared.tailnet,
                tag: "tag:spurfire-test".to_owned(),
                credentials: Vec::new(),
                include_devices: true,
                now: UnixMillis::new(1),
                dry_run: false,
            })
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::IdentityMismatch);
        assert_eq!(provider.child_secret_count(), 1);
        organization_token.assert_async().await;
        create.assert_async().await;
        child_token.assert_async().await;
        policy_write.assert_async().await;
        policy_read.assert_async().await;
        no_delete.assert_async().await;
    }

    #[tokio::test]
    async fn missing_child_vault_entry_fails_closed_with_stable_reason() {
        let provider = TailscaleProvider::new(
            TailscaleClient::new("http://127.0.0.1:1", "org-client", "org-secret"),
            "-",
        );
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let error = provider
            .lobby_access_error(
                lobby_id,
                ProvisioningMode::TailnetPerLobby,
                NetworkLifecycle::Active,
                false,
            )
            .unwrap();
        assert_eq!(
            error.state_reason(),
            "child_secret_unavailable_manual_remediation"
        );

        let result = provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                network_generation: 1,
                identity: Some(ProviderNetworkIdentity {
                    provider_tailnet_id: Some("TtRestartedCNTRL".to_owned()),
                    tailnet_dns_name: TailnetDnsName::parse("tail-restarted.ts.net").unwrap(),
                }),
                mode: ProvisioningMode::TailnetPerLobby,
                player_id: PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap(),
                tailnet: "tail-restarted.ts.net".to_owned(),
                tag: dedicated_rider_tag(lobby_id),
                expires_at: UnixMillis::new(300_000),
                dry_run: false,
            })
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::ChildSecretUnavailable);
    }

    #[tokio::test]
    async fn mint_scope_denial_downgrades_cached_capabilities() {
        let mut server = Server::new_async().await;
        let token = server
            .mock("POST", "/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"token-canary","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let organization_tailnets = server
            .mock("GET", "/organizations/-/tailnets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tailnets":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let keys = server
            .mock("GET", "/tailnet/-/keys")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let devices = server
            .mock("GET", "/tailnet/-/devices")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"devices":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let acl = server
            .mock("GET", "/tailnet/-/acl")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let denied_mint = server
            .mock("POST", "/tailnet/-/keys")
            .with_status(403)
            .with_body("scope denied canary")
            .expect(1)
            .create_async()
            .await;
        let provider = TailscaleProvider::new(
            TailscaleClient::new(server.url(), "client-canary", "secret-canary"),
            "-",
        );
        assert!(provider
            .refresh_capabilities()
            .await
            .shared_tailnet_available());
        let result = provider
            .mint_credential(MintCredentialRequest {
                lobby_id: LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                network_generation: 1,
                identity: None,
                mode: ProvisioningMode::SharedTailnet,
                player_id: PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap(),
                tailnet: "-".to_owned(),
                tag: "tag:spurfire-lobby-test".to_owned(),
                expires_at: UnixMillis::new(300_000),
                dry_run: false,
            })
            .await;
        assert!(matches!(
            result,
            Err(ProviderError::InsufficientScopes {
                operation: "auth_keys"
            })
        ));
        assert!(!provider.cached_capabilities().can_mint_auth_keys);
        assert!(!provider.cached_capabilities().shared_tailnet_available());

        token.assert_async().await;
        organization_tailnets.assert_async().await;
        keys.assert_async().await;
        devices.assert_async().await;
        acl.assert_async().await;
        denied_mint.assert_async().await;
    }
}
