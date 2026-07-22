//! Kubernetes Lease backed anti-rollback authority for protected Alpha.
//!
//! The PVC is evidence, never admission authority. Before every broker mutation
//! the caller must read and compare this external record and replace it with the
//! exact `resourceVersion` returned by that read. A copied or rolled-back PVC
//! therefore cannot recreate admission authority.

use reqwest::{Certificate, Client, StatusCode};
use serde::{Deserialize, Serialize};
use spurfire_protocol::{LobbyId, UnixMillis};
use std::{path::Path, time::Duration};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const LEASE_BINDING_ANNOTATION: &str = "alpha.spurfire.dev/binding-v1";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedPhase {
    Admission,
    CleanupOnly,
    Released,
    Quarantined,
}

impl ProtectedPhase {
    #[must_use]
    pub const fn permits_admission(self) -> bool {
        matches!(self, Self::Admission)
    }
}

/// Exact non-secret tuple duplicated into the named Lease annotation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseBinding {
    pub installation_id: String,
    /// Resource version of the unbound, pre-install Lease named by the signed
    /// receipt. Installing this binding necessarily advances Kubernetes'
    /// current resourceVersion, so the immutable receipt version must travel
    /// inside the binding rather than be confused with the next CAS version.
    pub receipt_resource_version: String,
    pub state_store_id_sha256: [u8; 32],
    pub receipt_digest: [u8; 32],
    pub lobby_id: LobbyId,
    pub generation: u64,
    pub supervisor_epoch: u64,
    pub state_sha256: [u8; 32],
    pub phase: ProtectedPhase,
    pub admission_play_deadline: UnixMillis,
    pub cleanup_deadline: UnixMillis,
}

impl LeaseBinding {
    pub fn validate(&self) -> Result<(), LeaseError> {
        if self.installation_id.len() < 16
            || self.installation_id.len() > 128
            || !self
                .installation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || self.state_store_id_sha256 == [0; 32]
            || self.receipt_resource_version.is_empty()
            || self.receipt_digest == [0; 32]
            || self.state_sha256 == [0; 32]
            || self.generation == 0
            || self.supervisor_epoch == 0
            || self.admission_play_deadline >= self.cleanup_deadline
        {
            return Err(LeaseError::InvalidBinding);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseSnapshot {
    pub uid: String,
    pub resource_version: String,
    pub binding: LeaseBinding,
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("named Lease authority is unavailable")]
    Unavailable,
    #[error("named Lease response is malformed")]
    Malformed,
    #[error("named Lease binding is invalid")]
    InvalidBinding,
    #[error("named Lease compare-and-swap conflict")]
    Conflict,
    #[error("named Lease is absent")]
    Absent,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObjectMeta {
    name: String,
    namespace: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    uid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    resource_version: String,
    #[serde(default)]
    annotations: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LeaseDocument {
    api_version: String,
    kind: String,
    metadata: ObjectMeta,
    spec: LeaseSpec,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LeaseSpec {
    holder_identity: String,
    lease_duration_seconds: u32,
}

/// Minimal client for one compile/configuration-bound Lease URL. It never lists
/// or deletes and has no method accepting an arbitrary object name.
pub struct KubernetesLeaseAuthority {
    client: Client,
    url: String,
    namespace: String,
    lease_name: String,
    bearer: Zeroizing<String>,
}

impl std::fmt::Debug for KubernetesLeaseAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KubernetesLeaseAuthority")
            .field("namespace", &self.namespace)
            .field("lease_name", &self.lease_name)
            .field("bearer", &"<redacted>")
            .finish()
    }
}

impl KubernetesLeaseAuthority {
    /// Reads the projected service-account files. No token is accepted through
    /// argv or environment and diagnostics never include it.
    pub fn from_service_account(
        namespace: impl Into<String>,
        lease_name: impl Into<String>,
        token_path: impl AsRef<Path>,
        ca_path: impl AsRef<Path>,
    ) -> Result<Self, LeaseError> {
        let namespace = namespace.into();
        let lease_name = lease_name.into();
        if !dns_label(&namespace) || !dns_label(&lease_name) {
            return Err(LeaseError::InvalidBinding);
        }
        let mut token_bytes = std::fs::read(token_path).map_err(|_| LeaseError::Unavailable)?;
        let token = String::from_utf8(token_bytes.clone()).map_err(|_| LeaseError::Unavailable)?;
        token_bytes.zeroize();
        let bearer = Zeroizing::new(token.trim().to_owned());
        if bearer.is_empty() {
            return Err(LeaseError::Unavailable);
        }
        let ca = std::fs::read(ca_path).map_err(|_| LeaseError::Unavailable)?;
        let certificate = Certificate::from_pem(&ca).map_err(|_| LeaseError::Unavailable)?;
        let client = Client::builder()
            .https_only(true)
            .add_root_certificate(certificate)
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| LeaseError::Unavailable)?;
        let url = format!(
            "https://kubernetes.default.svc/apis/coordination.k8s.io/v1/namespaces/{namespace}/leases/{lease_name}"
        );
        Ok(Self {
            client,
            url,
            namespace,
            lease_name,
            bearer,
        })
    }

    pub async fn read(&self) -> Result<LeaseSnapshot, LeaseError> {
        let response = self
            .client
            .get(&self.url)
            .bearer_auth(self.bearer.as_str())
            .send()
            .await
            .map_err(|_| LeaseError::Unavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(LeaseError::Absent);
        }
        decode_response(response).await
    }

    /// Creates only the fixed name. Installation should normally pre-create
    /// the Lease; this exists for an explicitly granted first-install path.
    pub async fn create(&self, binding: &LeaseBinding) -> Result<LeaseSnapshot, LeaseError> {
        binding.validate()?;
        let collection = self
            .url
            .rsplit_once('/')
            .map(|(prefix, _)| prefix)
            .ok_or(LeaseError::InvalidBinding)?;
        let document = self.document("", "", binding)?;
        let response = self
            .client
            .post(collection)
            .bearer_auth(self.bearer.as_str())
            .json(&document)
            .send()
            .await
            .map_err(|_| LeaseError::Unavailable)?;
        decode_response(response).await
    }

    /// Exact Kubernetes optimistic CAS. The returned resourceVersion must be
    /// used for the next mutation; stale callers can only conflict.
    pub async fn compare_and_swap(
        &self,
        expected: &LeaseSnapshot,
        next: &LeaseBinding,
    ) -> Result<LeaseSnapshot, LeaseError> {
        next.validate()?;
        if expected.uid.is_empty() || expected.resource_version.is_empty() {
            return Err(LeaseError::InvalidBinding);
        }
        let current = self.read().await?;
        if current != *expected {
            return Err(LeaseError::Conflict);
        }
        let document = self.document(&expected.uid, &expected.resource_version, next)?;
        let response = self
            .client
            .put(&self.url)
            .bearer_auth(self.bearer.as_str())
            .json(&document)
            .send()
            .await
            .map_err(|_| LeaseError::Unavailable)?;
        decode_response(response).await
    }

    fn document(
        &self,
        uid: &str,
        resource_version: &str,
        binding: &LeaseBinding,
    ) -> Result<LeaseDocument, LeaseError> {
        let encoded = serde_json::to_string(binding).map_err(|_| LeaseError::Malformed)?;
        Ok(LeaseDocument {
            api_version: "coordination.k8s.io/v1".to_owned(),
            kind: "Lease".to_owned(),
            metadata: ObjectMeta {
                name: self.lease_name.clone(),
                namespace: self.namespace.clone(),
                uid: uid.to_owned(),
                resource_version: resource_version.to_owned(),
                annotations: [(LEASE_BINDING_ANNOTATION.to_owned(), encoded)]
                    .into_iter()
                    .collect(),
            },
            spec: LeaseSpec {
                holder_identity: format!("{}/{}", binding.lobby_id, binding.generation),
                lease_duration_seconds: 3600,
            },
        })
    }
}

async fn decode_response(response: reqwest::Response) -> Result<LeaseSnapshot, LeaseError> {
    if response.status() == StatusCode::CONFLICT {
        return Err(LeaseError::Conflict);
    }
    if !response.status().is_success() {
        return Err(LeaseError::Unavailable);
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(LeaseError::Malformed);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| LeaseError::Unavailable)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(LeaseError::Malformed);
    }
    let document: LeaseDocument =
        serde_json::from_slice(&bytes).map_err(|_| LeaseError::Malformed)?;
    if document.api_version != "coordination.k8s.io/v1" || document.kind != "Lease" {
        return Err(LeaseError::Malformed);
    }
    let encoded = document
        .metadata
        .annotations
        .get(LEASE_BINDING_ANNOTATION)
        .ok_or(LeaseError::Malformed)?;
    let binding: LeaseBinding = serde_json::from_str(encoded).map_err(|_| LeaseError::Malformed)?;
    binding.validate()?;
    if document.metadata.uid.is_empty() || document.metadata.resource_version.is_empty() {
        return Err(LeaseError::Malformed);
    }
    Ok(LeaseSnapshot {
        uid: document.metadata.uid,
        resource_version: document.metadata.resource_version,
        binding,
    })
}

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_capability_distinct_and_bindings_fail_closed() {
        assert!(ProtectedPhase::Admission.permits_admission());
        for phase in [
            ProtectedPhase::CleanupOnly,
            ProtectedPhase::Released,
            ProtectedPhase::Quarantined,
        ] {
            assert!(!phase.permits_admission());
        }
        let mut binding = LeaseBinding {
            installation_id: "installation-alpha-1".into(),
            receipt_resource_version: "6".into(),
            state_store_id_sha256: [1; 32],
            receipt_digest: [2; 32],
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-0000000000aa").unwrap(),
            generation: 1,
            supervisor_epoch: 1,
            state_sha256: [3; 32],
            phase: ProtectedPhase::Admission,
            admission_play_deadline: UnixMillis::new(10),
            cleanup_deadline: UnixMillis::new(20),
        };
        assert!(binding.validate().is_ok());
        binding.receipt_resource_version.clear();
        assert!(binding.validate().is_err());
    }

    #[test]
    fn authority_has_no_arbitrary_name_or_delete_surface() {
        assert!(dns_label("spurfire-alpha-authority"));
        assert!(!dns_label("other/lease"));
    }
}
