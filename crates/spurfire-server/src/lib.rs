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
pub mod rehearsal;
pub mod service;
pub mod store;
pub mod vault;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{Config, ConfigError};
pub use error::ApiError;
pub use provider::{
    ChildPolicyStatus, CleanupLobbyRequest, CleanupOutcome, CredentialCleanup, DryRunProvider,
    MintCredentialRequest, MintedCredential, MutationGatedProvider, NetworkProvider,
    ObserveNetworkRequest, PrepareLobbyRequest, PreparedNetwork, ProviderCapabilities,
    ProviderDeviceObservation, ProviderError, ProviderNetworkIdentity, SecretString,
    TailnetPresenceRequest, TailscaleProvider,
};
pub use rehearsal::{
    verify_local_rehearsal_receipt, LocalRehearsalClaims, LocalRehearsalQualification,
    LocalRehearsalReceipt, RehearsalReceiptError, RehearsalVerificationContext,
    LOCAL_REHEARSAL_AUDIENCE, REHEARSAL_POLICY_PROFILE, REVIEWED_SOURCE_SHA,
};
pub use service::{build_local_rehearsal_router, build_router, router, AppState};
pub use store::{
    CreateStoreOutcome, InMemoryStore, JsonFileStore, LobbyStore, StoreError,
    StoredCapabilityVerifier, StoredCredential, StoredLobby, StoredNetworkIdentity,
};
pub use vault::{ChildVaultIdentity, EncryptedChildVault, VaultError};
