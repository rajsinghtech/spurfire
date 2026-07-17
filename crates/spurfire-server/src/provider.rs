//! Network-provider boundary and Tailscale/dry-run implementations.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
};

use async_trait::async_trait;
use spurfire_control::{AuthKeyOpts, ChildTailscaleClient, ControlError, TailscaleClient};
use spurfire_protocol::{
    CapabilitiesResponse, CapabilityModeStatus, CapabilityModes, LobbyId, PlannedAction, PlayerId,
    ProvisioningMode, ResponseMetadata, UnixMillis, DRY_RUN_AUTH_KEY,
};
use thiserror::Error;
use zeroize::Zeroizing;

/// Provider request made while a lobby record is being prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrepareLobbyRequest {
    /// Public lobby identifier.
    pub lobby_id: LobbyId,
    /// Requested backing mode.
    pub mode: ProvisioningMode,
    /// Whether this operation must be simulated.
    pub dry_run: bool,
}

/// Non-secret provider state retained with a lobby.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedNetwork {
    /// Tailnet selector needed by later provider calls.
    pub tailnet: String,
    /// True when the provider suppressed all mutations.
    pub dry_run: bool,
    /// Response metadata for the create operation.
    pub metadata: ResponseMetadata,
}

/// Request to mint one player credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintCredentialRequest {
    /// Lobby receiving the player.
    pub lobby_id: LobbyId,
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
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    /// Wraps provider-returned secret material.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// Explicitly transfers a copy of secret material into the one allowed join response. The
    /// provider-owned allocation is zeroized as this wrapper is dropped.
    #[must_use]
    pub fn into_exposed(self) -> String {
        self.0.to_string()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Provider result for a first credential issue.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialCleanup {
    /// Provider key identifier, never key material.
    pub credential_id: String,
    /// Key expiry. An already-expired key needs no upstream revoke call.
    pub expires_at: UnixMillis,
}

/// Request for lobby resource cleanup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupLobbyRequest {
    /// Lobby being cleaned.
    pub lobby_id: LobbyId,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupOutcome {
    /// True when capability-dependent cleanup should be retried.
    pub cleanup_pending: bool,
    /// Credential receipts successfully revoked or confirmed expired.
    pub revoked_credential_ids: Vec<String>,
    /// Number of device delete calls attempted.
    pub attempted_device_deletes: usize,
    /// Dry-run response metadata.
    pub metadata: ResponseMetadata,
}

/// Cached, non-secret capability verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    /// A child-tailnet record survived a process restart but its in-memory OAuth material did not.
    #[error("child tailnet credentials are unavailable; manual remediation is required")]
    ChildSecretUnavailable,
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

impl ProviderError {
    /// Machine-readable lobby reason suitable for a public response.
    #[must_use]
    pub fn state_reason(&self) -> &'static str {
        match self {
            Self::ChildSecretUnavailable => "child_secret_unavailable_manual_remediation",
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
        _dry_run: bool,
    ) -> Option<ProviderError> {
        None
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
    dns_name: String,
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
        }
    }

    fn child_access(
        &self,
        lobby_id: LobbyId,
        expected_dns_name: &str,
    ) -> Result<Arc<ChildTailscaleClient>, ProviderError> {
        let vault = self
            .child_vault
            .read()
            .map_err(|_| ProviderError::Unavailable {
                operation: "child_secret_vault",
            })?;
        let access = vault
            .get(&lobby_id)
            .filter(|access| access.dns_name == expected_dns_name)
            .ok_or(ProviderError::ChildSecretUnavailable)?;
        Ok(Arc::clone(&access.client))
    }

    #[cfg(test)]
    fn child_secret_count(&self) -> usize {
        self.child_vault.read().map_or(0, |vault| vault.len())
    }

    /// Builds the control adapter from `TS_API_BASE`, `TS_CLIENT_ID`, and
    /// `TS_CLIENT_SECRET`. The values remain inside `spurfire-control`.
    pub async fn from_env(shared_tailnet: impl Into<String>) -> Result<Self, ProviderError> {
        let client = TailscaleClient::from_env()
            .await
            .map_err(|error| map_control_error(error, "startup"))?;
        Ok(Self::new(client, shared_tailnet))
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
        dry_run: bool,
    ) -> Option<ProviderError> {
        if dry_run || mode != ProvisioningMode::TailnetPerLobby {
            return None;
        }
        match self.child_vault.read() {
            Ok(vault) if vault.contains_key(&lobby_id) => None,
            Ok(_) => Some(ProviderError::ChildSecretUnavailable),
            Err(_) => Some(ProviderError::Unavailable {
                operation: "child_secret_vault",
            }),
        }
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
            return Ok(PreparedNetwork {
                tailnet: self.shared_tailnet.clone(),
                dry_run: false,
                metadata: ResponseMetadata::default(),
            });
        }

        if let Ok(vault) = self.child_vault.read() {
            if let Some(access) = vault.get(&request.lobby_id) {
                return Ok(PreparedNetwork {
                    tailnet: access.dns_name.clone(),
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
        let dns_name = tailnet.dns_name.clone();
        let child = Arc::new(
            self.client
                .child_scoped(tailnet.into_child_oauth_credentials()),
        );
        self.child_vault
            .write()
            .map_err(|_| ProviderError::Unavailable {
                operation: "child_secret_vault",
            })?
            .insert(
                request.lobby_id,
                ChildTailnetAccess {
                    dns_name: dns_name.clone(),
                    client: child,
                },
            );
        if let Ok(mut cache) = self.capabilities.write() {
            cache.oauth_token_ok = true;
            cache.can_manage_organization_tailnets = true;
        }
        Ok(PreparedNetwork {
            tailnet: dns_name,
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
            tags: vec![request.tag],
            ttl_secs: 300,
        };
        let key = match request.mode {
            ProvisioningMode::SharedTailnet => {
                self.client
                    .create_auth_key(&request.tailnet, &options)
                    .await
            }
            ProvisioningMode::TailnetPerLobby => {
                let child = self.child_access(request.lobby_id, &request.tailnet)?;
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
        if request.mode == ProvisioningMode::TailnetPerLobby {
            return self.cleanup_child_tailnet(request).await;
        }
        self.cleanup_shared_tailnet(request).await
    }
}

impl TailscaleProvider {
    async fn cleanup_child_tailnet(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        let child = self.child_access(request.lobby_id, &request.tailnet)?;
        if request.include_devices {
            child
                .delete_tailnet(&request.tailnet)
                .await
                .map_err(|error| map_control_error(error, "child_tailnet_delete"))?;
            self.child_vault
                .write()
                .map_err(|_| ProviderError::Unavailable {
                    operation: "child_secret_vault",
                })?
                .remove(&request.lobby_id);
            return Ok(CleanupOutcome {
                cleanup_pending: false,
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

fn map_control_error(error: ControlError, operation: &'static str) -> ProviderError {
    match error {
        ControlError::Http { status: 403, .. } => ProviderError::InsufficientScopes { operation },
        ControlError::Http { status, .. } => ProviderError::Upstream { operation, status },
        ControlError::ProvisioningUnavailable(_) => ProviderError::Unavailable { operation },
        ControlError::Env(_)
        | ControlError::InvalidTailnetName(_)
        | ControlError::Reqwest(_)
        | ControlError::Json(_) => ProviderError::Unavailable { operation },
    }
}

#[cfg(test)]
mod tests {
    use mockito::{Matcher, Server};

    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString::new("tskey-auth-canary-secret");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert!(!format!("{secret:?}").contains("canary"));
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
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: true,
            })
            .await
            .unwrap();
        let minted = provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                mode: ProvisioningMode::DryRun,
                player_id,
                tailnet: "-".to_owned(),
                tag: "tag:spurfire-lobby-test".to_owned(),
                expires_at: UnixMillis::new(300_000),
                dry_run: true,
            })
            .await
            .unwrap();
        assert_eq!(minted.auth_key.into_exposed(), DRY_RUN_AUTH_KEY);
        provider
            .cleanup_lobby(CleanupLobbyRequest {
                lobby_id,
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
    }

    #[tokio::test]
    async fn tailnet_per_lobby_uses_child_scope_then_deletes_and_evicts() {
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
        let key = server
            .mock("POST", "/tailnet/tail-provider.ts.net/keys")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"key-receipt","key":"tskey-auth-join-secret"}"#)
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
        let provider = TailscaleProvider::new(
            TailscaleClient::new(server.url(), "org-client", "org-secret"),
            "-",
        );
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let player_id = PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap();

        let prepared = provider
            .prepare_lobby(PrepareLobbyRequest {
                lobby_id,
                mode: ProvisioningMode::TailnetPerLobby,
                dry_run: false,
            })
            .await
            .unwrap();
        assert_eq!(prepared.tailnet, "tail-provider.ts.net");
        assert_eq!(provider.child_secret_count(), 1);
        let provider_debug = format!("{provider:?}");
        assert!(!provider_debug.contains(CHILD_ID));
        assert!(!provider_debug.contains(CHILD_SECRET));

        let minted = provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                mode: ProvisioningMode::TailnetPerLobby,
                player_id,
                tailnet: prepared.tailnet.clone(),
                tag: "tag:spurfire-lobby-test".to_owned(),
                expires_at: UnixMillis::new(300_000),
                dry_run: false,
            })
            .await
            .unwrap();
        assert_eq!(minted.credential_id, "key-receipt");
        assert_eq!(minted.auth_key.into_exposed(), "tskey-auth-join-secret");

        let outcome = provider
            .cleanup_lobby(CleanupLobbyRequest {
                lobby_id,
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
        assert!(!outcome.cleanup_pending);
        assert_eq!(outcome.revoked_credential_ids, ["key-receipt"]);
        assert_eq!(provider.child_secret_count(), 0);

        organization_token.assert_async().await;
        create.assert_async().await;
        child_token.assert_async().await;
        key.assert_async().await;
        delete.assert_async().await;
    }

    #[tokio::test]
    async fn missing_child_vault_entry_fails_closed_with_stable_reason() {
        let provider = TailscaleProvider::new(
            TailscaleClient::new("http://127.0.0.1:1", "org-client", "org-secret"),
            "-",
        );
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let error = provider
            .lobby_access_error(lobby_id, ProvisioningMode::TailnetPerLobby, false)
            .unwrap();
        assert_eq!(
            error.state_reason(),
            "child_secret_unavailable_manual_remediation"
        );

        let result = provider
            .mint_credential(MintCredentialRequest {
                lobby_id,
                mode: ProvisioningMode::TailnetPerLobby,
                player_id: PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap(),
                tailnet: "tail-restarted.ts.net".to_owned(),
                tag: "tag:spurfire-lobby-test".to_owned(),
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
