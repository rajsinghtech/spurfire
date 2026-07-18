//! Lobby store abstraction, retention rules, and durable JSON implementation.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use spurfire_protocol::{
    AuthorityElection, ConnectivitySample, InputHash, JoinCredentialReceipt, Lobby,
    LobbyCapabilityScope, LobbyId, LobbyState, NetworkLifecycle, PlayerId, TailnetDnsName,
    UnixMillis,
};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, sync::RwLock};

use crate::crypto::{constant_time_eq, sha256};

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
const CLEANUP_ABSENCE_MIN_SEPARATION_MS: u64 = 5_000;

const fn default_network_generation() -> u64 {
    1
}

const fn default_roster_revision() -> u64 {
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
    scopes: Vec<LobbyCapabilityScope>,
    lobby_id: LobbyId,
    #[serde(default)]
    player_id: Option<PlayerId>,
    network_generation: u64,
    expires_at: UnixMillis,
    #[serde(default)]
    revoked: bool,
    #[serde(default)]
    consumed_at: Option<UnixMillis>,
}

impl StoredCapabilityVerifier {
    /// Constructs a generation-bound hash-only capability.
    #[must_use]
    pub fn new(
        verifier: [u8; 32],
        scopes: Vec<LobbyCapabilityScope>,
        lobby_id: LobbyId,
        player_id: Option<PlayerId>,
        network_generation: u64,
        expires_at: UnixMillis,
    ) -> Self {
        let mut scopes = scopes;
        scopes.sort_unstable();
        scopes.dedup();
        Self {
            verifier,
            scopes,
            lobby_id,
            player_id,
            network_generation,
            expires_at,
            revoked: false,
            consumed_at: None,
        }
    }

    pub(crate) fn authorizes(
        &self,
        candidate: &[u8; 32],
        lobby_id: LobbyId,
        generation: u64,
        required: LobbyCapabilityScope,
        expected_player: Option<PlayerId>,
        now: UnixMillis,
    ) -> bool {
        !self.revoked
            && self.consumed_at.is_none()
            && self.scopes.contains(&required)
            && self.lobby_id == lobby_id
            && self.network_generation == generation
            && expected_player.is_none_or(|player| self.player_id == Some(player))
            && now < self.expires_at
            && constant_time_eq(&self.verifier, candidate)
    }

    pub(crate) fn consume(
        &mut self,
        candidate: &[u8; 32],
        lobby_id: LobbyId,
        generation: u64,
        required: LobbyCapabilityScope,
        now: UnixMillis,
    ) -> bool {
        if !self.authorizes(candidate, lobby_id, generation, required, None, now) {
            return false;
        }
        self.consumed_at = Some(now);
        true
    }

    pub(crate) const fn player_id(&self) -> Option<PlayerId> {
        self.player_id
    }
}

impl fmt::Debug for StoredCapabilityVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCapabilityVerifier")
            .field("verifier", &"<sha256-verifier>")
            .field("scopes", &self.scopes)
            .field("lobby_id", &self.lobby_id)
            .field("player_id", &self.player_id)
            .field("network_generation", &self.network_generation)
            .field("expires_at", &self.expires_at)
            .field("revoked", &self.revoked)
            .field("consumed", &self.consumed_at.is_some())
            .finish()
    }
}

/// Durable receipt of one accepted authority heartbeat. It is authoritative
/// only as a service receipt event, never as current gameplay truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAcceptedHeartbeat {
    pub(crate) player_id: PlayerId,
    pub(crate) epoch: u64,
    pub(crate) input_hash: InputHash,
    pub(crate) received_at: UnixMillis,
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
    #[serde(default = "default_roster_revision")]
    pub(crate) roster_revision: u64,
    #[serde(default)]
    pub(crate) session_generation: u64,
    #[serde(default = "default_network_lifecycle")]
    pub(crate) network_lifecycle: NetworkLifecycle,
    #[serde(default)]
    pub(crate) network_identity: Option<StoredNetworkIdentity>,
    /// SHA-256 over normalized generated child-policy semantics; never a provider body.
    #[serde(default)]
    pub(crate) child_policy_digest: Option<String>,
    /// Coarse policy gate status (`verified`, `mismatch`, `denied`, or `unavailable`).
    #[serde(default)]
    pub(crate) child_policy_status: Option<String>,
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
    pub(crate) capabilities: Vec<StoredCapabilityVerifier>,
    pub(crate) idle_ttl_ms: u64,
    pub(crate) measurements: BTreeMap<PlayerId, ConnectivitySample>,
    #[serde(default)]
    pub(crate) authenticated_reporters: BTreeSet<PlayerId>,
    pub(crate) credentials: BTreeMap<PlayerId, StoredCredential>,
    pub(crate) join_replays: BTreeMap<String, StoredJoinReplay>,
    pub(crate) start_replays: BTreeMap<String, StoredMutationReplay>,
    pub(crate) results_replays: BTreeMap<String, StoredMutationReplay>,
    pub(crate) pending_issuances: BTreeMap<PlayerId, StoredIssuanceReservation>,
    pub(crate) join_attempts: VecDeque<StoredJoinAttempt>,
    pub(crate) cleanup_pending: bool,
    pub(crate) last_election: Option<AuthorityElection>,
    #[serde(default)]
    pub(crate) authority_epoch: u64,
    pub(crate) started_at: Option<UnixMillis>,
    pub(crate) last_authority_heartbeat_at: Option<UnixMillis>,
    #[serde(default)]
    pub(crate) last_accepted_heartbeat: Option<StoredAcceptedHeartbeat>,
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
            roster_revision: 1,
            session_generation: 0,
            network_lifecycle: if dry_run {
                NetworkLifecycle::Simulated
            } else {
                NetworkLifecycle::Reserved
            },
            network_identity: None,
            child_policy_digest: None,
            child_policy_status: None,
            cleanup_requested_at: None,
            delete_acknowledged_at: None,
            child_secret_erased_at: None,
            first_absence_observed_at: None,
            absence_confirmed_at: None,
            capabilities: Vec::new(),
            idle_ttl_ms,
            measurements: BTreeMap::new(),
            authenticated_reporters: BTreeSet::new(),
            credentials: BTreeMap::new(),
            join_replays: BTreeMap::new(),
            start_replays: BTreeMap::new(),
            results_replays: BTreeMap::new(),
            pending_issuances: BTreeMap::new(),
            join_attempts: VecDeque::new(),
            cleanup_pending: false,
            last_election: None,
            authority_epoch: 0,
            started_at: None,
            last_authority_heartbeat_at: None,
            last_accepted_heartbeat: None,
            terminal_at: None,
        }
    }

    /// Installs a hash-only creator capability before the atomic create.
    #[must_use]
    pub fn with_creator_capability(mut self, capability: StoredCapabilityVerifier) -> Self {
        self.capabilities.push(capability);
        self
    }

    /// Adds another exact-lobby hash-only capability.
    pub fn add_capability(&mut self, capability: StoredCapabilityVerifier) {
        self.capabilities.push(capability);
    }

    /// Finds a capability without revealing whether the lobby exists.
    pub(crate) fn authorize(
        &self,
        verifier: &[u8; 32],
        required: LobbyCapabilityScope,
        expected_player: Option<PlayerId>,
        now: UnixMillis,
    ) -> Option<Option<PlayerId>> {
        self.capabilities
            .iter()
            .find(|capability| {
                capability.authorizes(
                    verifier,
                    self.lobby.lobby_id,
                    self.network_generation,
                    required,
                    expected_player,
                    now,
                )
            })
            .map(StoredCapabilityVerifier::player_id)
    }

    pub(crate) fn active_invitation_count(&self, now: UnixMillis) -> usize {
        self.capabilities
            .iter()
            .filter(|capability| {
                capability.scopes.contains(&LobbyCapabilityScope::LobbyJoin)
                    && !capability.revoked
                    && capability.consumed_at.is_none()
                    && now < capability.expires_at
            })
            .count()
    }

    pub(crate) fn matches_invitation(&self, verifier: &[u8; 32], now: UnixMillis) -> bool {
        self.capabilities.iter().any(|capability| {
            capability.scopes.contains(&LobbyCapabilityScope::LobbyJoin)
                && !capability.revoked
                && now < capability.expires_at
                && constant_time_eq(&capability.verifier, verifier)
        })
    }

    /// Atomically consumes a one-use invitation while the caller holds the lobby lock.
    pub(crate) fn consume_invitation(&mut self, verifier: &[u8; 32], now: UnixMillis) -> bool {
        self.capabilities.iter_mut().any(|capability| {
            capability.consume(
                verifier,
                self.lobby.lobby_id,
                self.network_generation,
                LobbyCapabilityScope::LobbyJoin,
                now,
            )
        })
    }

    /// Revokes every participant capability bound to a player who has left.
    pub(crate) fn revoke_player_capabilities(&mut self, player_id: PlayerId) {
        for capability in &mut self.capabilities {
            if capability.player_id() == Some(player_id) {
                capability.revoked = true;
            }
        }
    }

    /// Revokes all creator control capabilities.
    pub fn revoke_creator_capability(&mut self) {
        for capability in &mut self.capabilities {
            if capability
                .scopes
                .contains(&LobbyCapabilityScope::LobbyDestroy)
            {
                capability.revoked = true;
            }
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
            .field("roster_revision", &self.roster_revision)
            .field("session_generation", &self.session_generation)
            .field("network_lifecycle", &self.network_lifecycle)
            .field("network_identity_present", &self.network_identity.is_some())
            .field(
                "child_policy_digest_present",
                &self.child_policy_digest.is_some(),
            )
            .field("child_policy_status", &self.child_policy_status)
            .field("capability_count", &self.capabilities.len())
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredCreateGrant {
    verifier: [u8; 32],
    expires_at: UnixMillis,
    consumed_at: Option<UnixMillis>,
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
    #[serde(default)]
    real_create_grants: Vec<StoredCreateGrant>,
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
    /// The one-use real-create grant was absent, invalid, expired, or consumed.
    #[error("real create grant invalid")]
    InvalidCreateGrant,
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
    /// Durably records a hash-only operator-issued real-create grant.
    async fn issue_real_create_grant(
        &self,
        verifier: [u8; 32],
        expires_at: UnixMillis,
    ) -> Result<(), StoreError>;

    /// Durably quarantines real admission after an orphan or reconciliation conflict.
    async fn quarantine_real_creation(&self) -> Result<(), StoreError>;

    /// Atomically inserts a lobby or resolves its create idempotency key.
    #[allow(clippy::too_many_arguments)]
    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        actor: PlayerId,
        now: UnixMillis,
        lobby: StoredLobby,
        real_create_grant: Option<[u8; 32]>,
        allow_new_real: bool,
    ) -> Result<CreateStoreOutcome, StoreError>;

    /// Lists retained IDs for internal maintenance without mutating records.
    async fn lobby_ids(&self) -> Vec<LobbyId>;

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
    async fn issue_real_create_grant(
        &self,
        verifier: [u8; 32],
        expires_at: UnixMillis,
    ) -> Result<(), StoreError> {
        let mut data = self.inner.write().await;
        if data.real_create_grants.len() >= 128 {
            return Err(StoreError::Capacity);
        }
        data.real_create_grants.push(StoredCreateGrant {
            verifier,
            expires_at,
            consumed_at: None,
        });
        Ok(())
    }

    async fn quarantine_real_creation(&self) -> Result<(), StoreError> {
        self.inner.write().await.real_creation_quarantined = true;
        Ok(())
    }

    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        actor: PlayerId,
        now: UnixMillis,
        lobby: StoredLobby,
        real_create_grant: Option<[u8; 32]>,
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
            real_create_grant,
            allow_new_real,
        )
    }

    async fn lobby_ids(&self) -> Vec<LobbyId> {
        self.inner.read().await.lobbies.keys().copied().collect()
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
    /// Lifetime-held OS advisory lock fencing every writer of this state image.
    _writer_lock: Arc<std::fs::File>,
}

impl JsonFileStore {
    /// Opens existing state or creates an empty in-memory image when absent.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|_| StoreError::Io)?;
        }
        let writer_lock = open_writer_lock(&path).map_err(|_| StoreError::Io)?;
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
            _writer_lock: Arc::new(writer_lock),
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
    async fn issue_real_create_grant(
        &self,
        verifier: [u8; 32],
        expires_at: UnixMillis,
    ) -> Result<(), StoreError> {
        let mut data = self.inner.write().await;
        if data.real_create_grants.len() >= 128 {
            return Err(StoreError::Capacity);
        }
        let mut next = data.clone();
        next.real_create_grants.push(StoredCreateGrant {
            verifier,
            expires_at,
            consumed_at: None,
        });
        self.commit(&next).await?;
        *data = next;
        Ok(())
    }

    async fn quarantine_real_creation(&self) -> Result<(), StoreError> {
        let mut data = self.inner.write().await;
        if data.real_creation_quarantined {
            return Ok(());
        }
        let mut next = data.clone();
        next.real_creation_quarantined = true;
        self.commit(&next).await?;
        *data = next;
        Ok(())
    }

    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        actor: PlayerId,
        now: UnixMillis,
        lobby: StoredLobby,
        real_create_grant: Option<[u8; 32]>,
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
            real_create_grant,
            allow_new_real,
        )?;
        self.commit(&next).await?;
        *data = next;
        Ok(outcome)
    }

    async fn lobby_ids(&self) -> Vec<LobbyId> {
        self.inner.read().await.lobbies.keys().copied().collect()
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

#[allow(clippy::too_many_arguments)]
fn create_in_data(
    data: &mut StoreData,
    idempotency_key: String,
    fingerprint: Vec<u8>,
    actor: PlayerId,
    now: UnixMillis,
    lobby: StoredLobby,
    real_create_grant: Option<[u8; 32]>,
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
        let candidate = real_create_grant.ok_or(StoreError::InvalidCreateGrant)?;
        let grant = data
            .real_create_grants
            .iter_mut()
            .find(|grant| {
                grant.consumed_at.is_none()
                    && now < grant.expires_at
                    && constant_time_eq(&grant.verifier, &candidate)
            })
            .ok_or(StoreError::InvalidCreateGrant)?;
        grant.consumed_at = Some(now);
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

    let release_requested = lease_release_lifecycle(lobby.network_lifecycle);
    let release = release_requested && has_release_proof(&lobby);
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
        } else if release_requested {
            data.real_creation_quarantined = true;
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
    stored.authorize(verifier, LobbyCapabilityScope::LobbyRead, None, now)?;
    Some(stored.clone())
}

fn idempotency_digest(fingerprint: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(IDEMPOTENCY_DIGEST_DOMAIN.len() + fingerprint.len());
    input.extend_from_slice(IDEMPOTENCY_DIGEST_DOMAIN);
    input.extend_from_slice(fingerprint);
    sha256(&input)
}

fn has_release_proof(stored: &StoredLobby) -> bool {
    match stored.network_lifecycle {
        NetworkLifecycle::CreateRejected => stored.network_identity.is_none(),
        NetworkLifecycle::DedicatedAbsent => {
            let identity_matches = stored.network_identity.as_ref().is_some_and(|identity| {
                identity.network_generation == stored.network_generation
                    && identity.tailnet_dns_name.as_str() == stored.tailnet
                    && identity.provider_tailnet_id.as_deref().is_some_and(|id| {
                        !id.is_empty()
                            && id.len() <= 128
                            && id.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                            })
                    })
            });
            identity_matches
                && stored.cleanup_requested_at.is_some()
                && stored.delete_acknowledged_at.is_some()
                && stored.child_secret_erased_at.is_some()
                && matches!(
                    (
                        stored.first_absence_observed_at,
                        stored.absence_confirmed_at
                    ),
                    (Some(first), Some(confirmed))
                        if confirmed
                            .checked_duration_since(first)
                            .is_some_and(|age| age >= CLEANUP_ABSENCE_MIN_SEPARATION_MS)
                )
                && !stored.cleanup_pending
        }
        NetworkLifecycle::SharedResourcesClean => {
            stored.lobby.provisioning_mode == spurfire_protocol::ProvisioningMode::SharedTailnet
                && stored.network_identity.as_ref().is_some_and(|identity| {
                    identity.network_generation == stored.network_generation
                        && identity.provider_tailnet_id.is_none()
                        && identity.tailnet_dns_name.as_str() == stored.tailnet
                })
                && stored.cleanup_requested_at.is_some()
                && !stored.cleanup_pending
        }
        _ => false,
    }
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
            if stored.dry_run
                || (lease_release_lifecycle(stored.network_lifecycle) && has_release_proof(stored))
            {
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
            if holders.len() == 1
                && holders[0] == lease.holder_lobby_id
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
    data.real_create_grants.retain(|grant| {
        grant.expires_at > now
            || grant
                .consumed_at
                .is_some_and(|at| !age_at_least(now, at, IDEMPOTENCY_RETENTION_MS))
    });
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

fn open_writer_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(lock_path)?;
    file.try_lock_exclusive()?;
    Ok(file)
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
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await.map_err(|_| StoreError::Io)?;
    file.write_all(&bytes).await.map_err(|_| StoreError::Io)?;
    file.sync_all().await.map_err(|_| StoreError::Io)?;
    drop(file);
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
        .map_err(|_| StoreError::Io)?;
    #[cfg(unix)]
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let directory = tokio::fs::File::open(parent)
            .await
            .map_err(|_| StoreError::Io)?;
        directory.sync_all().await.map_err(|_| StoreError::Io)?;
    }
    Ok(())
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

    fn real_record(lobby_id: &str) -> StoredLobby {
        let mut lobby = lobby();
        lobby.lobby_id = LobbyId::parse(lobby_id).unwrap();
        lobby.provisioning_mode = ProvisioningMode::TailnetPerLobby;
        StoredLobby::new(
            lobby,
            player_id(),
            "provisioning.invalid",
            "tag:test",
            false,
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
                None,
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
                None,
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

    #[tokio::test]
    async fn real_lease_resolves_replay_before_quota_and_holds_ambiguity() {
        let store = InMemoryStore::new();
        store
            .issue_real_create_grant([1; 32], UnixMillis::new(1_000))
            .await
            .unwrap();
        let first = real_record("00000000-0000-4000-8000-000000000011");
        let first_id = first.lobby.lobby_id;
        assert!(matches!(
            store
                .create(
                    "real-create".to_owned(),
                    b"same-body".to_vec(),
                    player_id(),
                    UnixMillis::new(10),
                    first,
                    Some([1; 32]),
                    true,
                )
                .await
                .unwrap(),
            CreateStoreOutcome::Created(_)
        ));
        assert!(store.real_lobby_lease_held().await);

        let replay_candidate = real_record("00000000-0000-4000-8000-000000000012");
        let replay = store
            .create(
                "real-create".to_owned(),
                b"same-body".to_vec(),
                player_id(),
                UnixMillis::new(11),
                replay_candidate,
                None,
                false,
            )
            .await
            .unwrap();
        assert!(
            matches!(replay, CreateStoreOutcome::Replay(stored) if stored.lobby.lobby_id == first_id)
        );

        let distinct = store
            .create(
                "other-real-create".to_owned(),
                b"other-body".to_vec(),
                player_id(),
                UnixMillis::new(12),
                real_record("00000000-0000-4000-8000-000000000013"),
                None,
                true,
            )
            .await;
        assert_eq!(distinct.unwrap_err(), StoreError::RealLobbyCapacityReached);

        let mut retained = store.get(first_id).await.unwrap();
        retained.network_lifecycle = NetworkLifecycle::CreateUnknown;
        store.replace(retained).await.unwrap();
        assert!(store.real_lobby_lease_held().await);
    }

    #[tokio::test]
    async fn lease_releases_only_at_explicit_safe_terminal_network_lifecycle() {
        let store = InMemoryStore::new();
        store
            .issue_real_create_grant([2; 32], UnixMillis::new(1_000))
            .await
            .unwrap();
        let record = real_record("00000000-0000-4000-8000-000000000021");
        let lobby_id = record.lobby.lobby_id;
        store
            .create(
                "release-create".to_owned(),
                b"body".to_vec(),
                player_id(),
                UnixMillis::new(10),
                record,
                Some([2; 32]),
                true,
            )
            .await
            .unwrap();
        for lifecycle in [
            NetworkLifecycle::Creating,
            NetworkLifecycle::Active,
            NetworkLifecycle::CleanupPending,
            NetworkLifecycle::VerifyingAbsence,
            NetworkLifecycle::ManualRemediation,
        ] {
            let mut stored = store.get(lobby_id).await.unwrap();
            stored.network_lifecycle = lifecycle;
            store.replace(stored).await.unwrap();
            assert!(
                store.real_lobby_lease_held().await,
                "released at {lifecycle:?}"
            );
        }
        let mut destroyed_only = store.get(lobby_id).await.unwrap();
        destroyed_only.lobby.state = LobbyState::Destroyed;
        destroyed_only.network_lifecycle = NetworkLifecycle::Active;
        store.replace(destroyed_only).await.unwrap();
        assert!(store.real_lobby_lease_held().await);

        let mut stored = store.get(lobby_id).await.unwrap();
        stored.network_lifecycle = NetworkLifecycle::DedicatedAbsent;
        stored.tailnet = "tail-release.ts.net".to_owned();
        stored.network_identity = Some(StoredNetworkIdentity {
            provider_tailnet_id: Some("TtReleaseCNTRL".to_owned()),
            tailnet_dns_name: TailnetDnsName::parse("tail-release.ts.net").unwrap(),
            network_generation: 1,
            captured_at: UnixMillis::new(10),
        });
        stored.cleanup_requested_at = Some(UnixMillis::new(20));
        stored.delete_acknowledged_at = Some(UnixMillis::new(21));
        stored.child_secret_erased_at = Some(UnixMillis::new(21));
        stored.first_absence_observed_at = Some(UnixMillis::new(30));
        stored.absence_confirmed_at = Some(UnixMillis::new(35 + 5_000));
        stored.cleanup_pending = false;
        store.replace(stored).await.unwrap();
        assert!(!store.real_lobby_lease_held().await);
    }

    #[tokio::test]
    async fn capability_lookup_is_exact_generation_expiring_and_constant_time_compare_path() {
        let store = InMemoryStore::new();
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000031").unwrap();
        let record = {
            let mut record = record();
            record.lobby.lobby_id = lobby_id;
            record.with_creator_capability(StoredCapabilityVerifier::new(
                [7; 32],
                vec![LobbyCapabilityScope::LobbyRead],
                lobby_id,
                None,
                1,
                UnixMillis::new(100),
            ))
        };
        store
            .create(
                "cap-create".to_owned(),
                b"body".to_vec(),
                player_id(),
                UnixMillis::new(0),
                record,
                None,
                false,
            )
            .await
            .unwrap();
        assert!(store
            .get_authorized_network_view(lobby_id, [7; 32], UnixMillis::new(99))
            .await
            .is_some());
        assert!(store
            .get_authorized_network_view(lobby_id, [8; 32], UnixMillis::new(99))
            .await
            .is_none());
        assert!(store
            .get_authorized_network_view(lobby_id, [7; 32], UnixMillis::new(100))
            .await
            .is_none());
        let mut stored = store.get(lobby_id).await.unwrap();
        stored.network_generation = 2;
        store.replace(stored).await.unwrap();
        assert!(store
            .get_authorized_network_view(lobby_id, [7; 32], UnixMillis::new(50))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn durable_restart_retains_real_lease_and_non_secret_identity_only() {
        let path = std::env::temp_dir().join(format!(
            "spurfire-real-lease-test-{}.json",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        let store = JsonFileStore::open(&path).await.unwrap();
        store
            .issue_real_create_grant([3; 32], UnixMillis::new(1_000))
            .await
            .unwrap();
        let mut record = real_record("00000000-0000-4000-8000-000000000041");
        record.network_lifecycle = NetworkLifecycle::Active;
        record.tailnet = "tail-restart.ts.net".to_owned();
        record.network_identity = Some(StoredNetworkIdentity {
            provider_tailnet_id: Some("TtRestartCNTRL".to_owned()),
            tailnet_dns_name: TailnetDnsName::parse("tail-restart.ts.net").unwrap(),
            network_generation: 1,
            captured_at: UnixMillis::new(10),
        });
        store
            .create(
                "restart-real".to_owned(),
                b"body".to_vec(),
                player_id(),
                UnixMillis::new(10),
                record,
                Some([3; 32]),
                true,
            )
            .await
            .unwrap();
        drop(store);

        let reopened = JsonFileStore::open(&path).await.unwrap();
        assert!(reopened.real_lobby_lease_held().await);
        let encoded = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(encoded.contains("TtRestartCNTRL"));
        assert!(encoded.contains("tail-restart.ts.net"));
        for forbidden in ["auth_key", "oauthClient", "child-secret", "bearer"] {
            assert!(!encoded.contains(forbidden));
        }
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
