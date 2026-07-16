//! # spurfire-control
//!
//! Control plane for Spurfire lobby lifecycle (see `docs/design.md`, "Lobby lifecycle").
//!
//! This crate talks to the Tailscale API. It is the ONLY place the Tailscale OAuth
//! client credentials are used — they must never ship in a game client.
//!
//! ## API contract (implement the `todo!()`s)
//!
//! Endpoint paths are NOT fully verified. The OAuth token endpoint works
//! (`POST {api_base}/oauth/token`, client_credentials grant). `POST {api_base}/tailnets`
//! returned 404 — the real tailnet-creation path (alpha "multiple tailnets" API) must be
//! discovered empirically before wiring it in. See `docs/tailscale-api.md` when present;
//! otherwise probe with curl. Standard endpoints (auth keys, devices) are documented by
//! Tailscale and are safe to use.
//!
//! Design for TWO provisioning modes, selectable at runtime:
//! - `TailnetPerLobby`: dedicated tailnet per lobby (alpha; may be unavailable).
//! - `SharedTailnet`: one managed tailnet + per-lobby tags/ACLs (fallback).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How lobbies are provisioned. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvisioningMode {
    TailnetPerLobby,
    SharedTailnet,
}

/// A created lobby tailnet (or tailnet-like container).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tailnet {
    pub name: String,
    /// Raw API response — the alpha shape is unstable, keep everything.
    pub raw: serde_json::Value,
}

/// Options for minting a per-player lobby credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKeyOpts {
    /// One-use, short-lived, ephemeral: lobby keys must be all three.
    pub ephemeral: bool,
    pub preauthorized: bool,
    pub reusable: bool,
    pub tags: Vec<String>,
    pub ttl_secs: u64,
}

impl Default for AuthKeyOpts {
    fn default() -> Self {
        Self {
            ephemeral: true,
            preauthorized: true,
            reusable: false,
            tags: vec!["tag:spurfire-lobby".into()],
            ttl_secs: 300,
        }
    }
}

/// A minted auth key. `key` is a secret — never log it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    pub id: String,
    pub key: String,
}

/// A device joined to a tailnet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub addresses: Vec<String>,
    #[serde(default)]
    pub last_seen: Option<String>,
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("missing env var: {0}")]
    Env(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Async Tailscale API client. Caches the OAuth token until expiry.
pub struct TailscaleClient {
    // http client, api base, client id/secret, cached token + expiry
}

impl TailscaleClient {
    /// Build from env: TS_CLIENT_ID, TS_CLIENT_SECRET, TS_API_BASE.
    /// (Caller should load .env first — see dotenvy.)
    pub async fn from_env() -> Result<Self, ControlError> {
        todo!()
    }

    /// Create a dedicated lobby tailnet (alpha API — verify path empirically first).
    pub async fn create_tailnet(&self, _name: &str) -> Result<Tailnet, ControlError> {
        todo!()
    }

    /// Delete a lobby tailnet and (implicitly) its ephemeral devices.
    pub async fn delete_tailnet(&self, _tailnet: &str) -> Result<(), ControlError> {
        todo!()
    }

    /// Mint a short-lived, one-use, tagged, ephemeral auth key for one lobby player.
    pub async fn create_auth_key(
        &self,
        _tailnet: &str,
        _opts: &AuthKeyOpts,
    ) -> Result<AuthKey, ControlError> {
        todo!()
    }

    /// List devices currently joined to a tailnet.
    pub async fn list_devices(&self, _tailnet: &str) -> Result<Vec<Device>, ControlError> {
        todo!()
    }

    /// Remove a device (lobby cleanup / kick).
    pub async fn delete_device(&self, _device_id: &str) -> Result<(), ControlError> {
        todo!()
    }
}
