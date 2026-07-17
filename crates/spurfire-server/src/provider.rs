//! Network-provider boundary and Tailscale/dry-run implementations.

use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use spurfire_control::{AuthKeyOpts, ControlError, TailscaleClient};
use spurfire_protocol::{
    LobbyId, PlannedAction, PlayerId, ProvisioningMode, ResponseMetadata, UnixMillis,
    DRY_RUN_AUTH_KEY,
};
use thiserror::Error;

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
pub struct SecretString(String);

impl SecretString {
    /// Wraps provider-returned secret material.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Explicitly transfers secret material into the one allowed join response.
    #[must_use]
    pub fn into_exposed(self) -> String {
        self.0
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

/// Request for lobby resource cleanup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupLobbyRequest {
    /// Lobby being cleaned.
    pub lobby_id: LobbyId,
    /// Provider tailnet selector.
    pub tailnet: String,
    /// Tag used to discover only this lobby's devices.
    pub tag: String,
    /// Number of credential receipts to revoke or let expire.
    pub credential_count: usize,
    /// Whether all mutating work must be simulated.
    pub dry_run: bool,
}

/// Cleanup outcome. Individual device identifiers are intentionally absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupOutcome {
    /// True when capability-dependent cleanup should be retried.
    pub cleanup_pending: bool,
    /// Number of device delete calls attempted.
    pub attempted_device_deletes: usize,
    /// Dry-run response metadata.
    pub metadata: ResponseMetadata,
}

/// Safe provider failure classification. Upstream bodies are always discarded.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// The verified alpha create route is unavailable.
    #[error("requested provisioning mode is unavailable")]
    ModeUnavailable,
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
            Self::ModeUnavailable => "provisioning_mode_unavailable_api_404",
            Self::InsufficientScopes {
                operation: "auth_keys",
            } => "provisioning_blocked_auth_keys_403",
            Self::InsufficientScopes {
                operation: "devices",
            } => "cleanup_blocked_devices_403",
            Self::InsufficientScopes { .. } => "provisioning_blocked_scopes_403",
            Self::Upstream { .. } => "provider_upstream_error",
            Self::Unavailable { .. } => "provider_unavailable",
        }
    }
}

/// Lobby-network operations used by the HTTP layer.
#[async_trait]
pub trait NetworkProvider: Send + Sync {
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

    /// Attempts lobby-scoped device cleanup.
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
    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        if request.mode == ProvisioningMode::TailnetPerLobby {
            return Err(ProviderError::ModeUnavailable);
        }
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

/// Adapter around [`spurfire_control::TailscaleClient`].
pub struct TailscaleProvider {
    client: Arc<TailscaleClient>,
    shared_tailnet: String,
    simulated_mints: AtomicU64,
}

impl TailscaleProvider {
    /// Wraps an existing client, useful with a mock HTTP transport.
    #[must_use]
    pub fn new(client: TailscaleClient, shared_tailnet: impl Into<String>) -> Self {
        Self {
            client: Arc::new(client),
            shared_tailnet: shared_tailnet.into(),
            simulated_mints: AtomicU64::new(0),
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
}

impl fmt::Debug for TailscaleProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TailscaleProvider")
            .field("client", &"<redacted>")
            .field("shared_tailnet", &"<configured>")
            .finish()
    }
}

#[async_trait]
impl NetworkProvider for TailscaleProvider {
    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        if request.mode == ProvisioningMode::TailnetPerLobby {
            return Err(ProviderError::ModeUnavailable);
        }
        if request.dry_run || request.mode == ProvisioningMode::DryRun {
            return Ok(dry_prepared_network());
        }
        Ok(PreparedNetwork {
            tailnet: self.shared_tailnet.clone(),
            dry_run: false,
            metadata: ResponseMetadata::default(),
        })
    }

    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        if request.dry_run {
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
        let key = self
            .client
            .create_auth_key(&request.tailnet, &options)
            .await
            .map_err(|error| map_control_error(error, "auth_keys"))?;
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
        if request.dry_run {
            return Ok(dry_cleanup_outcome(&request));
        }

        let devices = self
            .client
            .list_devices(&request.tailnet)
            .await
            .map_err(|error| map_control_error(error, "devices"))?;
        let mut outcome = CleanupOutcome::default();
        for device in devices
            .into_iter()
            .filter(|device| device.tags.iter().any(|tag| tag == &request.tag))
        {
            outcome.attempted_device_deletes += 1;
            if self.client.delete_device(&device.id).await.is_err() {
                // A later sweep lists the lobby tag again. No device identifier is retained.
                outcome.cleanup_pending = true;
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
    if request.credential_count > 0 {
        planned_actions.push(PlannedAction {
            method: "DELETE".to_owned(),
            path: "/tailnet/-/keys/{credential_id}".to_owned(),
            description: "revoke unconsumed lobby credentials".to_owned(),
        });
    }
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
    CleanupOutcome {
        cleanup_pending: false,
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
        ControlError::ProvisioningUnavailable(_) => ProviderError::ModeUnavailable,
        ControlError::Env(_) | ControlError::Reqwest(_) | ControlError::Json(_) => {
            ProviderError::Unavailable { operation }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString::new("tskey-auth-canary-secret");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert!(!format!("{secret:?}").contains("canary"));
    }
}
