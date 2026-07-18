//! Lobby store abstraction, retention rules, and durable JSON implementation.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spurfire_protocol::{
    AuthorityElection, ConnectivitySample, JoinCredentialReceipt, Lobby, LobbyId, LobbyState,
    NetworkLifecycle, PlayerId, TailnetDnsName, UnixMillis,
};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::sync::RwLock;

/// Idempotency records and destroyed tombstones are retained for 24 hours.
pub const IDEMPOTENCY_RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;
/// Failed and expired debugging records are retained for 15 minutes.
pub const TERMINAL_DEBUG_RETENTION_MS: u64 = 15 * 60 * 1_000;
/// A starting lobby fails if the authority does not heartbeat within 120 seconds.
pub const START_TIMEOUT_MS: u64 = 120 * 1_000;
/// Hard process-level lobby quota for the prototype service.
pub const MAX_STORED_LOBBIES: usize = 10_000;
/// Hard process-level create-idempotency quota.
pub const MAX_CREATE_REPLAYS: usize = 20_000;
/// Creator inspection capability remains usable briefly while cleanup finishes.
pub const CREATOR_CAPABILITY_CLEANUP_GRACE_MS: u64 = 15 * 60 * 1_000;

const IDEMPOTENCY_DIGEST_DOMAIN: &[u8] = b"spurfire-real-lobby-lease-v1\0";

const fn default_network_generation() -> u64 {
    1
}

const fn default_network_lifecycle() -> NetworkLifecycle {
    NetworkLifecycle::ManualRemediation
}

/// Non-secret record proving that a credential has already been delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    /// Provider receipt identifier, never key material.
    pub credential_id: String,
    /// Absolute key expiration.
    pub expires_at: UnixMillis,
    /// Whether upstream revocation succeeded or expiry was confirmed.
    pub revoked: bool,
    /// Whether a revocation retry is pending.
    pub cleanup_pending: bool,
    /// Whether this credential was simulated.
    pub dry_run: bool,
}

impl StoredCredential {
    pub(crate) fn receipt(&self) -> JoinCredentialReceipt {
        JoinCredentialReceipt {
            credential_id: self.credential_id.clone(),
            expires_at: self.expires_at,
            one_use: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredJoinReplay {
    pub fingerprint: Vec<u8>,
    pub player_id: PlayerId,
    pub receipt: JoinCredentialReceipt,
    pub created_at: UnixMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredMutationReplay {
    pub fingerprint: Vec<u8>,
    pub actor: PlayerId,
    pub created_at: UnixMillis,
}

/// Durable reservation written before a key-mint request leaves the process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredIssuanceReservation {
    pub fingerprint: Vec<u8>,
    pub idempotency_key: String,
    pub created_at: UnixMillis,
    pub expires_at: UnixMillis,
}

/// One rate-limit event. It contains only a client-asserted UUID and timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredJoinAttempt {
    pub player_id: PlayerId,
    pub attempted_at: UnixMillis,
}

/// Durable, non-secret provider identity bound to one lobby generation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredNetworkIdentity {
    /// Exact stable provider ID. Dedicated mode requires this value.
    pub(crate) provider_tailnet_id: Option<String>,
    /// Canonical provider-returned tailnet DNS name/FQDN.
    pub(crate) tailnet_dns_name: TailnetDnsName,
    /// Generation to which both identity values belong.
    pub(crate) network_generation: u64,
    /// Trusted service time at which the typed create result was captured.
    pub(crate) captured_at: UnixMillis,
}

impl fmt::Debug for StoredNetworkIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredNetworkIdentity")
            .field("provider_tailnet_id", &"<operator-metadata>")
            .field("tailnet_dns_name", &"<topology-metadata>")
            .field("network_generation", &self.network_generation)
            .field("captured_at", &self.captured_at)
            .finish()
    }
}

/// Hash-only exact-lobby capability record. Plaintext is never persisted.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCapabilityVerifier {
    verifier: [u8; 32],
    lobby_id: LobbyId,
    network_generation: u64,
    expires_at: UnixMillis,
    revoked: bool,
}

impl StoredCapabilityVerifier {
    /// Constructs a verifier already hashed with the service's domain separator.
    #[must_use]
    pub fn new(
        verifier: [u8; 32],
        lobby_id: LobbyId,
        network_generation: u64,
        expires_at: UnixMillis,
    ) -> Self {
        Self {
            verifier,
            lobby_id,
            network_generation,
            expires_at,
            revoked: false,
        }
    }

    fn authorizes(
        &self,
        candidate: &[u8; 32],
        lobby_id: LobbyId,
        generation: u64,
        now: UnixMillis,
    ) -> bool {
        !self.revoked
            && self.lobby_id == lobby_id
            && self.network_generation == generation
            && now < self.expires_at
            && bool::from(self.verifier.ct_eq(candidate))
    }
}

impl fmt::Debug for StoredCapabilityVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCapabilityVerifier")
            .field("verifier", &"<sha256-verifier>")
            .field("lobby_id", &self.lobby_id)
            .field("network_generation", &self.network_generation)
            .field("expires_at", &self.expires_at)
            .field("revoked", &self.revoked)
            .finish()
    }
}

/// Complete lobby record. It never contains auth-key/OAuth material or device IDs.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredLobby {
    pub(crate) lobby: Lobby,
    pub(crate) creator_player_id: PlayerId,
    pub(crate) tailnet: String,
    pub(crate) tag: String,
    pub(crate) dry_run: bool,
    #[serde(default = "default_network_generation")]
    pub(crate) network_generation: u64,
    #[serde(default = "default_network_lifecycle")]
    pub(crate) network_lifecycle: NetworkLifecycle,
    #[serde(default)]
    pub(crate) network_identity: Option<StoredNetworkIdentity>,
    #[serde(default)]
    pub(crate) cleanup_requested_at: Option<UnixMillis>,
    #[serde(default)]
    pub(crate) delete_acknowledged_at: Option<UnixMillis>,
    #[serde(default)]
    pub(crate) child_secret_erased_at: Option<UnixMillis>,
    #[serde(default)]
    pub(crate) first_absence_observed_at: Option<UnixMillis>,
    #[serde(default)]
    pub(crate) absence_confirmed_at: Option<UnixMillis>,
    #[serde(default)]
    pub(crate) creator_capability: Option<StoredCapabilityVerifier>,
    pub(crate) idle_ttl_ms: u64,
    pub(crate) measurements: BTreeMap<PlayerId, ConnectivitySample>,
    pub(crate) credentials: BTreeMap<PlayerId, StoredCredential>,
    pub(crate) join_replays: BTreeMap<String, StoredJoinReplay>,
    pub(crate) start_replays: BTreeMap<String, StoredMutationReplay>,
    pub(crate) results_replays: BTreeMap<String, StoredMutationReplay>,
    pub(crate) pending_issuances: BTreeMap<PlayerId, StoredIssuanceReservation>,
    pub(crate) join_attempts: VecDeque<StoredJoinAttempt>,
    pub(crate) cleanup_pending: bool,
    pub(crate) last_election: Option<AuthorityElection>,
    pub(crate) started_at: Option<UnixMillis>,
    pub(crate) last_authority_heartbeat_at: Option<UnixMillis>,
    pub(crate) terminal_at: Option<UnixMillis>,
}

impl StoredLobby {
    /// Creates a record around a validated public snapshot and non-secret provider metadata.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        lobby: Lobby,
        creator_player_id: PlayerId,
        tailnet: impl Into<String>,
        tag: impl Into<String>,
        dry_run: bool,
        idle_ttl_ms: u64,
    ) -> Self {
        Self {
            lobby,
            creator_player_id,
            tailnet: tailnet.into(),
            tag: tag.into(),
            dry_run,
            network_generation: 1,
            network_lifecycle: if dry_run {
                NetworkLifecycle::Simulated
            } else {
                NetworkLifecycle::Reserved
            },
            network_identity: None,
            cleanup_requested_at: None,
            delete_acknowledged_at: None,
            child_secret_erased_at: None,
            first_absence_observed_at: None,
            absence_confirmed_at: None,
            creator_capability: None,
            idle_ttl_ms,
            measurements: BTreeMap::new(),
            credentials: BTreeMap::new(),
            join_replays: BTreeMap::new(),
            start_replays: BTreeMap::new(),
            results_replays: BTreeMap::new(),
            pending_issuances: BTreeMap::new(),
            join_attempts: VecDeque::new(),
            cleanup_pending: false,
            last_election: None,
            started_at: None,
            last_authority_heartbeat_at: None,
            terminal_at: None,
        }
    }

    /// Installs a hash-only creator capability before the atomic create.
    #[must_use]
    pub fn with_creator_capability(mut self, capability: StoredCapabilityVerifier) -> Self {
        self.creator_capability = Some(capability);
        self
    }

    /// Revokes the creator capability without retaining replacement plaintext.
    pub fn revoke_creator_capability(&mut self) {
        if let Some(capability) = &mut self.creator_capability {
            capability.revoked = true;
        }
    }

    /// Returns a cloned public snapshot with no provider-only state.
    #[must_use]
    pub fn snapshot(&self) -> Lobby {
        let mut lobby = self.lobby.clone();
        lobby.cleanup_pending = self.cleanup_pending;
        lobby
    }

    /// Whether this lobby can make no real network mutations.
    #[must_use]
    pub const fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}

impl fmt::Debug for StoredLobby {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredLobby")
            .field("lobby", &self.lobby)
            .field("creator_player_id", &self.creator_player_id)
            .field("tailnet", &"<configured>")
            .field("tag", &self.tag)
            .field("dry_run", &self.dry_run)
            .field("network_generation", &self.network_generation)
            .field("network_lifecycle", &self.network_lifecycle)
            .field("network_identity_present", &self.network_identity.is_some())
            .field("creator_capability", &self.creator_capability)
            .field("measurement_count", &self.measurements.len())
            .field("credential_receipt_count", &self.credentials.len())
            .field("pending_issuance_count", &self.pending_issuances.len())
            .field("cleanup_pending", &self.cleanup_pending)
            .finish()
    }
}

/// Atomic result of create-idempotency evaluation.
#[derive(Clone, Debug)]
pub enum CreateStoreOutcome {
    /// A new lobby record and idempotency entry were inserted.
    Created(StoredLobby),
    /// An identical request key and actor replayed the original lobby.
    Replay(StoredLobby),
    /// The key was already attached to a different request body or actor.
    Conflict,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CreateReplay {
    fingerprint: Vec<u8>,
    lobby_id: LobbyId,
    actor: PlayerId,
    created_at: UnixMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RealLobbyLease {
    holder_lobby_id: LobbyId,
    network_generation: u64,
    idempotency_digest: [u8; 32],
    acquired_at: UnixMillis,
    lifecycle: NetworkLifecycle,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    lobbies: BTreeMap<LobbyId, StoredLobby>,
    #[serde(default)]
    create_replays: BTreeMap<String, CreateReplay>,
    #[serde(default)]
    real_lobby_lease: Option<RealLobbyLease>,
    #[serde(default)]
    real_creation_quarantined: bool,
}

/// Store failures that indicate an internal consistency or persistence error.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StoreError {
    /// UUID generation collided with an existing record.
    #[error("lobby identifier already exists")]
    DuplicateLobby,
    /// A record disappeared before replacement.
    #[error("lobby record no longer exists")]
    LobbyNotFound,
    /// An idempotency record referenced a missing lobby.
    #[error("idempotency record is inconsistent")]
    InconsistentIdempotency,
    /// A configured process quota was reached.
    #[error("lobby store capacity reached")]
    Capacity,
    /// The independent real-mutation kill switch denied a new reservation.
    #[error("real provider mutations are disabled")]
    RealMutationsDisabled,
    /// The singleton real-lobby lease is held or quarantined.
    #[error("real lobby capacity reached")]
    RealLobbyCapacityReached,
    /// Durable state could not be read or replaced.
    #[error("durable lobby state I/O failed")]
    Io,
    /// Durable state was not valid Spurfire JSON.
    #[error("durable lobby state is invalid")]
    Decode,
}

/// Persistence boundary used by the HTTP service.
#[async_trait]
pub trait LobbyStore: Send + Sync {
    /// Atomically inserts a lobby or resolves its create idempotency key.
    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        actor: PlayerId,
        now: UnixMillis,
        lobby: StoredLobby,
        allow_new_real: bool,
    ) -> Result<CreateStoreOutcome, StoreError>;

    /// Reads one complete non-secret record.
    async fn get(&self, lobby_id: LobbyId) -> Option<StoredLobby>;

    /// Performs exact-lobby, generation-bound, constant-time capability lookup.
    /// Missing, expired, revoked, wrong-scope, and wrong-lobby candidates all
    /// return `None` without an existence distinction.
    async fn get_authorized_network_view(
        &self,
        lobby_id: LobbyId,
        verifier: [u8; 32],
        now: UnixMillis,
    ) -> Option<StoredLobby>;

    /// Replaces one record after a per-lobby serialized service mutation.
    async fn replace(&self, lobby: StoredLobby) -> Result<(), StoreError>;

    /// Applies deterministic TTL/start-timeout transitions, retention eviction,
    /// and returns IDs needing teardown or retry in sorted order.
    async fn cleanup_expired(&self, now: UnixMillis) -> Result<Vec<LobbyId>, StoreError>;
}

/// Process-local store backed by an `Arc<RwLock<...>>`.
#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<RwLock<StoreData>>,
}

impl InMemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of retained lobby records.
    pub async fn len(&self) -> usize {
        self.inner.read().await.lobbies.len()
    }

    /// Whether no lobby records are retained.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Whether fail-closed real capacity is currently held.
    pub async fn real_lobby_lease_held(&self) -> bool {
        self.inner.read().await.real_lobby_lease.is_some()
    }
}

impl fmt::Debug for InMemoryStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemoryStore")
            .field("inner", &"Arc<RwLock<StoreData>>")
            .finish()
    }
}

#[async_trait]
impl LobbyStore for InMemoryStore {
    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        actor: PlayerId,
        now: UnixMillis,
        lobby: StoredLobby,
        allow_new_real: bool,
    ) -> Result<CreateStoreOutcome, StoreError> {
        let mut data = self.inner.write().await;
        create_in_data(
            &mut data,
            idempotency_key,
            fingerprint,
            actor,
            now,
            lobby,
            allow_new_real,
        )
    }

    async fn get(&self, lobby_id: LobbyId) -> Option<StoredLobby> {
        self.inner.read().await.lobbies.get(&lobby_id).cloned()
    }

    async fn get_authorized_network_view(
        &self,
        lobby_id: LobbyId,
        verifier: [u8; 32],
        now: UnixMillis,
    ) -> Option<StoredLobby> {
        let data = self.inner.read().await;
        authorized_network_view(&data, lobby_id, &verifier, now)
    }

    async fn replace(&self, lobby: StoredLobby) -> Result<(), StoreError> {
        let mut data = self.inner.write().await;
        replace_in_data(&mut data, lobby)
    }

    async fn cleanup_expired(&self, now: UnixMillis) -> Result<Vec<LobbyId>, StoreError> {
        let mut data = self.inner.write().await;
        Ok(cleanup_in_data(&mut data, now))
    }
}

/// Durable, non-secret JSON store used by real mode.
#[derive(Clone)]
pub struct JsonFileStore {
    path: Arc<PathBuf>,
    inner: Arc<RwLock<StoreData>>,
}

impl JsonFileStore {
    /// Opens existing state or creates an empty in-memory image when absent.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let (mut data, existed) = match tokio::fs::read(&path).await {
            Ok(bytes) => (
                serde_json::from_slice(&bytes).map_err(|_| StoreError::Decode)?,
                true,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (StoreData::default(), false)
            }
            Err(_) => return Err(StoreError::Io),
        };
        let normalized = normalize_loaded_store(&mut data);
        if existed && normalized {
            persist_data(&path, &data).await?;
        }
        Ok(Self {
            path: Arc::new(path),
            inner: Arc::new(RwLock::new(data)),
        })
    }

    /// Number of retained records, useful for operational tests.
    pub async fn len(&self) -> usize {
        self.inner.read().await.lobbies.len()
    }

    /// Whether no durable lobby records are retained.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Whether the durable fail-closed real capacity lease is held.
    pub async fn real_lobby_lease_held(&self) -> bool {
        self.inner.read().await.real_lobby_lease.is_some()
    }

    async fn commit(&self, next: &StoreData) -> Result<(), StoreError> {
        persist_data(&self.path, next).await
    }
}

impl fmt::Debug for JsonFileStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonFileStore")
            .field("path", &self.path)
            .field("inner", &"Arc<RwLock<StoreData>>")
            .finish()
    }
}

#[async_trait]
impl LobbyStore for JsonFileStore {
    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        actor: PlayerId,
        now: UnixMillis,
        lobby: StoredLobby,
        allow_new_real: bool,
    ) -> Result<CreateStoreOutcome, StoreError> {
        let mut data = self.inner.write().await;
        let mut next = data.clone();
        let outcome = create_in_data(
            &mut next,
            idempotency_key,
            fingerprint,
            actor,
            now,
            lobby,
            allow_new_real,
        )?;
        self.commit(&next).await?;
        *data = next;
        Ok(outcome)
    }

    async fn get(&self, lobby_id: LobbyId) -> Option<StoredLobby> {
        self.inner.read().await.lobbies.get(&lobby_id).cloned()
    }

    async fn get_authorized_network_view(
        &self,
        lobby_id: LobbyId,
        verifier: [u8; 32],
        now: UnixMillis,
    ) -> Option<StoredLobby> {
        let data = self.inner.read().await;
        authorized_network_view(&data, lobby_id, &verifier, now)
    }

    async fn replace(&self, lobby: StoredLobby) -> Result<(), StoreError> {
        let mut data = self.inner.write().await;
        let mut next = data.clone();
        replace_in_data(&mut next, lobby)?;
        self.commit(&next).await?;
        *data = next;
        Ok(())
    }

    async fn cleanup_expired(&self, now: UnixMillis) -> Result<Vec<LobbyId>, StoreError> {
        let mut data = self.inner.write().await;
        let mut next = data.clone();
        let ids = cleanup_in_data(&mut next, now);
        if next != *data {
            self.commit(&next).await?;
            *data = next;
        }
        Ok(ids)
    }
}

impl PartialEq for StoreData {
    fn eq(&self, other: &Self) -> bool {
        // Stable JSON equality avoids deriving equality through every internal
        // election detail while still suppressing no-op durable rewrites.
        serde_json::to_vec(self).ok() == serde_json::to_vec(other).ok()
    }
}

fn create_in_data(
    data: &mut StoreData,
    idempotency_key: String,
    fingerprint: Vec<u8>,
    actor: PlayerId,
    now: UnixMillis,
    lobby: StoredLobby,
    allow_new_real: bool,
) -> Result<CreateStoreOutcome, StoreError> {
    purge_retained(data, now);
    // Replay resolution deliberately precedes both the kill switch and the
    // singleton lease so an accepted request never performs a second create.
    if let Some(replay) = data.create_replays.get(&idempotency_key) {
        if replay.fingerprint != fingerprint || replay.actor != actor {
            return Ok(CreateStoreOutcome::Conflict);
        }
        return data
            .lobbies
            .get(&replay.lobby_id)
            .cloned()
            .map(CreateStoreOutcome::Replay)
            .ok_or(StoreError::InconsistentIdempotency);
    }
    if data.lobbies.len() >= MAX_STORED_LOBBIES || data.create_replays.len() >= MAX_CREATE_REPLAYS {
        return Err(StoreError::Capacity);
    }
    if data.lobbies.contains_key(&lobby.lobby.lobby_id) {
        return Err(StoreError::DuplicateLobby);
    }

    if !lobby.dry_run {
        if !allow_new_real {
            return Err(StoreError::RealMutationsDisabled);
        }
        if data.real_creation_quarantined || data.real_lobby_lease.is_some() {
            return Err(StoreError::RealLobbyCapacityReached);
        }
        data.real_lobby_lease = Some(RealLobbyLease {
            holder_lobby_id: lobby.lobby.lobby_id,
            network_generation: lobby.network_generation,
            idempotency_digest: idempotency_digest(&fingerprint),
            acquired_at: now,
            lifecycle: lobby.network_lifecycle,
        });
    }

    data.create_replays.insert(
        idempotency_key,
        CreateReplay {
            fingerprint,
            lobby_id: lobby.lobby.lobby_id,
            actor,
            created_at: now,
        },
    );
    data.lobbies.insert(lobby.lobby.lobby_id, lobby.clone());
    Ok(CreateStoreOutcome::Created(lobby))
}

fn replace_in_data(data: &mut StoreData, lobby: StoredLobby) -> Result<(), StoreError> {
    let lobby_id = lobby.lobby.lobby_id;
    let Some(slot) = data.lobbies.get_mut(&lobby_id) else {
        return Err(StoreError::LobbyNotFound);
    };
    *slot = lobby.clone();

    let release = matches!(
        lobby.network_lifecycle,
        NetworkLifecycle::CreateRejected
            | NetworkLifecycle::DedicatedAbsent
            | NetworkLifecycle::SharedResourcesClean
    );
    if let Some(lease) = data
        .real_lobby_lease
        .as_mut()
        .filter(|lease| lease.holder_lobby_id == lobby_id)
    {
        if lease.network_generation != lobby.network_generation {
            data.real_creation_quarantined = true;
            return Ok(());
        }
        lease.lifecycle = lobby.network_lifecycle;
        if release {
            data.real_lobby_lease = None;
        }
    } else if !lobby.dry_run && !release {
        // A retained real record without its exact lease is restart ambiguity.
        data.real_creation_quarantined = true;
    }
    Ok(())
}

fn authorized_network_view(
    data: &StoreData,
    lobby_id: LobbyId,
    verifier: &[u8; 32],
    now: UnixMillis,
) -> Option<StoredLobby> {
    let stored = data.lobbies.get(&lobby_id)?;
    stored.creator_capability.as_ref().filter(|capability| {
        capability.authorizes(verifier, lobby_id, stored.network_generation, now)
    })?;
    Some(stored.clone())
}

fn idempotency_digest(fingerprint: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(IDEMPOTENCY_DIGEST_DOMAIN);
    hasher.update(fingerprint);
    hasher.finalize().into()
}

fn lease_release_lifecycle(lifecycle: NetworkLifecycle) -> bool {
    matches!(
        lifecycle,
        NetworkLifecycle::CreateRejected
            | NetworkLifecycle::DedicatedAbsent
            | NetworkLifecycle::SharedResourcesClean
    )
}

/// Migrates old real records into a fail-closed singleton lease. A missing or
/// mismatched lease never becomes free capacity after restart.
fn normalize_loaded_store(data: &mut StoreData) -> bool {
    let mut changed = false;
    let active_real: Vec<LobbyId> = data
        .lobbies
        .iter_mut()
        .filter_map(|(lobby_id, stored)| {
            if stored.dry_run || lease_release_lifecycle(stored.network_lifecycle) {
                return None;
            }
            if stored.network_generation == 0 {
                stored.network_generation = 1;
                changed = true;
            }
            if stored.network_lifecycle == NetworkLifecycle::Simulated {
                stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
                stored.cleanup_pending = true;
                changed = true;
            }
            Some(*lobby_id)
        })
        .collect();

    match (&data.real_lobby_lease, active_real.as_slice()) {
        (None, []) => {}
        (None, [holder]) => {
            let stored = data.lobbies.get(holder).expect("collected from lobbies");
            data.real_lobby_lease = Some(RealLobbyLease {
                holder_lobby_id: *holder,
                network_generation: stored.network_generation,
                idempotency_digest: [0; 32],
                acquired_at: stored.lobby.created_at,
                lifecycle: stored.network_lifecycle,
            });
            changed = true;
        }
        (None, holders) => {
            let holder = holders[0];
            let stored = data.lobbies.get(&holder).expect("collected from lobbies");
            data.real_lobby_lease = Some(RealLobbyLease {
                holder_lobby_id: holder,
                network_generation: stored.network_generation,
                idempotency_digest: [0; 32],
                acquired_at: stored.lobby.created_at,
                lifecycle: NetworkLifecycle::ManualRemediation,
            });
            data.real_creation_quarantined = true;
            changed = true;
        }
        (Some(lease), holders)
            if holders.contains(&lease.holder_lobby_id)
                && data
                    .lobbies
                    .get(&lease.holder_lobby_id)
                    .is_some_and(|stored| {
                        stored.network_generation == lease.network_generation
                    }) => {}
        (Some(_), _) => {
            data.real_creation_quarantined = true;
            changed = true;
        }
    }
    changed
}

fn cleanup_in_data(data: &mut StoreData, now: UnixMillis) -> Vec<LobbyId> {
    purge_retained(data, now);
    let mut cleanup = Vec::new();
    for (lobby_id, record) in &mut data.lobbies {
        let transitioned = apply_time_transitions(record, now);
        if transitioned
            || matches!(record.lobby.state, LobbyState::Closing)
            || (record.cleanup_pending
                && matches!(
                    record.lobby.state,
                    LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
                ))
        {
            cleanup.push(*lobby_id);
        }
    }
    cleanup
}

/// Applies deadline-driven edges through the declared protocol transition table.
pub(crate) fn apply_time_transitions(record: &mut StoredLobby, now: UnixMillis) -> bool {
    let absolute_due = now >= record.lobby.ttl.absolute_expires_at;
    let idle_due = now >= record.lobby.ttl.idle_expires_at;
    let start_due = record
        .started_at
        .and_then(|started| now.checked_duration_since(started))
        .is_some_and(|age| age >= START_TIMEOUT_MS);
    let target = match record.lobby.state {
        LobbyState::Provisioning if absolute_due => {
            Some((LobbyState::Failed, Some("absolute_ttl_expired".to_owned())))
        }
        LobbyState::Forming | LobbyState::Ready if absolute_due || idle_due => {
            Some((LobbyState::Expired, None))
        }
        LobbyState::Starting if absolute_due => {
            Some((LobbyState::Failed, Some("absolute_ttl_expired".to_owned())))
        }
        LobbyState::Starting if start_due => {
            Some((LobbyState::Failed, Some("start_timeout".to_owned())))
        }
        LobbyState::InMatch if absolute_due => {
            Some((LobbyState::Failed, Some("absolute_ttl_expired".to_owned())))
        }
        _ => None,
    };
    let Some((next, reason)) = target else {
        return false;
    };
    if record.lobby.state.validate_transition(next).is_err() {
        return false;
    }
    record.lobby.state = next;
    record.lobby.state_reason = reason;
    record.lobby.authority = None;
    record.last_election = None;
    record.terminal_at = Some(now);
    record.cleanup_pending = true;
    true
}

fn purge_retained(data: &mut StoreData, now: UnixMillis) {
    for record in data.lobbies.values_mut() {
        record
            .join_replays
            .retain(|_, replay| !age_at_least(now, replay.created_at, IDEMPOTENCY_RETENTION_MS));
        record
            .start_replays
            .retain(|_, replay| !age_at_least(now, replay.created_at, IDEMPOTENCY_RETENTION_MS));
        record
            .results_replays
            .retain(|_, replay| !age_at_least(now, replay.created_at, IDEMPOTENCY_RETENTION_MS));
        record
            .pending_issuances
            .retain(|_, reservation| reservation.expires_at > now);
        while record
            .join_attempts
            .front()
            .is_some_and(|attempt| age_at_least(now, attempt.attempted_at, 60_000))
        {
            record.join_attempts.pop_front();
        }
    }

    let removed: Vec<LobbyId> = data
        .lobbies
        .iter()
        .filter_map(|(id, record)| {
            if data
                .real_lobby_lease
                .as_ref()
                .is_some_and(|lease| lease.holder_lobby_id == *id)
            {
                return None;
            }
            let terminal_at = record.terminal_at?;
            let retention = match record.lobby.state {
                LobbyState::Failed | LobbyState::Expired => TERMINAL_DEBUG_RETENTION_MS,
                LobbyState::Destroyed => IDEMPOTENCY_RETENTION_MS,
                _ => return None,
            };
            age_at_least(now, terminal_at, retention).then_some(*id)
        })
        .collect();
    for lobby_id in removed {
        data.lobbies.remove(&lobby_id);
    }
    data.create_replays.retain(|_, replay| {
        !age_at_least(now, replay.created_at, IDEMPOTENCY_RETENTION_MS)
            && data.lobbies.contains_key(&replay.lobby_id)
    });
}

fn age_at_least(now: UnixMillis, earlier: UnixMillis, duration: u64) -> bool {
    now.checked_duration_since(earlier)
        .is_some_and(|age| age >= duration)
}

async fn persist_data(path: &Path, data: &StoreData) -> Result<(), StoreError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| StoreError::Io)?;
    }
    let bytes = serde_json::to_vec(data).map_err(|_| StoreError::Decode)?;
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|_| StoreError::Io)?;
    #[cfg(windows)]
    if tokio::fs::try_exists(path)
        .await
        .map_err(|_| StoreError::Io)?
    {
        // Windows rename does not replace an existing destination. This keeps
        // the store functional there; production multi-process durability
        // still requires a transactional database.
        tokio::fs::remove_file(path)
            .await
            .map_err(|_| StoreError::Io)?;
    }
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|_| StoreError::Io)
}

#[cfg(test)]
mod tests {
    use spurfire_protocol::{LobbyTtl, ProvisioningMode, WIRE_VERSION};

    use super::*;

    fn player_id() -> PlayerId {
        PlayerId::parse("00000000-0000-4000-8000-000000000002").unwrap()
    }

    fn lobby() -> Lobby {
        Lobby {
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
            display_name: "Expiry".to_owned(),
            state: LobbyState::Forming,
            state_reason: None,
            roster: Vec::new(),
            max_players: 8,
            map_seed: None,
            authority: None,
            ttl: LobbyTtl {
                idle_expires_at: UnixMillis::new(1_000),
                absolute_expires_at: UnixMillis::new(2_000),
            },
            wire_version: WIRE_VERSION,
            provisioning_mode: ProvisioningMode::DryRun,
            created_at: UnixMillis::new(0),
            cleanup_pending: false,
        }
    }

    fn record() -> StoredLobby {
        StoredLobby::new(
            lobby(),
            player_id(),
            "dry-run.invalid",
            "tag:test",
            true,
            1_000,
        )
    }

    #[tokio::test]
    async fn cleanup_uses_caller_supplied_time() {
        let store = InMemoryStore::new();
        store
            .create(
                "create-1".to_owned(),
                b"body".to_vec(),
                player_id(),
                UnixMillis::new(0),
                record(),
                false,
            )
            .await
            .unwrap();

        assert!(store
            .cleanup_expired(UnixMillis::new(999))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store.cleanup_expired(UnixMillis::new(1_000)).await.unwrap(),
            vec![LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap()]
        );
        assert_eq!(
            store
                .get(LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap())
                .await
                .unwrap()
                .snapshot()
                .state,
            LobbyState::Expired
        );
    }

    #[tokio::test]
    async fn durable_store_survives_reopen_without_secret_material() {
        const CHILD_SECRET_CANARY: &str = "child-oauth-secret-must-not-persist";
        let child_credentials = spurfire_control::ChildOAuthCredentials::new(
            "child-oauth-id-must-not-persist",
            CHILD_SECRET_CANARY,
        );
        assert!(!format!("{child_credentials:?}").contains(CHILD_SECRET_CANARY));
        drop(child_credentials);
        let path =
            std::env::temp_dir().join(format!("spurfire-store-test-{}.json", std::process::id()));
        let _ = tokio::fs::remove_file(&path).await;
        let store = JsonFileStore::open(&path).await.unwrap();
        store
            .create(
                "durable-create".to_owned(),
                b"body".to_vec(),
                player_id(),
                UnixMillis::new(0),
                record(),
                false,
            )
            .await
            .unwrap();
        drop(store);
        let reopened = JsonFileStore::open(&path).await.unwrap();
        assert_eq!(reopened.len().await, 1);
        let encoded = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!encoded.contains("auth_key"));
        assert!(!encoded.contains("child_oauth"));
        assert!(!encoded.contains(CHILD_SECRET_CANARY));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[test]
    fn starting_timeout_is_a_valid_failed_edge() {
        let mut stored = record();
        stored.lobby.state = LobbyState::Starting;
        stored.lobby.ttl.absolute_expires_at = UnixMillis::new(1_000_000);
        stored.started_at = Some(UnixMillis::new(1_000));
        assert!(apply_time_transitions(
            &mut stored,
            UnixMillis::new(1_000 + START_TIMEOUT_MS)
        ));
        assert_eq!(stored.lobby.state, LobbyState::Failed);
        assert_eq!(stored.lobby.state_reason.as_deref(), Some("start_timeout"));
    }
}
