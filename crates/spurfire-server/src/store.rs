//! Lobby store abstraction and deterministic in-memory implementation.

use std::{collections::BTreeMap, fmt, sync::Arc};

use async_trait::async_trait;
use spurfire_protocol::{
    ConnectivitySample, JoinCredentialReceipt, Lobby, LobbyId, LobbyState, PlayerId, UnixMillis,
};
use thiserror::Error;
use tokio::sync::RwLock;

/// Non-secret record proving that a credential has already been delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCredential {
    /// Provider receipt identifier.
    pub credential_id: String,
    /// Absolute key expiration.
    pub expires_at: UnixMillis,
    /// Whether cleanup revoked this receipt.
    pub revoked: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredJoinReplay {
    pub fingerprint: Vec<u8>,
    pub player_id: PlayerId,
    pub receipt: JoinCredentialReceipt,
}

/// Complete in-memory lobby record. It never contains auth-key material or device IDs.
#[derive(Clone)]
pub struct StoredLobby {
    pub(crate) lobby: Lobby,
    pub(crate) tailnet: String,
    pub(crate) tag: String,
    pub(crate) dry_run: bool,
    pub(crate) idle_ttl_ms: u64,
    pub(crate) measurements: BTreeMap<PlayerId, ConnectivitySample>,
    pub(crate) credentials: BTreeMap<PlayerId, StoredCredential>,
    pub(crate) join_replays: BTreeMap<String, StoredJoinReplay>,
    pub(crate) cleanup_pending: bool,
}

impl StoredLobby {
    /// Creates a record around a validated public snapshot and non-secret provider metadata.
    #[must_use]
    pub fn new(
        lobby: Lobby,
        tailnet: impl Into<String>,
        tag: impl Into<String>,
        dry_run: bool,
        idle_ttl_ms: u64,
    ) -> Self {
        Self {
            lobby,
            tailnet: tailnet.into(),
            tag: tag.into(),
            dry_run,
            idle_ttl_ms,
            measurements: BTreeMap::new(),
            credentials: BTreeMap::new(),
            join_replays: BTreeMap::new(),
            cleanup_pending: false,
        }
    }

    /// Returns a cloned public snapshot with no provider-only state.
    #[must_use]
    pub fn snapshot(&self) -> Lobby {
        self.lobby.clone()
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
            .field("tailnet", &"<configured>")
            .field("tag", &self.tag)
            .field("dry_run", &self.dry_run)
            .field("measurement_count", &self.measurements.len())
            .field("credential_receipt_count", &self.credentials.len())
            .field("cleanup_pending", &self.cleanup_pending)
            .finish()
    }
}

/// Atomic result of create-idempotency evaluation.
#[derive(Clone, Debug)]
pub enum CreateStoreOutcome {
    /// A new lobby record and idempotency entry were inserted.
    Created(StoredLobby),
    /// An identical request key replayed the original lobby.
    Replay(StoredLobby),
    /// The key was already attached to a different request body.
    Conflict,
}

#[derive(Clone, Debug)]
struct CreateReplay {
    fingerprint: Vec<u8>,
    lobby_id: LobbyId,
}

#[derive(Default)]
struct StoreData {
    lobbies: BTreeMap<LobbyId, StoredLobby>,
    create_replays: BTreeMap<String, CreateReplay>,
}

/// Store failures that indicate an internal consistency error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
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
}

/// Persistence boundary used by the HTTP service.
#[async_trait]
pub trait LobbyStore: Send + Sync {
    /// Atomically inserts a lobby or resolves its create idempotency key.
    async fn create(
        &self,
        idempotency_key: String,
        fingerprint: Vec<u8>,
        lobby: StoredLobby,
    ) -> Result<CreateStoreOutcome, StoreError>;

    /// Reads one complete non-secret record.
    async fn get(&self, lobby_id: LobbyId) -> Option<StoredLobby>;

    /// Replaces one record after a serialized service mutation.
    async fn replace(&self, lobby: StoredLobby) -> Result<(), StoreError>;

    /// Applies deterministic TTL transitions using only the supplied timestamp.
    /// Returned IDs are sorted lexicographically.
    async fn cleanup_expired(&self, now: UnixMillis) -> Vec<LobbyId>;
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
        lobby: StoredLobby,
    ) -> Result<CreateStoreOutcome, StoreError> {
        let mut data = self.inner.write().await;
        if let Some(replay) = data.create_replays.get(&idempotency_key) {
            if replay.fingerprint != fingerprint {
                return Ok(CreateStoreOutcome::Conflict);
            }
            return data
                .lobbies
                .get(&replay.lobby_id)
                .cloned()
                .map(CreateStoreOutcome::Replay)
                .ok_or(StoreError::InconsistentIdempotency);
        }
        if data.lobbies.contains_key(&lobby.lobby.lobby_id) {
            return Err(StoreError::DuplicateLobby);
        }
        data.create_replays.insert(
            idempotency_key,
            CreateReplay {
                fingerprint,
                lobby_id: lobby.lobby.lobby_id,
            },
        );
        data.lobbies.insert(lobby.lobby.lobby_id, lobby.clone());
        Ok(CreateStoreOutcome::Created(lobby))
    }

    async fn get(&self, lobby_id: LobbyId) -> Option<StoredLobby> {
        self.inner.read().await.lobbies.get(&lobby_id).cloned()
    }

    async fn replace(&self, lobby: StoredLobby) -> Result<(), StoreError> {
        let mut data = self.inner.write().await;
        let Some(slot) = data.lobbies.get_mut(&lobby.lobby.lobby_id) else {
            return Err(StoreError::LobbyNotFound);
        };
        *slot = lobby;
        Ok(())
    }

    async fn cleanup_expired(&self, now: UnixMillis) -> Vec<LobbyId> {
        let mut data = self.inner.write().await;
        let mut transitioned = Vec::new();
        for (lobby_id, record) in &mut data.lobbies {
            let absolute_due = now >= record.lobby.ttl.absolute_expires_at;
            let idle_due = now >= record.lobby.ttl.idle_expires_at;
            let target = match record.lobby.state {
                LobbyState::Provisioning if absolute_due => {
                    Some((LobbyState::Failed, Some("absolute_ttl_expired".to_owned())))
                }
                LobbyState::Forming | LobbyState::Ready if absolute_due || idle_due => {
                    Some((LobbyState::Expired, None))
                }
                LobbyState::Starting | LobbyState::InMatch if absolute_due => {
                    Some((LobbyState::Failed, Some("absolute_ttl_expired".to_owned())))
                }
                _ => None,
            };
            let mut needs_cleanup = false;
            if let Some((state, reason)) = target {
                if record.lobby.state.validate_transition(state).is_ok() {
                    record.lobby.state = state;
                    record.lobby.state_reason = reason;
                    record.lobby.authority = None;
                    for credential in record.credentials.values_mut() {
                        credential.revoked = true;
                    }
                    needs_cleanup = true;
                }
            }
            if needs_cleanup
                || (record.cleanup_pending
                    && matches!(
                        record.lobby.state,
                        LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
                    ))
            {
                transitioned.push(*lobby_id);
            }
        }
        transitioned
    }
}

#[cfg(test)]
mod tests {
    use spurfire_protocol::{LobbyTtl, ProvisioningMode, WireVersion, WIRE_VERSION};

    use super::*;

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
            wire_version: WireVersion::new(WIRE_VERSION.major(), WIRE_VERSION.minor()),
            provisioning_mode: ProvisioningMode::DryRun,
            created_at: UnixMillis::new(0),
        }
    }

    #[tokio::test]
    async fn cleanup_uses_caller_supplied_time() {
        let store = InMemoryStore::new();
        let record = StoredLobby::new(lobby(), "dry-run.invalid", "tag:test", true, 1_000);
        store
            .create("create-1".to_owned(), b"body".to_vec(), record)
            .await
            .unwrap();

        assert!(store.cleanup_expired(UnixMillis::new(999)).await.is_empty());
        assert_eq!(
            store.cleanup_expired(UnixMillis::new(1_000)).await,
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
}
