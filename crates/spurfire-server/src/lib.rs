//! Prototype Axum lobby service for Spurfire.
//!
//! Gameplay remains peer-to-peer. This crate owns only lobby metadata,
//! deterministic authority inputs, and narrowly scoped join enrollment.

#![forbid(unsafe_code)]

pub mod clock;
pub mod config;
mod crypto;
pub mod error;
pub mod provider;
pub mod service;
pub mod store;
pub mod vault;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{Config, ConfigError};
pub use error::ApiError;
pub use provider::{
    CleanupLobbyRequest, CleanupOutcome, CredentialCleanup, DryRunProvider, MintCredentialRequest,
    MintedCredential, MutationGatedProvider, NetworkProvider, ObserveNetworkRequest,
    PrepareLobbyRequest, PreparedNetwork, ProviderCapabilities, ProviderDeviceObservation,
    ProviderError, ProviderNetworkIdentity, SecretString, TailnetPresenceRequest,
    TailscaleProvider,
};
pub use service::{build_router, router, AppState};
pub use store::{
    CreateStoreOutcome, InMemoryStore, JsonFileStore, LobbyStore, StoreError,
    StoredCapabilityVerifier, StoredCredential, StoredLobby, StoredNetworkIdentity,
};
pub use vault::{ChildVaultIdentity, EncryptedChildVault, VaultError};
