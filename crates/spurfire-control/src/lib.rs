//! # spurfire-control
//!
//! Secret-safe Tailscale control-plane primitives for Spurfire lobby lifecycle.
//! Organization credentials can create API-only child tailnets through
//! `/organizations/-/tailnets`; the one-time child OAuth credentials remain in memory and are
//! used only through [`ChildTailscaleClient`]. Shared-tailnet callers continue to use the normal
//! auth-key and device methods on [`TailscaleClient`].

use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, Instant},
};

use reqwest::{Method, Url};
use serde::{de, de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const REDACTED: &str = "<redacted>";
const MAX_TAILNET_DNS_NAME_LEN: usize = 253;
/// The single application UDP port admitted between lobby riders.
pub const SPURFIRE_GAMEPLAY_UDP_PORT: u16 = 41_643;

/// Validated provider-returned tailnet DNS name/FQDN.
///
/// The canonical representation is lowercase ASCII without a trailing root
/// dot. This is topology metadata rather than a credential, but diagnostics
/// still omit it so accidental logs do not become a lobby directory.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TailnetDnsName(String);

impl TailnetDnsName {
    /// Parses and canonicalizes a complete DNS name before it can enter a
    /// provider path or durable identity tuple.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() || !value.is_ascii() {
            return Err("tailnet DNS name must be non-empty ASCII");
        }
        let value = value.strip_suffix('.').unwrap_or(value);
        if value.is_empty() || value.len() > MAX_TAILNET_DNS_NAME_LEN {
            return Err("tailnet DNS name length is invalid");
        }
        let mut label_count = 0_usize;
        for label in value.split('.') {
            label_count += 1;
            if label.is_empty() || label.len() > 63 {
                return Err("tailnet DNS label length is invalid");
            }
            let bytes = label.as_bytes();
            if bytes.first() == Some(&b'-')
                || bytes.last() == Some(&b'-')
                || !bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            {
                return Err("tailnet DNS label characters are invalid");
            }
        }
        if label_count < 2 {
            return Err("tailnet DNS name must be fully qualified");
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// Returns the canonical complete DNS name/FQDN.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TailnetDnsName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TailnetDnsName(<topology-metadata>)")
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
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// How lobbies are provisioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvisioningMode {
    TailnetPerLobby,
    SharedTailnet,
}

macro_rules! redacted_secret_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        pub struct $name(Zeroizing<String>);

        impl $name {
            /// Wrap secret material. The allocation is zeroized when its final owner is dropped.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(Zeroizing::new(value.into()))
            }

            fn expose_secret(&self) -> &str {
                self.0.as_str()
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                Self::new(self.expose_secret())
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.expose_secret() == other.expose_secret()
            }
        }

        impl Eq for $name {}

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(REDACTED)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(REDACTED)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(REDACTED)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                String::deserialize(deserializer).map(Self::new)
            }
        }
    };
}

redacted_secret_type!(
    /// Child-tailnet OAuth client identifier. It is treated as credential material even though
    /// an OAuth client ID is not independently sufficient for authentication.
    ChildOAuthClientId
);
redacted_secret_type!(
    /// One-time child-tailnet OAuth client secret.
    ChildOAuthClientSecret
);
redacted_secret_type!(SecretMaterial);

/// OAuth material returned exactly once when an organization creates an API-only tailnet.
///
/// Diagnostic and serialized forms are always redacted. Callers transfer this value into
/// [`TailscaleClient::child_scoped`] rather than persisting it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildOAuthCredentials {
    #[serde(rename = "id")]
    client_id: ChildOAuthClientId,
    #[serde(rename = "secret")]
    client_secret: ChildOAuthClientSecret,
}

impl ChildOAuthCredentials {
    /// Constructs child credentials, primarily for secret-manager adapters and tests.
    #[must_use]
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: ChildOAuthClientId::new(client_id),
            client_secret: ChildOAuthClientSecret::new(client_secret),
        }
    }

    /// Transfers both secret allocations into an encrypted-vault adapter.
    /// Callers must encrypt immediately and must never log or persist plaintext.
    #[must_use]
    pub fn into_secret_parts(self) -> (Zeroizing<String>, Zeroizing<String>) {
        (self.client_id.0, self.client_secret.0)
    }
}

impl fmt::Debug for ChildOAuthCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChildOAuthCredentials(<redacted>)")
    }
}

impl fmt::Display for ChildOAuthCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

/// A newly created API-only child tailnet.
///
/// This typed response deliberately has no raw JSON field: the upstream create body contains a
/// one-time child secret and must not be retained wholesale.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tailnet {
    /// Stable organization tailnet ID.
    pub id: String,
    /// Provider-returned, validated tailnet DNS name/FQDN.
    pub dns_name: TailnetDnsName,
    /// Human-facing organization display name.
    pub display_name: String,
    #[serde(rename = "oauthClient")]
    oauth_credentials: ChildOAuthCredentials,
}

impl Tailnet {
    /// Splits the typed non-secret identity from the one-time OAuth material.
    ///
    /// Callers must bind the stable ID and DNS name to their durable generation
    /// before using the child credential for any destructive operation.
    #[must_use]
    pub fn into_parts(self) -> (String, TailnetDnsName, String, ChildOAuthCredentials) {
        (
            self.id,
            self.dns_name,
            self.display_name,
            self.oauth_credentials,
        )
    }

    /// Transfers only the one-time OAuth material into a child-scoped client.
    /// Prefer [`Self::into_parts`] when the provider identity must be retained.
    #[must_use]
    pub fn into_child_oauth_credentials(self) -> ChildOAuthCredentials {
        self.oauth_credentials
    }
}

impl fmt::Debug for Tailnet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tailnet")
            .field("id", &self.id)
            .field("dns_name", &self.dns_name)
            .field("display_name", &self.display_name)
            .field("oauth_credentials", &REDACTED)
            .finish()
    }
}

/// Non-secret entry returned by the organization tailnet listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationTailnet {
    /// Stable organization tailnet ID.
    pub id: String,
    /// Human-facing organization display name.
    pub display_name: String,
    /// Validated DNS name/FQDN, when supplied by the listing API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<TailnetDnsName>,
    /// Organization identifier, when supplied by the listing API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    /// Provider creation timestamp, kept as its wire string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
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

/// Normalized restrictive policy installed in every dedicated lobby tailnet.
///
/// Construction is intentionally generated rather than caller-defined: the one rider tag may
/// reach only the Spurfire application UDP port on same-tag peers. Empty policy sections are
/// explicit so SSH, Funnel/Serve attributes, route auto-approval, exit-node approval, legacy ACLs,
/// and policy tests cannot accidentally inherit permissive values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildTailnetPolicy {
    wire: ChildTailnetPolicyWire,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyGrant {
    src: Vec<String>,
    dst: Vec<String>,
    ip: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyAutoApprovers {
    #[serde(default)]
    routes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    exit_node: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChildTailnetPolicyWire {
    #[serde(default)]
    groups: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    hosts: BTreeMap<String, String>,
    #[serde(default)]
    tag_owners: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    grants: Vec<PolicyGrant>,
    #[serde(default)]
    acls: Vec<serde_json::Value>,
    #[serde(default)]
    ssh: Vec<serde_json::Value>,
    #[serde(default)]
    node_attrs: Vec<serde_json::Value>,
    #[serde(default)]
    auto_approvers: PolicyAutoApprovers,
    #[serde(default)]
    tests: Vec<serde_json::Value>,
}

impl ChildTailnetPolicy {
    /// Generates the only child-tailnet policy Spurfire accepts.
    pub fn restrictive_riders(rider_tag: &str) -> Result<Self, ControlError> {
        validate_rider_tag(rider_tag)?;
        let mut tag_owners = BTreeMap::new();
        tag_owners.insert(rider_tag.to_owned(), Vec::new());
        Ok(Self {
            wire: ChildTailnetPolicyWire {
                tag_owners,
                grants: vec![PolicyGrant {
                    src: vec![rider_tag.to_owned()],
                    dst: vec![rider_tag.to_owned()],
                    ip: vec![format!("udp:{SPURFIRE_GAMEPLAY_UDP_PORT}")],
                }],
                ..ChildTailnetPolicyWire::default()
            },
        })
    }

    fn from_wire(mut wire: ChildTailnetPolicyWire) -> Self {
        normalize_map_values(&mut wire.groups);
        normalize_map_values(&mut wire.tag_owners);
        normalize_map_values(&mut wire.auto_approvers.routes);
        wire.auto_approvers.exit_node.sort();
        wire.auto_approvers.exit_node.dedup();
        for grant in &mut wire.grants {
            grant.src.sort();
            grant.src.dedup();
            grant.dst.sort();
            grant.dst.dedup();
            grant.ip.sort();
            grant.ip.dedup();
        }
        wire.grants.sort();
        wire.grants.dedup();
        Self { wire }
    }

    /// Returns a stable SHA-256 digest of normalized policy semantics, never a provider body.
    #[must_use]
    pub fn semantic_digest(&self) -> String {
        let normalized = Self::from_wire(self.wire.clone());
        let bytes = serde_json::to_vec(&normalized.wire)
            .expect("the generated child policy is always JSON serializable");
        let digest = Sha256::digest(bytes);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Compares normalized policy meaning, independent of object and set-like array order.
    #[must_use]
    pub fn semantically_matches(&self, other: &Self) -> bool {
        Self::from_wire(self.wire.clone()).wire == Self::from_wire(other.wire.clone()).wire
    }
}

fn normalize_map_values(map: &mut BTreeMap<String, Vec<String>>) {
    for values in map.values_mut() {
        values.sort();
        values.dedup();
    }
}

fn validate_rider_tag(tag: &str) -> Result<(), ControlError> {
    let suffix = tag.strip_prefix("tag:spurfire-lobby-").unwrap_or_default();
    if !suffix.is_empty()
        && tag.len() <= 128
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        Ok(())
    } else {
        Err(ControlError::InvalidPolicy)
    }
}

/// Non-secret evidence that an exact normalized policy passed provider readback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildPolicyEvidence {
    /// SHA-256 of normalized policy semantics.
    pub semantic_digest: String,
}

/// A minted auth key. `key` is a secret — never log it.
#[derive(Clone, Deserialize)]
pub struct AuthKey {
    pub id: String,
    pub key: String,
}

impl Serialize for AuthKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct RedactedAuthKey<'a> {
            id: &'a str,
            key: &'static str,
        }
        RedactedAuthKey {
            id: &self.id,
            key: REDACTED,
        }
        .serialize(serializer)
    }
}

impl fmt::Debug for AuthKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthKey")
            .field("id", &self.id)
            .field("key", &REDACTED)
            .finish()
    }
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

/// One authoritative inventory page. `request_cursor` is the cursor used to
/// obtain this response and `next_cursor=None` is the only terminal signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryPage<T> {
    pub request_cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub items: Vec<T>,
}

/// Inventory that can only be constructed after every cursor reaches a unique
/// terminal page within fixed bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompleteInventory<T> {
    items: Vec<T>,
}

impl<T> CompleteInventory<T> {
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }
    #[must_use]
    pub fn into_items(self) -> Vec<T> {
        self.items
    }
}

/// Validate a provider pagination transcript. Errors, timeouts and partial
/// transcripts never produce this type and therefore cannot establish absence.
pub fn collect_complete_inventory<T>(
    pages: Vec<InventoryPage<T>>,
    max_pages: usize,
    max_items: usize,
) -> Result<CompleteInventory<T>, ControlError> {
    use std::collections::BTreeSet;
    if pages.is_empty() || pages.len() > max_pages {
        return Err(ControlError::IncompletePagination);
    }
    let mut expected = None;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let page_count = pages.len();
    for (index, page) in pages.into_iter().enumerate() {
        if page.request_cursor != expected {
            return Err(ControlError::IncompletePagination);
        }
        if let Some(cursor) = page.request_cursor.as_ref() {
            if cursor.is_empty()
                || cursor.len() > 512
                || cursor
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
                || !seen.insert(cursor.clone())
            {
                return Err(ControlError::IncompletePagination);
            }
        }
        if items.len().saturating_add(page.items.len()) > max_items {
            return Err(ControlError::IncompletePagination);
        }
        items.extend(page.items);
        expected = page.next_cursor;
        if expected.is_none() && index + 1 != page_count {
            return Err(ControlError::IncompletePagination);
        }
    }
    if expected.is_some() {
        return Err(ControlError::IncompletePagination);
    }
    Ok(CompleteInventory { items })
}

/// Secret-safe control-plane failure.
#[derive(Error)]
pub enum ControlError {
    #[error("missing env var: {0}")]
    Env(String),
    #[error("http {status}: upstream response body discarded")]
    Http { status: u16 },
    #[error("tailnet provisioning request is invalid: {0}")]
    InvalidTailnetName(&'static str),
    #[error("tailnet operation requires child-scoped OAuth credentials: {0}")]
    ProvisioningUnavailable(String),
    #[error("provider URL or path could not be constructed safely")]
    InvalidProviderPath,
    #[error("child-tailnet policy is invalid")]
    InvalidPolicy,
    #[error("child-tailnet policy readback did not match required semantics")]
    PolicyMismatch,
    #[error("provider pagination was partial, malformed, repeated, or over limit")]
    IncompletePagination,
    #[error("Tailscale transport failed; details redacted")]
    Reqwest(#[source] reqwest::Error),
    #[error("Tailscale JSON response was invalid; details redacted")]
    Json(#[source] serde_json::Error),
}

impl From<reqwest::Error> for ControlError {
    fn from(error: reqwest::Error) -> Self {
        Self::Reqwest(error)
    }
}

impl From<serde_json::Error> for ControlError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl fmt::Debug for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(name) => formatter.debug_tuple("Env").field(name).finish(),
            Self::Http { status } => formatter
                .debug_struct("Http")
                .field("status", status)
                .field("body", &"<discarded>")
                .finish(),
            Self::InvalidTailnetName(reason) => formatter
                .debug_tuple("InvalidTailnetName")
                .field(reason)
                .finish(),
            Self::ProvisioningUnavailable(_) => {
                formatter.write_str("ProvisioningUnavailable(<redacted>)")
            }
            Self::InvalidProviderPath => formatter.write_str("InvalidProviderPath"),
            Self::InvalidPolicy => formatter.write_str("InvalidPolicy"),
            Self::PolicyMismatch => formatter.write_str("PolicyMismatch"),
            Self::IncompletePagination => formatter.write_str("IncompletePagination"),
            Self::Reqwest(_) => formatter.write_str("Reqwest(<redacted>)"),
            Self::Json(_) => formatter.write_str("Json(<redacted>)"),
        }
    }
}

struct CachedToken {
    value: SecretMaterial,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: SecretMaterial,
    expires_in: u64,
}

struct OAuthSession {
    http: reqwest::Client,
    api_base: String,
    client_id: SecretMaterial,
    client_secret: SecretMaterial,
    token: Mutex<Option<CachedToken>>,
}

impl OAuthSession {
    fn new(
        http: reqwest::Client,
        api_base: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            http,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            client_id: SecretMaterial::new(client_id),
            client_secret: SecretMaterial::new(client_secret),
            token: Mutex::new(None),
        }
    }

    async fn access_token(&self) -> Result<SecretMaterial, ControlError> {
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
                ("client_id", self.client_id.expose_secret()),
                ("client_secret", self.client_secret.expose_secret()),
            ])
            .send()
            .await?;
        let token: TokenResponse = Self::decode(response).await?;
        let result = token.access_token.clone();
        *cache = Some(CachedToken {
            value: token.access_token,
            expires_at: Instant::now() + Duration::from_secs(token.expires_in),
        });
        Ok(result)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, ControlError> {
        let url = Url::parse(&format!("{}{}", self.api_base, path))
            .map_err(|_| ControlError::InvalidProviderPath)?;
        self.send_url(method, url, body).await
    }

    async fn send_segments(
        &self,
        method: Method,
        segments: &[&str],
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, ControlError> {
        let mut url = Url::parse(&format!("{}/", self.api_base))
            .map_err(|_| ControlError::InvalidProviderPath)?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| ControlError::InvalidProviderPath)?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        self.send_url(method, url, body).await
    }

    async fn send_url(
        &self,
        method: Method,
        url: Url,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, ControlError> {
        let token = self.access_token().await?;
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token.expose_secret());
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
        if !status.is_success() {
            return Err(ControlError::Http {
                status: status.as_u16(),
            });
        }
        let bytes = Zeroizing::new(response.bytes().await?.to_vec());
        Ok(serde_json::from_slice(bytes.as_slice())?)
    }

    async fn http_error(response: reqwest::Response) -> ControlError {
        ControlError::Http {
            status: response.status().as_u16(),
        }
    }

    async fn probe_token(&self) -> Result<(), ControlError> {
        let _token = self.access_token().await?;
        Ok(())
    }

    async fn probe_tailnet_resource(
        &self,
        tailnet: &str,
        resource: &str,
    ) -> Result<(), ControlError> {
        validate_tailnet_selector(tailnet)?;
        self.send_segments(Method::GET, &["tailnet", tailnet, resource], None)
            .await?;
        Ok(())
    }

    async fn create_auth_key(
        &self,
        tailnet: &str,
        opts: &AuthKeyOpts,
    ) -> Result<AuthKey, ControlError> {
        validate_tailnet_selector(tailnet)?;
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
        let response = self
            .send_segments(Method::POST, &["tailnet", tailnet, "keys"], Some(&body))
            .await?;
        Self::decode(response).await
    }

    async fn delete_auth_key(
        &self,
        tailnet: &str,
        credential_id: &str,
    ) -> Result<(), ControlError> {
        validate_tailnet_selector(tailnet)?;
        self.send_segments(
            Method::DELETE,
            &["tailnet", tailnet, "keys", credential_id],
            None,
        )
        .await?;
        Ok(())
    }

    async fn list_devices(&self, tailnet: &str) -> Result<Vec<Device>, ControlError> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum DevicesResponse {
            Wrapped { devices: Vec<Device> },
            Bare(Vec<Device>),
        }

        validate_tailnet_selector(tailnet)?;
        let response = self
            .send_segments(Method::GET, &["tailnet", tailnet, "devices"], None)
            .await?;
        match Self::decode(response).await? {
            DevicesResponse::Wrapped { devices } | DevicesResponse::Bare(devices) => Ok(devices),
        }
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), ControlError> {
        self.send_segments(Method::DELETE, &["device", device_id], None)
            .await?;
        Ok(())
    }

    async fn delete_tailnet(&self, dns_name: &str) -> Result<(), ControlError> {
        let dns_name = TailnetDnsName::parse(dns_name)
            .map_err(|_| ControlError::InvalidTailnetName("invalid tailnet DNS name/FQDN"))?;
        match self
            .send_segments(Method::DELETE, &["tailnet", dns_name.as_str()], None)
            .await
        {
            Ok(_) | Err(ControlError::Http { status: 404 }) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn validate_configured_api_base(value: &str) -> Result<(), ControlError> {
    let url = Url::parse(value).map_err(|_| ControlError::InvalidProviderPath)?;
    let exact_origin = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some()
        && url.path().trim_end_matches('/') == "/api/v2";
    if exact_origin {
        Ok(())
    } else {
        Err(ControlError::InvalidProviderPath)
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("built-in reqwest timeout configuration is valid")
}

/// Async organization-scoped Tailscale API client. Caches its OAuth token until shortly before
/// expiry.
pub struct TailscaleClient {
    session: OAuthSession,
}

impl TailscaleClient {
    /// Build from env: `TS_CLIENT_ID`, `TS_CLIENT_SECRET`, `TS_API_BASE`.
    pub async fn from_env() -> Result<Self, ControlError> {
        let get = |name: &str| std::env::var(name).map_err(|_| ControlError::Env(name.into()));
        let api_base = get("TS_API_BASE")?;
        validate_configured_api_base(&api_base)?;
        Ok(Self::new(
            api_base,
            get("TS_CLIENT_ID")?,
            get("TS_CLIENT_SECRET")?,
        ))
    }

    /// Build with an explicit API base and organization credentials.
    #[must_use]
    pub fn new(
        api_base: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            session: OAuthSession::new(http_client(), api_base, client_id, client_secret),
        }
    }

    /// Verify that the organization OAuth token exchange succeeds without making an API mutation.
    pub async fn probe_oauth_token(&self) -> Result<(), ControlError> {
        self.session.probe_token().await
    }

    /// List API-only and primary tailnets visible to the organization token.
    pub async fn list_organization_tailnets(
        &self,
    ) -> Result<Vec<OrganizationTailnet>, ControlError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ListResponse {
            tailnets: Vec<OrganizationTailnet>,
            #[serde(default, alias = "next", alias = "nextCursor")]
            next_page: Option<String>,
        }

        const MAX_PAGES: usize = 32;
        const MAX_ITEMS: usize = 4_096;
        let mut pages = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = Url::parse(&format!(
                "{}/organizations/-/tailnets",
                self.session.api_base
            ))
            .map_err(|_| ControlError::InvalidProviderPath)?;
            if let Some(value) = cursor.as_deref() {
                url.query_pairs_mut().append_pair("cursor", value);
            }
            let response = self.session.send_url(Method::GET, url, None).await?;
            let decoded = OAuthSession::decode::<ListResponse>(response).await?;
            let next = decoded.next_page.filter(|value| !value.is_empty());
            pages.push(InventoryPage {
                request_cursor: cursor.clone(),
                next_cursor: next.clone(),
                items: decoded.tailnets,
            });
            if next.is_none() {
                break;
            }
            if pages.len() >= MAX_PAGES {
                return Err(ControlError::IncompletePagination);
            }
            cursor = next;
        }
        Ok(collect_complete_inventory(pages, MAX_PAGES, MAX_ITEMS)?.into_items())
    }

    /// Create one API-only child tailnet through the verified organization endpoint.
    pub async fn create_tailnet(&self, display_name: &str) -> Result<Tailnet, ControlError> {
        validate_tailnet_display_name(display_name)?;
        let body = serde_json::json!({"displayName": display_name});
        let response = self
            .session
            .send(Method::POST, "/organizations/-/tailnets", Some(&body))
            .await?;
        // Decode directly into the typed shape. The response bytes are dropped immediately and
        // are never retained as a generic JSON value.
        OAuthSession::decode(response).await
    }

    /// Construct a client whose tokens and operations are scoped to one API-only child tailnet.
    #[must_use]
    pub fn child_scoped(&self, credentials: ChildOAuthCredentials) -> ChildTailscaleClient {
        let client_id = credentials.client_id.expose_secret().to_owned();
        let client_secret = credentials.client_secret.expose_secret().to_owned();
        ChildTailscaleClient {
            session: OAuthSession::new(
                self.session.http.clone(),
                self.session.api_base.clone(),
                client_id,
                client_secret,
            ),
        }
    }

    /// Parent tokens must not delete child tailnets. Use [`Self::child_scoped`] and
    /// [`ChildTailscaleClient::delete_tailnet`] instead.
    pub async fn delete_tailnet(&self, _tailnet: &str) -> Result<(), ControlError> {
        Err(ControlError::ProvisioningUnavailable(
            "a child-scoped token is required for deletion".to_owned(),
        ))
    }

    /// Mint a short-lived, one-use, tagged, ephemeral auth key in the shared tailnet.
    pub async fn create_auth_key(
        &self,
        tailnet: &str,
        opts: &AuthKeyOpts,
    ) -> Result<AuthKey, ControlError> {
        self.session.create_auth_key(tailnet, opts).await
    }

    /// Revoke a shared-tailnet auth key by its non-secret receipt identifier.
    pub async fn delete_auth_key(
        &self,
        tailnet: &str,
        credential_id: &str,
    ) -> Result<(), ControlError> {
        self.session.delete_auth_key(tailnet, credential_id).await
    }

    /// Probe token/settings access without mutating the shared tailnet.
    pub async fn probe_settings(&self, tailnet: &str) -> Result<(), ControlError> {
        self.session
            .probe_tailnet_resource(tailnet, "settings")
            .await
    }

    /// Probe shared auth-key scope using the non-mutating key-list endpoint.
    pub async fn probe_auth_keys(&self, tailnet: &str) -> Result<(), ControlError> {
        self.session.probe_tailnet_resource(tailnet, "keys").await
    }

    /// Probe shared ACL scope using the non-mutating policy-read endpoint.
    pub async fn probe_acl(&self, tailnet: &str) -> Result<(), ControlError> {
        self.session.probe_tailnet_resource(tailnet, "acl").await
    }

    /// List devices currently joined to the shared tailnet.
    pub async fn list_devices(&self, tailnet: &str) -> Result<Vec<Device>, ControlError> {
        self.session.list_devices(tailnet).await
    }

    /// Remove a shared-tailnet device.
    pub async fn delete_device(&self, device_id: &str) -> Result<(), ControlError> {
        self.session.delete_device(device_id).await
    }
}

impl fmt::Debug for TailscaleClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TailscaleClient(<redacted>)")
    }
}

/// Child-tailnet client with an independent token cache and no organization-list/create methods.
pub struct ChildTailscaleClient {
    session: OAuthSession,
}

impl ChildTailscaleClient {
    /// Exchange the one-time child OAuth credentials for a child-scoped token without exposing it.
    pub async fn authenticate(&self) -> Result<(), ControlError> {
        self.session.probe_token().await
    }

    /// Writes the generated policy to one exact child-tailnet selector.
    ///
    /// Redirects are disabled by the bounded client, and path segments are percent-encoded without
    /// changing the configured origin. A successful write is not readiness evidence until
    /// [`Self::verify_policy`] succeeds.
    pub async fn write_policy(
        &self,
        tailnet: &str,
        policy: &ChildTailnetPolicy,
    ) -> Result<(), ControlError> {
        validate_tailnet_selector(tailnet)?;
        let body = serde_json::to_value(&policy.wire)?;
        self.session
            .send_segments(Method::POST, &["tailnet", tailnet, "acl"], Some(&body))
            .await?;
        Ok(())
    }

    /// Reads and normalizes one exact child-tailnet policy without retaining a raw provider body.
    pub async fn read_policy(&self, tailnet: &str) -> Result<ChildTailnetPolicy, ControlError> {
        validate_tailnet_selector(tailnet)?;
        let response = self
            .session
            .send_segments(Method::GET, &["tailnet", tailnet, "acl"], None)
            .await?;
        let wire = OAuthSession::decode::<ChildTailnetPolicyWire>(response).await?;
        Ok(ChildTailnetPolicy::from_wire(wire))
    }

    /// Requires a semantic readback match and returns digest-only evidence.
    pub async fn verify_policy(
        &self,
        tailnet: &str,
        expected: &ChildTailnetPolicy,
    ) -> Result<ChildPolicyEvidence, ControlError> {
        let actual = self.read_policy(tailnet).await?;
        if !expected.semantically_matches(&actual) {
            return Err(ControlError::PolicyMismatch);
        }
        Ok(ChildPolicyEvidence {
            semantic_digest: expected.semantic_digest(),
        })
    }

    /// Writes then semantically reads back the required restrictive policy.
    pub async fn apply_and_verify_policy(
        &self,
        tailnet: &str,
        policy: &ChildTailnetPolicy,
    ) -> Result<ChildPolicyEvidence, ControlError> {
        self.write_policy(tailnet, policy).await?;
        self.verify_policy(tailnet, policy).await
    }

    /// Mint a one-use player auth key under the child scope.
    pub async fn create_auth_key(
        &self,
        tailnet: &str,
        opts: &AuthKeyOpts,
    ) -> Result<AuthKey, ControlError> {
        self.session.create_auth_key(tailnet, opts).await
    }

    /// Revoke a child-tailnet auth-key receipt.
    pub async fn delete_auth_key(
        &self,
        tailnet: &str,
        credential_id: &str,
    ) -> Result<(), ControlError> {
        self.session.delete_auth_key(tailnet, credential_id).await
    }

    /// List child-tailnet devices.
    pub async fn list_devices(&self, tailnet: &str) -> Result<Vec<Device>, ControlError> {
        self.session.list_devices(tailnet).await
    }

    /// Delete one child-tailnet device.
    pub async fn delete_device(&self, device_id: &str) -> Result<(), ControlError> {
        self.session.delete_device(device_id).await
    }

    /// Delete the child tailnet. A 404 is treated as successful idempotent cleanup.
    pub async fn delete_tailnet(&self, dns_name: &str) -> Result<(), ControlError> {
        self.session.delete_tailnet(dns_name).await
    }
}

impl fmt::Debug for ChildTailscaleClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChildTailscaleClient(<redacted>)")
    }
}

fn validate_tailnet_selector(tailnet: &str) -> Result<(), ControlError> {
    let safe_compatibility_label = !tailnet.is_empty()
        && tailnet.len() <= 63
        && !tailnet.starts_with('-')
        && !tailnet.ends_with('-')
        && tailnet
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if tailnet == "-" || safe_compatibility_label || TailnetDnsName::parse(tailnet).is_ok() {
        Ok(())
    } else {
        Err(ControlError::InvalidTailnetName(
            "invalid safe tailnet selector",
        ))
    }
}

fn validate_tailnet_display_name(display_name: &str) -> Result<(), ControlError> {
    let valid = !display_name.is_empty()
        && display_name.len() <= 50
        && display_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'\'' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ControlError::InvalidTailnetName(
            "displayName must match ^[A-Za-z0-9' -]{1,50}$",
        ))
    }
}

#[cfg(test)]
mod tests {
    use mockito::{Matcher, Server};
    use serde_json::json;

    use super::*;

    async fn token_mock_for(
        server: &mut Server,
        client_id: &str,
        client_secret: &str,
        access_token: &str,
        expires_in: u64,
        expected: usize,
    ) -> mockito::Mock {
        server
            .mock("POST", "/oauth/token")
            .match_header(
                "content-type",
                Matcher::Regex("application/x-www-form-urlencoded.*".into()),
            )
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("grant_type".into(), "client_credentials".into()),
                Matcher::UrlEncoded("client_id".into(), client_id.into()),
                Matcher::UrlEncoded("client_secret".into(), client_secret.into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({"access_token":access_token,"expires_in":expires_in}).to_string())
            .expect(expected)
            .create_async()
            .await
    }

    async fn token_mock(server: &mut Server, expires_in: u64, expected: usize) -> mockito::Mock {
        token_mock_for(
            server,
            "client",
            "secret",
            "test-token",
            expires_in,
            expected,
        )
        .await
    }

    #[tokio::test]
    async fn oauth_redirect_never_replays_credentials() {
        let mut origin = Server::new_async().await;
        let mut attacker = Server::new_async().await;
        let sink = attacker
            .mock("POST", "/stolen")
            .expect(0)
            .with_status(200)
            .create_async()
            .await;
        let redirect = origin
            .mock("POST", "/oauth/token")
            .with_status(307)
            .with_header("location", &format!("{}/stolen", attacker.url()))
            .expect(1)
            .create_async()
            .await;
        let client = TailscaleClient::new(origin.url(), "client-canary", "secret-canary");
        assert!(matches!(
            client.probe_oauth_token().await,
            Err(ControlError::Http { status: 307 })
        ));
        redirect.assert_async().await;
        sink.assert_async().await;
    }

    #[test]
    fn configured_provider_origin_is_exact_https_api_v2() {
        assert!(validate_configured_api_base("https://api.tailscale.com/api/v2").is_ok());
        for rejected in [
            "http://api.tailscale.com/api/v2",
            "https://user@api.tailscale.com/api/v2",
            "https://api.tailscale.com/api/v2?redirect=1",
            "https://api.tailscale.com/other",
        ] {
            assert!(matches!(
                validate_configured_api_base(rejected),
                Err(ControlError::InvalidProviderPath)
            ));
        }
    }

    #[tokio::test]
    async fn caches_unexpired_token() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let devices = server
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
        devices.assert_async().await;
    }

    #[tokio::test]
    async fn refreshes_token_inside_sixty_second_window() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 30, 2).await;
        let devices = server
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
        devices.assert_async().await;
    }

    #[tokio::test]
    async fn lists_organization_tailnets_at_verified_path() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let list = server
            .mock("GET", "/organizations/-/tailnets")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"tailnets":[{"id":"TtStableCNTRL","displayName":"Spurfire Test","orgId":"org-1","createdAt":"2026-07-16T00:00:00Z"}]}"#,
            )
            .expect(1)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        let tailnets = client.list_organization_tailnets().await.unwrap();

        assert_eq!(tailnets.len(), 1);
        assert_eq!(tailnets[0].id, "TtStableCNTRL");
        assert_eq!(tailnets[0].display_name, "Spurfire Test");
        token.assert_async().await;
        list.assert_async().await;
    }

    #[tokio::test]
    async fn organization_tailnet_absence_requires_terminal_page() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let first = server
            .mock("GET", "/organizations/-/tailnets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tailnets":[],"nextPage":"page-2"}"#)
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/organizations/-/tailnets")
            .match_query(mockito::Matcher::UrlEncoded(
                "cursor".into(),
                "page-2".into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tailnets":[{"id":"late","displayName":"Late"}]}"#)
            .expect(1)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        let tailnets = client.list_organization_tailnets().await.unwrap();

        assert_eq!(
            tailnets
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["late"]
        );
        token.assert_async().await;
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn creates_typed_tailnet_without_exposing_one_time_secret() {
        const CHILD_ID: &str = "child-client-canary";
        const CHILD_SECRET: &str = "child-secret-canary";
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let create = server
            .mock("POST", "/organizations/-/tailnets")
            .match_header("authorization", "Bearer test-token")
            .match_body(Matcher::Json(json!({"displayName":"spurfire-probe-test"})))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "id":"TtStableCNTRL",
                    "dnsName":"tail-test.ts.net",
                    "displayName":"spurfire-probe-test",
                    "oauthClient":{"id":CHILD_ID,"secret":CHILD_SECRET}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        let tailnet = client.create_tailnet("spurfire-probe-test").await.unwrap();
        let debug = format!("{tailnet:?}");
        let serialized = serde_json::to_string(&tailnet).unwrap();

        assert_eq!(tailnet.id, "TtStableCNTRL");
        assert_eq!(tailnet.dns_name.as_str(), "tail-test.ts.net");
        for output in [&debug, &serialized] {
            assert!(!output.contains(CHILD_ID));
            assert!(!output.contains(CHILD_SECRET));
            assert!(output.contains(REDACTED));
        }
        assert!(!debug.contains("raw"));
        token.assert_async().await;
        create.assert_async().await;
    }

    #[tokio::test]
    async fn exchanges_child_credentials_for_child_scoped_token() {
        let mut server = Server::new_async().await;
        let token = token_mock_for(
            &mut server,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let parent = TailscaleClient::new(server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));

        child.authenticate().await.unwrap();

        token.assert_async().await;
    }

    #[test]
    fn generated_child_policy_is_restrictive_and_digest_is_semantic() {
        let tag = "tag:spurfire-lobby-00000000-0000-4000-8000-000000000001";
        let policy = ChildTailnetPolicy::restrictive_riders(tag).unwrap();
        let value = serde_json::to_value(&policy.wire).unwrap();

        assert_eq!(value["tagOwners"][tag], json!([]));
        assert_eq!(
            value["grants"],
            json!([{
                "src":[tag], "dst":[tag], "ip":["udp:41643"]
            }])
        );
        for empty in ["acls", "ssh", "nodeAttrs", "tests"] {
            assert_eq!(value[empty], json!([]));
        }
        assert_eq!(value["autoApprovers"], json!({"routes":{},"exitNode":[]}));
        assert_eq!(policy.semantic_digest().len(), 64);
        assert!(!value.to_string().contains("tcp"));
        assert!(!value.to_string().contains("*:"));
    }

    #[tokio::test]
    async fn policy_write_requires_normalized_matching_readback_before_evidence() {
        let mut server = Server::new_async().await;
        let token = token_mock_for(
            &mut server,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let tag = "tag:spurfire-lobby-00000000-0000-4000-8000-000000000001";
        let policy = ChildTailnetPolicy::restrictive_riders(tag).unwrap();
        let write = server
            .mock("POST", "/tailnet/tail-test.ts.net/acl")
            .match_header("authorization", "Bearer child-token")
            .match_body(Matcher::Json(serde_json::to_value(&policy.wire).unwrap()))
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let read = server
            .mock("GET", "/tailnet/tail-test.ts.net/acl")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Missing empty/default fields and object order do not change semantics.
            .with_body(
                json!({
                    "grants":[{"ip":["udp:41643"],"dst":[tag],"src":[tag]}],
                    "tagOwners":{(tag):[]}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let parent = TailscaleClient::new(server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));

        let evidence = child
            .apply_and_verify_policy("tail-test.ts.net", &policy)
            .await
            .unwrap();

        assert_eq!(evidence.semantic_digest, policy.semantic_digest());
        token.assert_async().await;
        write.assert_async().await;
        read.assert_async().await;
    }

    #[tokio::test]
    async fn policy_mismatch_and_scope_denial_are_typed_and_body_safe() {
        let mut mismatch_server = Server::new_async().await;
        let _token = token_mock_for(
            &mut mismatch_server,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let tag = "tag:spurfire-lobby-00000000-0000-4000-8000-000000000001";
        let policy = ChildTailnetPolicy::restrictive_riders(tag).unwrap();
        let _read = mismatch_server
            .mock("GET", "/tailnet/tail-test.ts.net/acl")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "tagOwners":{(tag):[]},
                    "grants":[{"src":[tag],"dst":[tag],"ip":["tcp:41643"]}]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let parent = TailscaleClient::new(mismatch_server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));
        assert!(matches!(
            child.verify_policy("tail-test.ts.net", &policy).await,
            Err(ControlError::PolicyMismatch)
        ));

        let mut denied_server = Server::new_async().await;
        let _token = token_mock_for(
            &mut denied_server,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let _denied = denied_server
            .mock("POST", "/tailnet/tail-test.ts.net/acl")
            .with_status(403)
            .with_body(r#"{"secret":"provider-body-canary"}"#)
            .expect(1)
            .create_async()
            .await;
        let parent = TailscaleClient::new(denied_server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));
        let error = child
            .write_policy("tail-test.ts.net", &policy)
            .await
            .unwrap_err();
        assert!(matches!(error, ControlError::Http { status: 403 }));
        assert!(!format!("{error:?}").contains("provider-body-canary"));
    }

    #[tokio::test]
    async fn policy_redirect_never_replays_bearer_or_policy_body() {
        let mut origin = Server::new_async().await;
        let mut attacker = Server::new_async().await;
        let _token = token_mock_for(
            &mut origin,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let sink = attacker
            .mock("POST", "/stolen-policy")
            .expect(0)
            .with_status(200)
            .create_async()
            .await;
        let redirect = origin
            .mock("POST", "/tailnet/tail-test.ts.net/acl")
            .with_status(307)
            .with_header("location", &format!("{}/stolen-policy", attacker.url()))
            .expect(1)
            .create_async()
            .await;
        let parent = TailscaleClient::new(origin.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));
        let policy = ChildTailnetPolicy::restrictive_riders(
            "tag:spurfire-lobby-00000000-0000-4000-8000-000000000001",
        )
        .unwrap();

        assert!(matches!(
            child.write_policy("tail-test.ts.net", &policy).await,
            Err(ControlError::Http { status: 307 })
        ));
        redirect.assert_async().await;
        sink.assert_async().await;
    }

    #[tokio::test]
    async fn child_scope_mints_key_with_verified_contract() {
        let mut server = Server::new_async().await;
        let token = token_mock_for(
            &mut server,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let key = server
            .mock("POST", "/tailnet/tail-test.ts.net/keys")
            .match_header("authorization", "Bearer child-token")
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
            .with_body(r#"{"id":"key-id","key":"synthetic-auth-key-secret"}"#)
            .expect(1)
            .create_async()
            .await;
        let parent = TailscaleClient::new(server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));
        let opts = AuthKeyOpts {
            tags: vec!["tag:spurfire-test".into()],
            ..AuthKeyOpts::default()
        };

        let result = child
            .create_auth_key("tail-test.ts.net", &opts)
            .await
            .unwrap();

        assert_eq!(result.id, "key-id");
        assert!(!format!("{result:?}").contains("synthetic-auth-key-secret"));
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("synthetic-auth-key-secret"));
        assert!(serialized.contains(REDACTED));
        token.assert_async().await;
        key.assert_async().await;
    }

    #[tokio::test]
    async fn child_scope_deletes_tailnet_at_verified_path() {
        let mut server = Server::new_async().await;
        let token = token_mock_for(
            &mut server,
            "child-client",
            "child-secret",
            "child-token",
            3600,
            1,
        )
        .await;
        let delete = server
            .mock("DELETE", "/tailnet/tail-test.ts.net")
            .match_header("authorization", "Bearer child-token")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let parent = TailscaleClient::new(server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));

        child.delete_tailnet("tail-test.ts.net").await.unwrap();

        token.assert_async().await;
        delete.assert_async().await;
    }

    #[tokio::test]
    async fn shared_auth_key_api_remains_compatible() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let key = server
            .mock("POST", "/tailnet/shared/keys")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"key-id","key":"synthetic-auth-key-secret"}"#)
            .expect(1)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        let result = client
            .create_auth_key("shared", &AuthKeyOpts::default())
            .await
            .unwrap();

        assert_eq!(result.id, "key-id");
        token.assert_async().await;
        key.assert_async().await;
    }

    #[tokio::test]
    async fn error_discards_upstream_secret_body() {
        const CANARY: &str = "child-secret-must-not-leak";
        let mut server = Server::new_async().await;
        let _token = token_mock(&mut server, 3600, 1).await;
        let _create = server
            .mock("POST", "/organizations/-/tailnets")
            .with_status(500)
            .with_body(format!(r#"{{"secret":"{CANARY}"}}"#))
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        let error = client
            .create_tailnet("spurfire-probe-test")
            .await
            .unwrap_err();

        assert!(!error.to_string().contains(CANARY));
        assert!(!format!("{error:?}").contains(CANARY));
        assert!(error.to_string().contains("discarded"));
    }

    #[tokio::test]
    async fn rejects_tailnet_path_injection_before_oauth_or_provider_io() {
        let mut server = Server::new_async().await;
        let no_requests = server
            .mock("POST", Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let parent = TailscaleClient::new(server.url(), "org-client", "org-secret");
        let child = parent.child_scoped(ChildOAuthCredentials::new("child-client", "child-secret"));

        for malicious in [
            "tail.ts.net/../other",
            "tail.ts.net?target=other",
            "tail.ts.net#fragment",
            "tail%2fother.ts.net",
            "user@tail.ts.net",
            "tail.ts.net:443",
            "tail\nts.net",
        ] {
            assert!(matches!(
                child.delete_tailnet(malicious).await,
                Err(ControlError::InvalidTailnetName(_))
            ));
            assert!(matches!(
                parent
                    .create_auth_key(malicious, &AuthKeyOpts::default())
                    .await,
                Err(ControlError::InvalidTailnetName(_))
            ));
        }
        no_requests.assert_async().await;
    }

    #[tokio::test]
    async fn rejects_untyped_provider_fqdn_in_create_response() {
        let mut server = Server::new_async().await;
        let token = token_mock(&mut server, 3600, 1).await;
        let create = server
            .mock("POST", "/organizations/-/tailnets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "id":"TtStableCNTRL",
                    "dnsName":"tail.ts.net/../other",
                    "displayName":"spurfire-probe-test",
                    "oauthClient":{"id":"child-id","secret":"child-secret"}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let client = TailscaleClient::new(server.url(), "client", "secret");

        assert!(matches!(
            client.create_tailnet("spurfire-probe-test").await,
            Err(ControlError::Json(_))
        ));
        token.assert_async().await;
        create.assert_async().await;
    }

    #[test]
    fn child_credentials_redact_debug_display_and_serde() {
        const ID: &str = "child-id-canary";
        const SECRET: &str = "child-secret-canary";
        let credentials = ChildOAuthCredentials::new(ID, SECRET);
        let outputs = [
            format!("{credentials:?}"),
            credentials.to_string(),
            serde_json::to_string(&credentials).unwrap(),
        ];
        for output in outputs {
            assert!(!output.contains(ID));
            assert!(!output.contains(SECRET));
            assert!(output.contains(REDACTED));
        }
    }

    #[test]
    fn complete_inventory_requires_terminal_unique_bounded_cursor_chain() {
        let complete = collect_complete_inventory(
            vec![
                InventoryPage {
                    request_cursor: None,
                    next_cursor: Some("page-2".into()),
                    items: vec!["other"],
                },
                InventoryPage {
                    request_cursor: Some("page-2".into()),
                    next_cursor: None,
                    items: vec!["stable-id-present"],
                },
            ],
            4,
            8,
        )
        .unwrap();
        assert_eq!(complete.items(), &["other", "stable-id-present"]);
        for pages in [
            vec![InventoryPage {
                request_cursor: None,
                next_cursor: Some("again".into()),
                items: vec![1],
            }],
            vec![
                InventoryPage {
                    request_cursor: None,
                    next_cursor: Some("again".into()),
                    items: vec![1],
                },
                InventoryPage {
                    request_cursor: Some("wrong".into()),
                    next_cursor: None,
                    items: vec![2],
                },
            ],
            vec![
                InventoryPage {
                    request_cursor: None,
                    next_cursor: Some("bad cursor".into()),
                    items: vec![1],
                },
                InventoryPage {
                    request_cursor: Some("bad cursor".into()),
                    next_cursor: None,
                    items: vec![2],
                },
            ],
        ] {
            assert!(matches!(
                collect_complete_inventory(pages, 4, 8),
                Err(ControlError::IncompletePagination)
            ));
        }
    }

    #[test]
    fn rejects_unverified_display_name_shapes_before_http() {
        for invalid in ["", "contains_underscore", "é", &"x".repeat(51)] {
            assert!(matches!(
                validate_tailnet_display_name(invalid),
                Err(ControlError::InvalidTailnetName(_))
            ));
        }
    }
}
