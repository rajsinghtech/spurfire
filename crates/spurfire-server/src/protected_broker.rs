//! Authenticated private ClusterIP provider broker protocol.
//!
//! TLS authenticates both pods from pinned installation CA files. Each frame is
//! additionally fenced by a per-run MAC, monotonic sequence, random nonce,
//! exact lobby/run/epoch tuple and the current Kubernetes Lease UID/resourceVersion.

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
    ClientConfig, RootCertStore, ServerConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spurfire_protocol::{LobbyId, UnixMillis};
use std::{
    io::BufReader,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    lease_authority::{KubernetesLeaseAuthority, LeaseBinding, LeaseSnapshot, ProtectedPhase},
    provider::{
        BrokerProviderTransport, CleanupLobbyRequest, CleanupOutcome, MintCredentialRequest,
        MintedCredential, NetworkProvider, ObserveNetworkRequest, PrepareLobbyRequest,
        PreparedNetwork, ProviderCapabilities, ProviderDeviceObservation, ProviderError,
        SecretString, TailnetPresenceRequest,
    },
};

const PROTOCOL: &str = "spurfire-private-broker/v1";
const MAX_FRAME: usize = 128 * 1024;
type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error)]
pub enum BrokerProtocolError {
    #[error("broker transport unavailable")]
    Unavailable,
    #[error("broker TLS identity is invalid")]
    TlsIdentity,
    #[error("broker frame is malformed")]
    Malformed,
    #[error("broker fence is stale")]
    StaleFence,
    #[error("broker operation is forbidden in this phase")]
    ForbiddenPhase,
    #[error("broker sequence or MAC is invalid")]
    Authentication,
    #[error("broker provider operation failed")]
    Provider,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
enum WireOperation {
    Capabilities,
    Prepare(PrepareLobbyRequest),
    Mint(MintCredentialRequest),
    Cleanup(CleanupLobbyRequest),
    Observe(ObserveNetworkRequest),
    Present(TailnetPresenceRequest),
    Erase(TailnetPresenceRequest),
    Release,
}

impl WireOperation {
    const fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Prepare(_) | Self::Mint(_) | Self::Cleanup(_) | Self::Erase(_) | Self::Release
        )
    }

    const fn is_admission(&self) -> bool {
        matches!(self, Self::Prepare(_) | Self::Mint(_))
    }

    fn exact_tuple(&self) -> Option<(LobbyId, u64)> {
        match self {
            Self::Capabilities | Self::Release => None,
            Self::Prepare(value) => Some((value.lobby_id, value.network_generation)),
            Self::Mint(value) => Some((value.lobby_id, value.network_generation)),
            Self::Cleanup(value) => Some((value.lobby_id, value.network_generation)),
            Self::Observe(value) => Some((value.lobby_id, value.network_generation)),
            Self::Present(value) | Self::Erase(value) => {
                Some((value.lobby_id, value.network_generation))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
enum WireResult {
    Capabilities(ProviderCapabilities),
    Prepared(PreparedNetwork),
    Minted {
        credential_id: String,
        auth_key: String,
        tailnet: String,
        metadata: spurfire_protocol::ResponseMetadata,
    },
    Cleanup(CleanupOutcome),
    Observation(ProviderDeviceObservation),
    Present(bool),
    Erased,
    Released,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    protocol: String,
    run_id: String,
    lobby_id: LobbyId,
    generation: u64,
    supervisor_epoch: u64,
    sequence: u64,
    nonce: [u8; 32],
    lease_uid: String,
    lease_resource_version: String,
    operation: WireOperation,
    mac: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    sequence: u64,
    nonce: [u8; 32],
    lease_uid: String,
    lease_resource_version: String,
    result: WireResult,
    mac: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct BrokerFence {
    pub run_id: String,
    pub lobby_id: LobbyId,
    pub generation: u64,
    pub supervisor_epoch: u64,
    pub phase: ProtectedPhase,
    pub admission_play_deadline: UnixMillis,
    pub cleanup_deadline: UnixMillis,
}

impl BrokerFence {
    fn validate_operation(
        &self,
        operation: &WireOperation,
        now: UnixMillis,
    ) -> Result<(), BrokerProtocolError> {
        if operation
            .exact_tuple()
            .is_some_and(|tuple| tuple != (self.lobby_id, self.generation))
        {
            return Err(BrokerProtocolError::StaleFence);
        }
        if matches!(
            self.phase,
            ProtectedPhase::Released | ProtectedPhase::Quarantined
        ) || (operation.is_admission() && !self.phase.permits_admission())
            || (operation.is_admission() && now >= self.admission_play_deadline)
            || (operation.is_mutation() && now >= self.cleanup_deadline)
        {
            return Err(BrokerProtocolError::ForbiddenPhase);
        }
        Ok(())
    }
}

/// Concrete worker-side transport. Credential material cannot be supplied to
/// its constructor; only public TLS/fence files and a non-provider per-run MAC.
pub struct MtlsBrokerProviderTransport {
    address: String,
    server_name: String,
    tls: Arc<ClientConfig>,
    mac_key: Zeroizing<[u8; 32]>,
    fence: BrokerFence,
    lease: Mutex<LeaseSnapshot>,
    sequence: AtomicU64,
}

impl std::fmt::Debug for MtlsBrokerProviderTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MtlsBrokerProviderTransport")
            .field("address", &self.address)
            .field("server_name", &self.server_name)
            .field("fence", &self.fence)
            .field("mac_key", &"<redacted>")
            .finish()
    }
}

impl MtlsBrokerProviderTransport {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address: impl Into<String>,
        server_name: impl Into<String>,
        ca_path: impl AsRef<Path>,
        certificate_path: impl AsRef<Path>,
        private_key_path: impl AsRef<Path>,
        mac_key_path: impl AsRef<Path>,
        fence: BrokerFence,
        lease: LeaseSnapshot,
    ) -> Result<Self, BrokerProtocolError> {
        if lease.binding.lobby_id != fence.lobby_id
            || lease.binding.generation != fence.generation
            || lease.binding.supervisor_epoch != fence.supervisor_epoch
            || lease.binding.phase != fence.phase
        {
            return Err(BrokerProtocolError::StaleFence);
        }
        let tls = Arc::new(client_tls(ca_path, certificate_path, private_key_path)?);
        Ok(Self {
            address: address.into(),
            server_name: server_name.into(),
            tls,
            mac_key: read_key(mac_key_path)?,
            fence,
            lease: Mutex::new(lease),
            sequence: AtomicU64::new(0),
        })
    }

    async fn call(&self, operation: WireOperation) -> Result<WireResult, BrokerProtocolError> {
        let now = UnixMillis::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| BrokerProtocolError::Unavailable)?
                .as_millis()
                .try_into()
                .map_err(|_| BrokerProtocolError::Unavailable)?,
        );
        self.fence.validate_operation(&operation, now)?;
        let sequence = self
            .sequence
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let mut nonce = [0; 32];
        getrandom::getrandom(&mut nonce).map_err(|_| BrokerProtocolError::Unavailable)?;
        let mut lease = self.lease.lock().await;
        let mut request = WireRequest {
            protocol: PROTOCOL.to_owned(),
            run_id: self.fence.run_id.clone(),
            lobby_id: self.fence.lobby_id,
            generation: self.fence.generation,
            supervisor_epoch: lease.binding.supervisor_epoch,
            sequence,
            nonce,
            lease_uid: lease.uid.clone(),
            lease_resource_version: lease.resource_version.clone(),
            operation,
            mac: [0; 32],
        };
        request.mac = request_mac(&request, &self.mac_key)?;
        let stream = TcpStream::connect(&self.address)
            .await
            .map_err(|_| BrokerProtocolError::Unavailable)?;
        let name = ServerName::try_from(self.server_name.clone())
            .map_err(|_| BrokerProtocolError::TlsIdentity)?;
        let mut stream = TlsConnector::from(Arc::clone(&self.tls))
            .connect(name, stream)
            .await
            .map_err(|_| BrokerProtocolError::TlsIdentity)?;
        write_frame(&mut stream, &request).await?;
        let response: WireResponse = read_frame(&mut stream).await?;
        if response.sequence != sequence
            || response.nonce != nonce
            || response.lease_uid != lease.uid
            || response.mac != response_mac(&response, &self.mac_key)?
        {
            return Err(BrokerProtocolError::Authentication);
        }
        lease.resource_version = response.lease_resource_version.clone();
        lease.binding.supervisor_epoch = lease
            .binding
            .supervisor_epoch
            .saturating_add(u64::from(request.operation.is_mutation()));
        Ok(response.result)
    }
}

fn provider_error(_: BrokerProtocolError, operation: &'static str) -> ProviderError {
    ProviderError::Unavailable { operation }
}

#[async_trait]
impl BrokerProviderTransport for MtlsBrokerProviderTransport {
    fn cached_capabilities(&self) -> ProviderCapabilities {
        // Construction occurs only after signed receipt + Lease qualification;
        // operation failures still fail closed at the broker/provider boundary.
        ProviderCapabilities::available()
    }
    async fn prepare(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        match self
            .call(WireOperation::Prepare(request))
            .await
            .map_err(|e| provider_error(e, "broker_prepare"))?
        {
            WireResult::Prepared(value) => Ok(value),
            _ => Err(ProviderError::Unavailable {
                operation: "broker_prepare",
            }),
        }
    }
    async fn mint(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        match self
            .call(WireOperation::Mint(request))
            .await
            .map_err(|e| provider_error(e, "broker_mint"))?
        {
            WireResult::Minted {
                credential_id,
                mut auth_key,
                tailnet,
                metadata,
            } => {
                let protected = SecretString::new(std::mem::take(&mut auth_key));
                auth_key.zeroize();
                Ok(MintedCredential {
                    credential_id,
                    auth_key: protected,
                    tailnet,
                    metadata,
                })
            }
            _ => Err(ProviderError::Unavailable {
                operation: "broker_mint",
            }),
        }
    }
    async fn cleanup(&self, request: CleanupLobbyRequest) -> Result<CleanupOutcome, ProviderError> {
        match self
            .call(WireOperation::Cleanup(request))
            .await
            .map_err(|e| provider_error(e, "broker_cleanup"))?
        {
            WireResult::Cleanup(value) => Ok(value),
            _ => Err(ProviderError::Unavailable {
                operation: "broker_cleanup",
            }),
        }
    }
    async fn observe(
        &self,
        request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError> {
        match self
            .call(WireOperation::Observe(request))
            .await
            .map_err(|e| provider_error(e, "broker_observe"))?
        {
            WireResult::Observation(value) => Ok(value),
            _ => Err(ProviderError::Unavailable {
                operation: "broker_observe",
            }),
        }
    }
    async fn present(&self, request: TailnetPresenceRequest) -> Result<bool, ProviderError> {
        match self
            .call(WireOperation::Present(request))
            .await
            .map_err(|e| provider_error(e, "broker_present"))?
        {
            WireResult::Present(value) => Ok(value),
            _ => Err(ProviderError::Unavailable {
                operation: "broker_present",
            }),
        }
    }
    async fn erase(&self, request: TailnetPresenceRequest) -> Result<(), ProviderError> {
        match self
            .call(WireOperation::Erase(request))
            .await
            .map_err(|e| provider_error(e, "broker_erase"))?
        {
            WireResult::Erased => Ok(()),
            _ => Err(ProviderError::Unavailable {
                operation: "broker_erase",
            }),
        }
    }
}

/// Recovery transport exposes only cleanup operations at the Rust type level.
pub struct CleanupOnlyBrokerTransport(pub Arc<MtlsBrokerProviderTransport>);
impl CleanupOnlyBrokerTransport {
    pub async fn cleanup(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        self.0.cleanup(request).await
    }
    pub async fn present(&self, request: TailnetPresenceRequest) -> Result<bool, ProviderError> {
        self.0.present(request).await
    }
    pub async fn erase(&self, request: TailnetPresenceRequest) -> Result<(), ProviderError> {
        self.0.erase(request).await
    }
    pub async fn release_lease(&self) -> Result<(), ProviderError> {
        match self
            .0
            .call(WireOperation::Release)
            .await
            .map_err(|error| provider_error(error, "broker_lease_release"))?
        {
            WireResult::Released => Ok(()),
            _ => Err(ProviderError::Unavailable {
                operation: "broker_lease_release",
            }),
        }
    }
}

pub struct BrokerServer {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    mac_key: Arc<Zeroizing<[u8; 32]>>,
    fence: BrokerFence,
    lease: Arc<KubernetesLeaseAuthority>,
    provider: Arc<dyn NetworkProvider>,
    last_sequence: Arc<Mutex<u64>>,
}

impl BrokerServer {
    #[allow(clippy::too_many_arguments)]
    pub async fn bind(
        address: &str,
        ca_path: impl AsRef<Path>,
        certificate_path: impl AsRef<Path>,
        private_key_path: impl AsRef<Path>,
        mac_key_path: impl AsRef<Path>,
        fence: BrokerFence,
        lease: Arc<KubernetesLeaseAuthority>,
        provider: Arc<dyn NetworkProvider>,
    ) -> Result<Self, BrokerProtocolError> {
        Ok(Self {
            listener: TcpListener::bind(address)
                .await
                .map_err(|_| BrokerProtocolError::Unavailable)?,
            acceptor: TlsAcceptor::from(Arc::new(server_tls(
                ca_path,
                certificate_path,
                private_key_path,
            )?)),
            mac_key: Arc::new(read_key(mac_key_path)?),
            fence,
            lease,
            provider,
            last_sequence: Arc::new(Mutex::new(0)),
        })
    }

    pub async fn serve(self) -> Result<(), BrokerProtocolError> {
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| BrokerProtocolError::Unavailable)?;
            let acceptor = self.acceptor.clone();
            let key = Arc::clone(&self.mac_key);
            let fence = self.fence.clone();
            let lease = Arc::clone(&self.lease);
            let provider = Arc::clone(&self.provider);
            let sequence = Arc::clone(&self.last_sequence);
            tokio::spawn(async move {
                let _ =
                    handle_connection(stream, acceptor, &key, &fence, &lease, provider, &sequence)
                        .await;
            });
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    key: &[u8; 32],
    fence: &BrokerFence,
    lease_authority: &KubernetesLeaseAuthority,
    provider: Arc<dyn NetworkProvider>,
    last_sequence: &Mutex<u64>,
) -> Result<(), BrokerProtocolError> {
    let mut stream = acceptor
        .accept(stream)
        .await
        .map_err(|_| BrokerProtocolError::TlsIdentity)?;
    let request: WireRequest = read_frame(&mut stream).await?;
    if request.protocol != PROTOCOL
        || request.mac != request_mac(&request, key)?
        || request.run_id != fence.run_id
        || (request.lobby_id, request.generation) != (fence.lobby_id, fence.generation)
    {
        return Err(BrokerProtocolError::Authentication);
    }
    fence.validate_operation(&request.operation, now())?;
    let mut sequence = last_sequence.lock().await;
    if request.sequence != sequence.saturating_add(1) {
        return Err(BrokerProtocolError::Authentication);
    }
    let expected = lease_authority
        .read()
        .await
        .map_err(|_| BrokerProtocolError::StaleFence)?;
    if expected.uid != request.lease_uid
        || expected.resource_version != request.lease_resource_version
        || expected.binding.lobby_id != fence.lobby_id
        || expected.binding.generation != fence.generation
        || expected.binding.supervisor_epoch != request.supervisor_epoch
        || expected.binding.phase != fence.phase
    {
        return Err(BrokerProtocolError::StaleFence);
    }
    // Reserve the exact intent in external CAS authority before provider I/O.
    // If the provider result is ambiguous, the advanced Lease prevents replay
    // from an old PVC/request and recovery can only reconcile cleanup.
    let next = if request.operation.is_mutation() {
        let mut binding: LeaseBinding = expected.binding.clone();
        binding.supervisor_epoch = binding
            .supervisor_epoch
            .checked_add(1)
            .ok_or(BrokerProtocolError::StaleFence)?;
        let bytes = serde_json::to_vec(&(request.sequence, request.nonce, &request.operation))
            .map_err(|_| BrokerProtocolError::Malformed)?;
        binding.state_sha256 = Sha256::digest(bytes).into();
        if matches!(request.operation, WireOperation::Release) {
            binding.phase = ProtectedPhase::Released;
        }
        lease_authority
            .compare_and_swap(&expected, &binding)
            .await
            .map_err(|_| BrokerProtocolError::StaleFence)?
    } else {
        expected
    };
    let result = dispatch(provider, request.operation.clone()).await?;
    *sequence = request.sequence;
    let mut response = WireResponse {
        sequence: request.sequence,
        nonce: request.nonce,
        lease_uid: next.uid,
        lease_resource_version: next.resource_version,
        result,
        mac: [0; 32],
    };
    response.mac = response_mac(&response, key)?;
    write_frame(&mut stream, &response).await
}

async fn dispatch(
    provider: Arc<dyn NetworkProvider>,
    operation: WireOperation,
) -> Result<WireResult, BrokerProtocolError> {
    let result = match operation {
        WireOperation::Capabilities => WireResult::Capabilities(provider.cached_capabilities()),
        WireOperation::Prepare(value) => WireResult::Prepared(
            provider
                .prepare_lobby(value)
                .await
                .map_err(|_| BrokerProtocolError::Provider)?,
        ),
        WireOperation::Mint(value) => {
            let minted = provider
                .mint_credential(value)
                .await
                .map_err(|_| BrokerProtocolError::Provider)?;
            let mut key = minted.auth_key.into_zeroizing();
            let result = WireResult::Minted {
                credential_id: minted.credential_id,
                auth_key: std::mem::take(&mut *key),
                tailnet: minted.tailnet,
                metadata: minted.metadata,
            };
            key.zeroize();
            result
        }
        WireOperation::Cleanup(value) => WireResult::Cleanup(
            provider
                .cleanup_lobby(value)
                .await
                .map_err(|_| BrokerProtocolError::Provider)?,
        ),
        WireOperation::Observe(value) => WireResult::Observation(
            provider
                .observe_network(value)
                .await
                .map_err(|_| BrokerProtocolError::Provider)?,
        ),
        WireOperation::Present(value) => WireResult::Present(
            provider
                .tailnet_present(value)
                .await
                .map_err(|_| BrokerProtocolError::Provider)?,
        ),
        WireOperation::Erase(value) => {
            provider
                .erase_child_secret(value)
                .await
                .map_err(|_| BrokerProtocolError::Provider)?;
            WireResult::Erased
        }
        WireOperation::Release => WireResult::Released,
    };
    Ok(result)
}

fn now() -> UnixMillis {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_millis().try_into().unwrap_or(u64::MAX));
    UnixMillis::new(millis)
}

fn request_mac(request: &WireRequest, key: &[u8; 32]) -> Result<[u8; 32], BrokerProtocolError> {
    let mut unsigned = request.clone();
    unsigned.mac = [0; 32];
    mac(&unsigned, key)
}
fn response_mac(response: &WireResponse, key: &[u8; 32]) -> Result<[u8; 32], BrokerProtocolError> {
    let mut unsigned = response.clone();
    unsigned.mac = [0; 32];
    mac(&unsigned, key)
}
fn mac<T: Serialize>(value: &T, key: &[u8; 32]) -> Result<[u8; 32], BrokerProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| BrokerProtocolError::Malformed)?;
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| BrokerProtocolError::Authentication)?;
    mac.update(PROTOCOL.as_bytes());
    mac.update(&bytes);
    Ok(mac.finalize().into_bytes().into())
}

async fn write_frame<T: Serialize, S: AsyncWriteExt + Unpin>(
    stream: &mut S,
    value: &T,
) -> Result<(), BrokerProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| BrokerProtocolError::Malformed)?;
    if bytes.len() > MAX_FRAME {
        return Err(BrokerProtocolError::Malformed);
    }
    stream
        .write_u32(
            bytes
                .len()
                .try_into()
                .map_err(|_| BrokerProtocolError::Malformed)?,
        )
        .await
        .map_err(|_| BrokerProtocolError::Unavailable)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| BrokerProtocolError::Unavailable)
}
async fn read_frame<T: for<'de> Deserialize<'de>, S: AsyncReadExt + Unpin>(
    stream: &mut S,
) -> Result<T, BrokerProtocolError> {
    let length = stream
        .read_u32()
        .await
        .map_err(|_| BrokerProtocolError::Unavailable)? as usize;
    if length > MAX_FRAME {
        return Err(BrokerProtocolError::Malformed);
    }
    let mut bytes = vec![0; length];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut bytes))
        .await
        .map_err(|_| BrokerProtocolError::Unavailable)?
        .map_err(|_| BrokerProtocolError::Unavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| BrokerProtocolError::Malformed)
}

fn read_key(path: impl AsRef<Path>) -> Result<Zeroizing<[u8; 32]>, BrokerProtocolError> {
    let mut bytes =
        Zeroizing::new(std::fs::read(path).map_err(|_| BrokerProtocolError::Authentication)?);
    if bytes.len() != 32 {
        return Err(BrokerProtocolError::Authentication);
    }
    let mut key = Zeroizing::new([0; 32]);
    key.copy_from_slice(&bytes);
    bytes.zeroize();
    Ok(key)
}
fn certificates(
    path: impl AsRef<Path>,
) -> Result<Vec<CertificateDer<'static>>, BrokerProtocolError> {
    let bytes = std::fs::read(path).map_err(|_| BrokerProtocolError::TlsIdentity)?;
    rustls_pemfile::certs(&mut BufReader::new(bytes.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| BrokerProtocolError::TlsIdentity)
}
fn private_key(path: impl AsRef<Path>) -> Result<PrivateKeyDer<'static>, BrokerProtocolError> {
    let mut bytes =
        Zeroizing::new(std::fs::read(path).map_err(|_| BrokerProtocolError::TlsIdentity)?);
    let key = rustls_pemfile::private_key(&mut BufReader::new(bytes.as_slice()))
        .map_err(|_| BrokerProtocolError::TlsIdentity)?
        .ok_or(BrokerProtocolError::TlsIdentity)?;
    bytes.zeroize();
    Ok(key)
}
fn roots(path: impl AsRef<Path>) -> Result<RootCertStore, BrokerProtocolError> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(path)? {
        roots
            .add(certificate)
            .map_err(|_| BrokerProtocolError::TlsIdentity)?;
    }
    if roots.is_empty() {
        return Err(BrokerProtocolError::TlsIdentity);
    }
    Ok(roots)
}
fn client_tls(
    ca: impl AsRef<Path>,
    cert: impl AsRef<Path>,
    key: impl AsRef<Path>,
) -> Result<ClientConfig, BrokerProtocolError> {
    ClientConfig::builder()
        .with_root_certificates(roots(ca)?)
        .with_client_auth_cert(certificates(cert)?, private_key(key)?)
        .map_err(|_| BrokerProtocolError::TlsIdentity)
}
fn server_tls(
    ca: impl AsRef<Path>,
    cert: impl AsRef<Path>,
    key: impl AsRef<Path>,
) -> Result<ServerConfig, BrokerProtocolError> {
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots(ca)?))
        .build()
        .map_err(|_| BrokerProtocolError::TlsIdentity)?;
    ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates(cert)?, private_key(key)?)
        .map_err(|_| BrokerProtocolError::TlsIdentity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_binds_nonce_sequence_lease_and_exact_operation() {
        let lobby = LobbyId::parse("00000000-0000-4000-8000-0000000000aa").unwrap();
        let operation = WireOperation::Present(TailnetPresenceRequest {
            lobby_id: lobby,
            network_generation: 1,
            identity: crate::provider::ProviderNetworkIdentity {
                provider_tailnet_id: Some("stable-1".into()),
                tailnet_dns_name: spurfire_protocol::TailnetDnsName::parse("alpha.example.ts.net")
                    .unwrap(),
            },
        });
        let mut request = WireRequest {
            protocol: PROTOCOL.into(),
            run_id: "run-0123456789abcdef".into(),
            lobby_id: lobby,
            generation: 1,
            supervisor_epoch: 1,
            sequence: 1,
            nonce: [1; 32],
            lease_uid: "uid".into(),
            lease_resource_version: "7".into(),
            operation,
            mac: [0; 32],
        };
        request.mac = request_mac(&request, &[9; 32]).unwrap();
        let original = request.mac;
        request.sequence = 2;
        assert_ne!(request_mac(&request, &[9; 32]).unwrap(), original);
    }
    #[test]
    fn cleanup_transport_has_no_admission_trait() {
        fn accepts_cleanup(_: &CleanupOnlyBrokerTransport) {}
        let _ = accepts_cleanup;
        assert!(WireOperation::Prepare(PrepareLobbyRequest {
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-0000000000aa").unwrap(),
            network_generation: 1,
            mode: spurfire_protocol::ProvisioningMode::TailnetPerLobby,
            dry_run: false
        })
        .is_admission());
    }
}
