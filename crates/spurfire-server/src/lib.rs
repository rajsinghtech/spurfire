//! Prototype Axum lobby service for Spurfire.
//!
//! Gameplay remains peer-to-peer. This crate owns only lobby metadata,
//! deterministic authority inputs, and narrowly scoped join enrollment.

#![deny(unsafe_code)]

#[allow(unsafe_code)]
pub mod alpha_execution;
pub mod clock;
pub mod config;
mod crypto;
pub mod error;
pub mod lease_authority;
pub mod owner_key;
pub mod protected_broker;
pub mod provider;
pub mod rehearsal;
pub mod service;
pub mod store;
pub mod supervision;
pub mod vault;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{Config, ConfigError};
pub use error::ApiError;
pub use lease_authority::{
    KubernetesLeaseAuthority, LeaseBinding, LeaseError, LeaseSnapshot, ProtectedPhase,
};
pub use protected_broker::{
    BrokerFence, BrokerProtocolError, BrokerServer, CleanupOnlyBrokerTransport,
    MtlsBrokerProviderTransport,
};
pub use provider::{
    BrokerProvider, BrokerProviderTransport, ChildPolicyStatus, CleanupLobbyRequest,
    CleanupOutcome, CredentialCleanup, DryRunProvider, MintCredentialRequest, MintedCredential,
    MutationGatedProvider, NetworkProvider, ObserveNetworkRequest, PrepareLobbyRequest,
    PreparedNetwork, ProviderCapabilities, ProviderDeviceObservation, ProviderError,
    ProviderNetworkIdentity, SecretString, TailnetPresenceRequest, TailscaleProvider,
};
pub use rehearsal::{
    verify_local_rehearsal_receipt, verify_protected_alpha_receipt,
    verify_protected_alpha_recovery_receipt, LocalRehearsalClaims, LocalRehearsalQualification,
    LocalRehearsalReceipt, ProtectedAlphaClaims, ProtectedAlphaQualification,
    ProtectedAlphaReceipt, ProtectedAlphaVerificationContext, RehearsalReceiptError,
    RehearsalVerificationContext, ALPHA_CLEANUP_MS, ALPHA_PLAY_MS, LOCAL_REHEARSAL_AUDIENCE,
    PROTECTED_ALPHA_AUDIENCE, PROTECTED_ALPHA_PURPOSE, REHEARSAL_POLICY_PROFILE,
    REVIEWED_SOURCE_SHA,
};
pub use service::{
    build_local_rehearsal_router, build_protected_alpha_operator_router,
    build_protected_alpha_public_router, build_router, router, AppState,
};
pub use store::{
    CreateStoreOutcome, InMemoryStore, JsonFileStore, LobbyStore, ProtectedAlphaRecovery,
    StoreBinding, StoreError, StoredCapabilityVerifier, StoredCredential, StoredLobby,
    StoredNetworkIdentity,
};
pub use supervision::{
    run_cleanup, AbsenceObservation, BrokerRequest, CredentialBroker, Fence, LedgerStore,
    Operation, OperationOutcome, SupervisedIdentity, SupervisionError, SupervisorLedger,
};
pub use vault::{ChildVaultIdentity, EncryptedChildVault, VaultError};
