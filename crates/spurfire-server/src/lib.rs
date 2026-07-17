//! Prototype Axum lobby service for Spurfire.
//!
//! Gameplay remains peer-to-peer. This crate owns only lobby metadata,
//! deterministic authority inputs, and narrowly scoped join enrollment.

#![forbid(unsafe_code)]

pub mod clock;
pub mod config;
pub mod error;
pub mod provider;
pub mod service;
pub mod store;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{Config, ConfigError};
pub use error::ApiError;
pub use provider::{
    CleanupLobbyRequest, CleanupOutcome, DryRunProvider, MintCredentialRequest, MintedCredential,
    NetworkProvider, PrepareLobbyRequest, PreparedNetwork, ProviderError, SecretString,
    TailscaleProvider,
};
pub use service::{build_router, router, AppState};
pub use store::{
    CreateStoreOutcome, InMemoryStore, LobbyStore, StoreError, StoredCredential, StoredLobby,
};
