//! # spurfire-control
//!
//! Control plane for Spurfire lobby lifecycle (see `docs/design.md`, "Lobby lifecycle").
//!
//! This crate talks to the Tailscale API. It is the ONLY place the Tailscale OAuth
//! client credentials are used — they must never ship in a game client.
//!
//! Two provisioning modes are supported by callers:
//! - [`ProvisioningMode::TailnetPerLobby`] is currently unavailable because the alpha
//!   tailnet-create API was not present during probing.
//! - [`ProvisioningMode::SharedTailnet`] uses normal auth-key and device endpoints.

use std::time::{Duration, Instant};

use reqwest::Method;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

/// How lobbies are provisioned.
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
    pub tags: Vec<String>,
    #[serde(default, rename = "lastSeen")]
    pub last_seen: Option<String>,
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("missing env var: {0}")]
    Env(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("Tailscale alpha tailnet provisioning is unavailable: {0}")]
    ProvisioningUnavailable(String),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Async Tailscale API client. Caches the OAuth token until shortly before expiry.
pub struct TailscaleClient {
    http: reqwest::Client,
    api_base: String,
    client_id: String,
    client_secret: String,
    token: Mutex<Option<CachedToken>>,
}

impl TailscaleClient {
    /// Build from env: `TS_CLIENT_ID`, `TS_CLIENT_SECRET`, `TS_API_BASE`.
    ///
    /// The caller should load `.env` first. The CLI uses [`dotenvy`].
    pub async fn from_env() -> Result<Self, ControlError> {
        let get = |name: &str| std::env::var(name).map_err(|_| ControlError::Env(name.into()));
        Ok(Self::new(
            get("TS_API_BASE")?,
            get("TS_CLIENT_ID")?,
            get("TS_CLIENT_SECRET")?,
        ))
    }

    /// Build with an explicit API base and credentials.
    ///
    /// This is useful for tests and deployments that do not source credentials from env.
    pub fn new(
        api_base: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            token: Mutex::new(None),
        }
    }

    async fn access_token(&self) -> Result<String, ControlError> {
        let mut cache = self.token.lock().await;
        if let Some(token) = cache.as_ref() {
            if Instant::now() + Duration::from_secs(60) < token.expires_at {
                return Ok(token.value.clone());
            }
        }

        let response = self
            .http
            .post(format!("{}/oauth/token", self.api_base))
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
            ])
            .send()
            .await?;
        let token: TokenResponse = Self::decode(response).await?;
        let value = token.access_token.clone();
        *cache = Some(CachedToken {
            value,
            expires_at: Instant::now() + Duration::from_secs(token.expires_in),
        });
        Ok(token.access_token)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, ControlError> {
        let token = self.access_token().await?;
        let mut request = self
            .http
            .request(method, format!("{}{}", self.api_base, path))
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(Self::http_error(response).await)
        }
    }

    async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ControlError> {
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            return Err(ControlError::Http {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn http_error(response: reqwest::Response) -> ControlError {
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| error.to_string());
        ControlError::Http { status, body }
    }

    /// Create a dedicated lobby tailnet.
    ///
    /// Empirical probes found no available alpha create endpoint, so this fails clearly rather
    /// than sending a request to an unverified route. Use [`ProvisioningMode::SharedTailnet`].
    pub async fn create_tailnet(&self, _name: &str) -> Result<Tailnet, ControlError> {
        Err(ControlError::ProvisioningUnavailable(
            "no tailnet-create endpoint is available to this OAuth client; use shared mode".into(),
        ))
    }

    /// Delete a dedicated lobby tailnet.
    pub async fn delete_tailnet(&self, _tailnet: &str) -> Result<(), ControlError> {
        Err(ControlError::ProvisioningUnavailable(
            "no tailnet-delete endpoint is available to this OAuth client; use shared mode".into(),
        ))
    }

    /// Mint a short-lived, one-use, tagged, ephemeral auth key for one lobby player.
    pub async fn create_auth_key(
        &self,
        tailnet: &str,
        opts: &AuthKeyOpts,
    ) -> Result<AuthKey, ControlError> {
        let path = format!("/tailnet/{tailnet}/keys");
        let body = serde_json::json!({
            "capabilities": {
                "devices": {
                    "create": {
                        "reusable": opts.reusable,
                        "ephemeral": opts.ephemeral,
                        "preauthorized": opts.preauthorized,
                        "tags": opts.tags,
                    }
                }
            },
            "expirySeconds": opts.ttl_secs,
        });
        let response = self.send(Method::POST, &path, Some(&body)).await?;
        Self::decode(response).await
    }

    /// List devices currently joined to a tailnet.
    pub async fn list_devices(&self, tailnet: &str) -> Result<Vec<Device>, ControlError> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum DevicesResponse {
            Wrapped { devices: Vec<Device> },
            Bare(Vec<Device>),
        }

        let path = format!("/tailnet/{tailnet}/devices");
        let response = self.send(Method::GET, &path, None).await?;
        match Self::decode(response).await? {
            DevicesResponse::Wrapped { devices } | DevicesResponse::Bare(devices) => Ok(devices),
        }
    }

    /// Remove a device (lobby cleanup / kick).
    pub async fn delete_device(&self, device_id: &str) -> Result<(), ControlError> {
        let path = format!("/device/{device_id}");
        self.send(Method::DELETE, &path, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mockito::{Matcher, Server};
    use serde_json::json;

    use super::*;

    async fn token_mock(server: &mut Server, expires_in: u64, expected: usize) -> mockito::Mock {
        server
            .mock("POST", "/oauth/token")
            .match_header(
                "content-type",
                Matcher::Regex("application/x-www-form-urlencoded.*".into()),
            )
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("grant_type".into(), "client_credentials".into()),
                Matcher::UrlEncoded("client_id".into(), "client".into()),
                Matcher::UrlEncoded("client_secret".into(), "secret".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({"access_token":"test-token","expires_in":expires_in}).to_string())
            .expect(expected)
            .create_async()
            .await
    }

    #[tokio::test]
    async fn caches_unexpired_token() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let settings = server
            .mock("GET", "/tailnet/example/devices")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"devices":[]}"#)
            .expect(2)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        client.list_devices("example").await.unwrap();
        client.list_devices("example").await.unwrap();

        token.assert_async().await;
        settings.assert_async().await;
    }

    #[tokio::test]
    async fn refreshes_token_inside_sixty_second_window() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 30, 2).await;
        let settings = server
            .mock("GET", "/tailnet/example/devices")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"devices":[]}"#)
            .expect(2)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        client.list_devices("example").await.unwrap();
        client.list_devices("example").await.unwrap();

        token.assert_async().await;
        settings.assert_async().await;
    }

    #[tokio::test]
    async fn sends_auth_key_contract_shape() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let key = server
            .mock("POST", "/tailnet/shared/keys")
            .match_header("authorization", "Bearer test-token")
            .match_body(Matcher::Json(json!({
                "capabilities":{"devices":{"create":{
                    "reusable":false,
                    "ephemeral":true,
                    "preauthorized":true,
                    "tags":["tag:spurfire-test"]
                }}},
                "expirySeconds":300
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"key-id","key":"tskey-auth-secret"}"#)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");
        let opts = AuthKeyOpts {
            tags: vec!["tag:spurfire-test".into()],
            ..AuthKeyOpts::default()
        };

        let result = client.create_auth_key("shared", &opts).await.unwrap();

        assert_eq!(result.id, "key-id");
        token.assert_async().await;
        key.assert_async().await;
    }

    #[tokio::test]
    async fn maps_api_errors_with_status_and_body() {
        let mut server = Server::new_async().await;
        let _token = token_mock(&mut server, 3600, 1).await;
        let _devices = server
            .mock("GET", "/tailnet/shared/devices")
            .with_status(403)
            .with_body("permission denied")
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        let error = client.list_devices("shared").await.unwrap_err();

        match error {
            ControlError::Http { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body, "permission denied");
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
