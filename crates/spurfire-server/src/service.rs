//! Axum router and lobby application service.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use spurfire_protocol::{
    elect_authority, elect_authority_for_roster, validate_start_roster, AcceptedHeartbeatReference,
    ApplicationQuality, AuthorityCandidate, AuthorityElection, AuthorityElectionError,
    AuthorityHeartbeatRequest, AuthorityHeartbeatResponse, AuthorityResponse, AuthoritySummary,
    BackingMode, CapabilitiesResponse, ControlElectionReference, CreateLobbyRequest,
    CreateLobbyResponse, DestroyLobbyResponse, Fact, FactAssurance, FactSource, FinalScore,
    Freshness, InspectedLobbyLifecycle, JoinCredential, JoinCredentialReceipt,
    JoinLobbyReplayResponse, JoinLobbyRequest, JoinLobbyResponse, LeaveLobbyRequest,
    LeaveLobbyResponse, Lobby, LobbyId, LobbyNetworkView, LobbyResponse, LobbyState, LobbyTtl,
    NetworkAuthority, NetworkBacking, NetworkCleanup, NetworkCounts, NetworkIsolation,
    NetworkLifecycle, NetworkTruthLabel, ParticipantCleanupReason, Player, PlayerId,
    PlayerJoinState, ProvisioningMode, ResponseMetadata, RouteAggregate, StartLobbyRequest,
    StartLobbyResponse, SubmitMeasurementsRequest, SubmitMeasurementsResponse,
    SubmitResultsRequest, SubmitResultsResponse, UnixMillis, UnknownReason, ABSOLUTE_TTL_MS,
    DRY_RUN_AUTH_KEY, IDLE_TTL_MS, JOIN_CREDENTIAL_TTL_MS, LOBBY_NETWORK_SCHEMA_VERSION,
    MEASUREMENT_FRESHNESS_MS, PROTOTYPE_MIN_PLAYERS, WIRE_VERSION,
};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, Semaphore};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    clock::{Clock, SystemClock},
    config::Config,
    crypto::{base64url_decode, base64url_encode, sha256},
    error::ApiError,
    provider::{
        CleanupLobbyRequest, CleanupOutcome, CredentialCleanup, MintCredentialRequest,
        MutationGatedProvider, NetworkProvider, ObserveNetworkRequest, PrepareLobbyRequest, ProviderError,
        ProviderNetworkIdentity, TailnetPresenceRequest,
    },
    store::{
        apply_time_transitions, CreateStoreOutcome, LobbyStore, StoredAcceptedHeartbeat,
        StoredCapabilityVerifier, StoredCredential, StoredIssuanceReservation, StoredJoinAttempt,
        StoredJoinReplay, StoredLobby, StoredMutationReplay, StoredNetworkIdentity,
    },
};

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const DRY_RUN_HEADER: &str = "x-spurfire-dry-run";
const ACTOR_HEADER: &str = "x-spurfire-player-id";
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const JOIN_RATE_WINDOW_MS: u64 = 60_000;
const MAX_JOIN_ATTEMPTS_PER_LOBBY: usize = 32;
const MAX_JOIN_ATTEMPTS_PER_PLAYER: usize = 4;
const AUTHORITY_SILENCE_MS: u64 = 2_000;
const MAX_PROVIDER_CONCURRENCY: usize = 16;
const PROVIDER_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FINAL_SCORE_ABS: i64 = 1_000_000;
const MAX_MATCH_DURATION_S: u32 = 60 * 60;
const AUTHORIZATION_HEADER: &str = "authorization";
const CAPABILITY_SCHEME: &str = "Spurfire-Capability";
const CAPABILITY_HASH_DOMAIN: &[u8] = b"spurfire-capability-lobby-read-v1\0";
const INSPECTOR_REPORT_FRESHNESS_MS: u64 = 15_000;
const PROVIDER_DEVICE_FRESHNESS_MS: u64 = 30_000;
const PROVIDER_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_ABSENCE_MIN_SEPARATION_MS: u64 = 5_000;
const MAX_CACHED_PROVIDER_OBSERVATIONS: usize = 10_000;
const LANDING_HTML: &str = include_str!("landing.html");
const INSPECT_HTML: &str = include_str!("inspect.html");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CachedObservationFailure {
    PermissionDenied,
    Timeout,
    SourceError,
    Conflict,
}

impl CachedObservationFailure {
    const fn unknown_reason(self) -> UnknownReason {
        match self {
            Self::PermissionDenied => UnknownReason::PermissionDenied,
            Self::Timeout => UnknownReason::Timeout,
            Self::SourceError => UnknownReason::SourceError,
            Self::Conflict => UnknownReason::Conflict,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CachedProviderObservation {
    network_generation: u64,
    enrolled_device_count: Option<u32>,
    successful_at: Option<UnixMillis>,
    last_failure: Option<(CachedObservationFailure, UnixMillis)>,
}

impl CachedProviderObservation {
    const fn success(
        network_generation: u64,
        enrolled_device_count: u32,
        at: UnixMillis,
    ) -> Self {
        Self {
            network_generation,
            enrolled_device_count: Some(enrolled_device_count),
            successful_at: Some(at),
            last_failure: None,
        }
    }

    const fn failed(
        previous: Option<Self>,
        failure: CachedObservationFailure,
        at: UnixMillis,
    ) -> Self {
        Self {
            network_generation: previous.map_or(0, |value| value.network_generation),
            enrolled_device_count: match previous {
                Some(previous) => previous.enrolled_device_count,
                None => None,
            },
            successful_at: match previous {
                Some(previous) => previous.successful_at,
                None => None,
            },
            last_failure: Some((failure, at)),
        }
    }
}

struct GeneratedCapability {
    plaintext: Zeroizing<String>,
    verifier: [u8; 32],
}

impl GeneratedCapability {
    fn generate() -> Self {
        // Three UUIDv4 values contribute 366 random bits after their fixed
        // version/variant bits, exceeding the 256-bit capability minimum.
        let mut bytes = [0_u8; 48];
        for (chunk, uuid) in
            bytes
                .chunks_exact_mut(16)
                .zip([Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()])
        {
            chunk.copy_from_slice(uuid.as_bytes());
        }
        let plaintext = Zeroizing::new(base64url_encode(&bytes));
        bytes.zeroize();
        let verifier = capability_verifier(plaintext.as_bytes());
        Self {
            plaintext,
            verifier,
        }
    }
}

impl fmt::Debug for GeneratedCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GeneratedCapability(<redacted>)")
    }
}

#[derive(Serialize)]
struct FirstCreateResponse<'a> {
    #[serde(flatten)]
    response: CreateLobbyResponse,
    creator_capability: &'a str,
}

/// Cloneable application dependencies shared by every Axum handler.
#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    store: Arc<dyn LobbyStore>,
    provider: Arc<dyn NetworkProvider>,
    clock: Arc<dyn Clock>,
    lobby_locks: Arc<Mutex<BTreeMap<LobbyId, Arc<Mutex<()>>>>>,
    observation_locks: Arc<Mutex<BTreeMap<LobbyId, Arc<Mutex<()>>>>>,
    provider_observations: Arc<RwLock<BTreeMap<LobbyId, CachedProviderObservation>>>,
    provider_limit: Arc<Semaphore>,
}

impl AppState {
    /// Creates application state using the production wall clock.
    #[must_use]
    pub fn new(
        config: Config,
        store: Arc<dyn LobbyStore>,
        provider: Arc<dyn NetworkProvider>,
    ) -> Self {
        let real_mutations_enabled = config.real_mutations_enabled;
        Self {
            config: Arc::new(config),
            store,
            provider: Arc::new(MutationGatedProvider::new(
                provider,
                real_mutations_enabled,
            )),
            clock: Arc::new(SystemClock),
            lobby_locks: Arc::new(Mutex::new(BTreeMap::new())),
            observation_locks: Arc::new(Mutex::new(BTreeMap::new())),
            provider_observations: Arc::new(RwLock::new(BTreeMap::new())),
            provider_limit: Arc::new(Semaphore::new(MAX_PROVIDER_CONCURRENCY)),
        }
    }

    /// Replaces the wall clock, normally for deterministic tests.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Returns the configured non-secret settings.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the persistence abstraction.
    #[must_use]
    pub fn store(&self) -> Arc<dyn LobbyStore> {
        Arc::clone(&self.store)
    }

    async fn lock_lobby(&self, lobby_id: LobbyId) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.lobby_locks.lock().await;
            Arc::clone(
                locks
                    .entry(lobby_id)
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }

    async fn lock_observation(&self, lobby_id: LobbyId) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.observation_locks.lock().await;
            Arc::clone(
                locks
                    .entry(lobby_id)
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }

    /// Refreshes one exact lobby's coarse provider observation into a bounded
    /// cache. This is an explicit worker/operator action; inspection GET never
    /// calls it and never fans out upstream.
    pub async fn refresh_network_observation_at(
        &self,
        lobby_id: LobbyId,
        now: UnixMillis,
    ) -> Result<(), ProviderError> {
        let _refresh = self.lock_observation(lobby_id).await;
        let stored = self
            .store
            .get(lobby_id)
            .await
            .ok_or(ProviderError::Unavailable {
                operation: "network_observation_lobby",
            })?;
        if stored.dry_run || stored.lobby.provisioning_mode == ProvisioningMode::DryRun {
            return Ok(());
        }
        if !matches!(
            stored.network_lifecycle,
            NetworkLifecycle::Active
                | NetworkLifecycle::CleanupRequested
                | NetworkLifecycle::CleanupPending
                | NetworkLifecycle::VerifyingAbsence
        ) {
            return Err(ProviderError::Unavailable {
                operation: "network_observation_inactive",
            });
        }
        let network_generation = stored.network_generation;
        let request = observe_request(&stored)?;
        let outcome = tokio::time::timeout(
            PROVIDER_OBSERVATION_TIMEOUT,
            self.provider.observe_network(request),
        )
        .await;

        // Provider I/O may race cleanup or a future replacement generation.
        // Revalidate before any result can enter the cache.
        let current = self.store.get(lobby_id).await;
        if current.as_ref().is_none_or(|value| {
            value.network_generation != network_generation
                || !matches!(
                    value.network_lifecycle,
                    NetworkLifecycle::Active
                        | NetworkLifecycle::CleanupRequested
                        | NetworkLifecycle::CleanupPending
                        | NetworkLifecycle::VerifyingAbsence
                )
        }) {
            return Err(ProviderError::Unavailable {
                operation: "network_observation_generation_changed",
            });
        }

        match outcome {
            Ok(Ok(observation)) => {
                self.cache_provider_observation(
                    lobby_id,
                    CachedProviderObservation::success(
                        network_generation,
                        observation.enrolled_device_count,
                        now,
                    ),
                )
                .await;
                Ok(())
            }
            Ok(Err(error)) => {
                let failure = observation_failure(&error);
                self.cache_provider_failure(lobby_id, failure, now).await;
                Err(error)
            }
            Err(_) => {
                self.cache_provider_failure(lobby_id, CachedObservationFailure::Timeout, now)
                    .await;
                Err(ProviderError::Unavailable {
                    operation: "network_observation_timeout",
                })
            }
        }
    }

    /// Refreshes against the configured wall clock.
    pub async fn refresh_network_observation(
        &self,
        lobby_id: LobbyId,
    ) -> Result<(), ProviderError> {
        self.refresh_network_observation_at(lobby_id, self.clock.now())
            .await
    }

    async fn cache_provider_observation(
        &self,
        lobby_id: LobbyId,
        observation: CachedProviderObservation,
    ) {
        let mut cache = self.provider_observations.write().await;
        if cache.len() < MAX_CACHED_PROVIDER_OBSERVATIONS || cache.contains_key(&lobby_id) {
            cache.insert(lobby_id, observation);
        }
    }

    async fn cache_provider_failure(
        &self,
        lobby_id: LobbyId,
        failure: CachedObservationFailure,
        now: UnixMillis,
    ) {
        let previous = self
            .provider_observations
            .read()
            .await
            .get(&lobby_id)
            .copied();
        self.cache_provider_observation(
            lobby_id,
            CachedProviderObservation::failed(previous, failure, now),
        )
        .await;
    }

    /// Runs deterministic expiry/start-timeout transitions and teardown retries.
    pub async fn cleanup_expired_at(&self, now: UnixMillis) -> Vec<LobbyId> {
        let lobby_ids = self.store.lobby_ids().await;
        let mut cleaned = Vec::new();
        for lobby_id in lobby_ids {
            // Candidate discovery is read-only. Transition and teardown happen
            // only after acquiring the same lock as join/delete handlers.
            let _lobby = self.lock_lobby(lobby_id).await;
            let Some(mut stored) = self.store.get(lobby_id).await else {
                continue;
            };
            let transitioned = apply_time_transitions(&mut stored, now);
            let needs_cleanup = transitioned
                || stored.lobby.state == LobbyState::Closing
                || (stored.cleanup_pending
                    && matches!(
                        stored.lobby.state,
                        LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
                    ));
            if !needs_cleanup {
                continue;
            }
            let finalize = stored.lobby.state == LobbyState::Closing;
            let _ = cleanup_resources(self, &mut stored, now, true, finalize).await;
            let _ = self.store.replace(stored).await;
            cleaned.push(lobby_id);
        }
        cleaned
    }

    /// Runs expiry cleanup against the configured clock.
    pub async fn cleanup_expired_now(&self) -> Vec<LobbyId> {
        self.cleanup_expired_at(self.clock.now()).await
    }
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("config", &self.config)
            .field("store", &"<lobby-store>")
            .field("provider", &"<network-provider>")
            .field("clock", &"<clock>")
            .field("provider_observations", &"<bounded-coarse-cache>")
            .finish()
    }
}

/// Builds the complete HTTP router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(landing))
        .route("/inspect", get(inspect_shell))
        .route("/healthz", get(healthz))
        .route("/v1/capabilities", get(get_capabilities))
        .route("/v1/lobbies", post(create_lobby))
        .route(
            "/v1/lobbies/{lobby_id}",
            get(get_lobby).delete(delete_lobby),
        )
        .route("/v1/lobbies/{lobby_id}/network", get(get_lobby_network))
        .route("/v1/lobbies/{lobby_id}/join", post(join_lobby))
        .route("/v1/lobbies/{lobby_id}/leave", post(leave_lobby))
        .route(
            "/v1/lobbies/{lobby_id}/measurements",
            post(submit_measurements),
        )
        .route(
            "/v1/lobbies/{lobby_id}/elect-authority",
            post(elect_lobby_authority),
        )
        .route("/v1/lobbies/{lobby_id}/authority", get(get_lobby_authority))
        .route("/v1/lobbies/{lobby_id}/start", post(start_lobby))
        .route(
            "/v1/lobbies/{lobby_id}/heartbeat",
            post(authority_heartbeat),
        )
        .route("/v1/lobbies/{lobby_id}/results", post(submit_results))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(64 * 1_024))
        .with_state(state)
}

/// Alias that reads naturally in embedders.
pub fn router(state: AppState) -> Router {
    build_router(state)
}

async fn landing() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    (headers, Html(LANDING_HTML))
}

async fn inspect_shell() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'sha256-M+A8mqmnVAeKAZUaP1OIDiMgzOa3E/Q2fsItjMYClpM='; script-src 'sha256-3xl3dBD9h+2H3qL/B3ZS2tKPYh9LqF4Uicf6YQuZbmk='; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    (headers, Html(INSPECT_HTML))
}

#[derive(Serialize)]
struct AnonymousDryLobbyResponse {
    lobby_id: LobbyId,
    state: LobbyState,
    max_players: u8,
    ttl: LobbyTtl,
    wire_version: spurfire_protocol::WireVersion,
    provisioning_mode: ProvisioningMode,
    created_at: UnixMillis,
    cleanup_pending: bool,
    roster_count: u32,
    dry_run: bool,
    planned_actions: Vec<spurfire_protocol::PlannedAction>,
}

impl From<LobbyResponse> for AnonymousDryLobbyResponse {
    fn from(response: LobbyResponse) -> Self {
        Self {
            lobby_id: response.lobby.lobby_id,
            state: response.lobby.state,
            max_players: response.lobby.max_players,
            ttl: response.lobby.ttl,
            wire_version: response.lobby.wire_version,
            provisioning_mode: ProvisioningMode::DryRun,
            created_at: response.lobby.created_at,
            cleanup_pending: response.lobby.cleanup_pending,
            roster_count: u32::try_from(response.lobby.roster.len())
                .expect("validated roster is bounded by MAX_PLAYERS"),
            dry_run: true,
            planned_actions: Vec::new(),
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    provisioning_ready: bool,
}

async fn healthz(State(state): State<AppState>) -> Json<HealthResponse> {
    let ready = state.config.force_dry_run
        || (state.config.real_mutations_enabled
            && state
                .provider
                .cached_capabilities()
                .mode_available(state.config.provisioning_mode));
    Json(HealthResponse {
        status: if ready { "ok" } else { "degraded" },
        provisioning_ready: ready,
    })
}

async fn get_capabilities(State(state): State<AppState>) -> Json<CapabilitiesResponse> {
    Json(
        state
            .provider
            .cached_capabilities()
            .response(metadata_for(state.config.force_dry_run)),
    )
}

async fn create_lobby(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CreateLobbyRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let request = parse_json(payload, dry_hint)?;
    let effective_dry_run = effective_request_dry_run(&state, &request, header_dry_run);
    let actor = require_actor(&headers, effective_dry_run)?;
    let idempotency_key = require_idempotency_key(&headers, effective_dry_run)?;
    request
        .validate()
        .map_err(|error| ApiError::validation(&error, effective_dry_run))?;

    if request.max_players > state.config.max_players {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "max_players_exceeds_service_limit",
            "max_players exceeds this service deployment limit",
        )
        .dry_run(effective_dry_run));
    }

    let effective_mode = if effective_dry_run {
        ProvisioningMode::DryRun
    } else {
        request.provisioning_mode
    };
    let request_fingerprint =
        fingerprint(&(request.clone(), effective_mode, actor), effective_dry_run)?;
    let now = state.clock.now();
    let lobby_id = new_lobby_id();
    let _lobby = state.lock_lobby(lobby_id).await;
    let absolute_ttl_ms = if effective_dry_run {
        state.config.dry_run_ttl_ms()
    } else {
        ABSOLUTE_TTL_MS
    };
    let idle_ttl_ms = IDLE_TTL_MS.min(absolute_ttl_ms);
    let lobby = Lobby {
        lobby_id,
        display_name: request.display_name,
        state: LobbyState::Provisioning,
        state_reason: None,
        roster: Vec::new(),
        max_players: request.max_players,
        map_seed: None,
        authority: None,
        ttl: LobbyTtl {
            idle_expires_at: now.saturating_add(idle_ttl_ms),
            absolute_expires_at: now.saturating_add(absolute_ttl_ms),
        },
        wire_version: WIRE_VERSION,
        provisioning_mode: effective_mode,
        created_at: now,
        cleanup_pending: false,
    };
    let creator_capability = GeneratedCapability::generate();
    let capability_record = StoredCapabilityVerifier::new(
        creator_capability.verifier,
        lobby_id,
        1,
        now.saturating_add(
            absolute_ttl_ms.saturating_add(crate::store::CREATOR_CAPABILITY_CLEANUP_GRACE_MS),
        ),
    );
    // Persist PROVISIONING/RESERVED and acquire the singleton real lease before
    // a child-tailnet mutation. Replay resolution occurs inside this same store
    // transaction before either the kill switch or quota is evaluated.
    let stored = StoredLobby::new(
        lobby,
        actor,
        "provisioning.invalid",
        lobby_tag(lobby_id),
        effective_dry_run,
        idle_ttl_ms,
    )
    .with_creator_capability(capability_record);

    let outcome = state
        .store
        .create(
            idempotency_key,
            request_fingerprint,
            actor,
            now,
            stored,
            state.config.real_mutations_enabled,
        )
        .await
        .map_err(|error| store_api_error(&error, effective_dry_run))?;
    match outcome {
        CreateStoreOutcome::Created(mut advanced) => {
            if !effective_dry_run {
                advanced.network_lifecycle = NetworkLifecycle::Creating;
                state
                    .store
                    .replace(advanced.clone())
                    .await
                    .map_err(|error| store_api_error(&error, effective_dry_run))?;
            }
            let prepared = tokio::time::timeout(
                PROVIDER_OPERATION_TIMEOUT,
                state.provider.prepare_lobby(PrepareLobbyRequest {
                    lobby_id,
                    network_generation: advanced.network_generation,
                    mode: effective_mode,
                    dry_run: effective_dry_run,
                }),
            )
            .await
            .unwrap_or(Err(ProviderError::Unavailable {
                operation: "prepare_lobby",
            }));
            let metadata = match prepared {
                Ok(prepared) => {
                    advanced.tailnet = prepared.tailnet;
                    advanced.dry_run = prepared.dry_run;
                    if prepared.dry_run {
                        advanced.network_lifecycle = NetworkLifecycle::Simulated;
                    } else {
                        let identity = prepared
                            .identity
                            .ok_or_else(|| internal_error(effective_dry_run))?;
                        identity
                            .validate_for_mode(effective_mode)
                            .map_err(|error| provider_api_error(&error, effective_dry_run))?;
                        advanced.network_identity = Some(StoredNetworkIdentity {
                            provider_tailnet_id: identity.provider_tailnet_id,
                            tailnet_dns_name: identity.tailnet_dns_name,
                            network_generation: advanced.network_generation,
                            captured_at: now,
                        });
                        advanced.network_lifecycle = NetworkLifecycle::Active;
                    }
                    if prepared.dry_run
                        || state
                            .provider
                            .cached_capabilities()
                            .mode_available(effective_mode)
                    {
                        transition(&mut advanced.lobby, LobbyState::Forming, effective_dry_run)?;
                    } else {
                        transition(&mut advanced.lobby, LobbyState::Failed, effective_dry_run)?;
                        advanced.network_lifecycle = NetworkLifecycle::ManualRemediation;
                        advanced.cleanup_pending = true;
                        advanced.lobby.state_reason = Some(
                            state
                                .provider
                                .cached_capabilities()
                                .blocked_state_reason(effective_mode)
                                .to_owned(),
                        );
                        advanced.terminal_at = Some(now);
                    }
                    prepared.metadata
                }
                Err(error) => {
                    transition(&mut advanced.lobby, LobbyState::Failed, effective_dry_run)?;
                    let ambiguous_child_create = effective_mode
                        == ProvisioningMode::TailnetPerLobby
                        && matches!(error, ProviderError::Unavailable { .. });
                    advanced.lobby.state_reason = Some(if ambiguous_child_create {
                        "tailnet_create_outcome_unknown_manual_remediation".to_owned()
                    } else {
                        error.state_reason().to_owned()
                    });
                    advanced.network_lifecycle = if ambiguous_child_create {
                        NetworkLifecycle::CreateUnknown
                    } else if effective_mode == ProvisioningMode::TailnetPerLobby
                        && matches!(
                            error,
                            ProviderError::InsufficientScopes {
                                operation: "organization_tailnet_create"
                            }
                        )
                    {
                        NetworkLifecycle::CreateRejected
                    } else {
                        NetworkLifecycle::ManualRemediation
                    };
                    advanced.cleanup_pending =
                        !matches!(advanced.network_lifecycle, NetworkLifecycle::CreateRejected);
                    advanced.terminal_at = Some(now);
                    metadata_for(effective_dry_run)
                }
            };
            let response = create_response(&advanced, metadata);
            state
                .store
                .replace(advanced)
                .await
                .map_err(|error| store_api_error(&error, effective_dry_run))?;
            Ok(sensitive_json_response(
                StatusCode::CREATED,
                &FirstCreateResponse {
                    response,
                    creator_capability: creator_capability.plaintext.as_str(),
                },
            ))
        }
        CreateStoreOutcome::Replay(stored) => Ok(sensitive_json_response(
            StatusCode::OK,
            &create_response(&stored, metadata_for(stored.dry_run)),
        )),
        CreateStoreOutcome::Conflict => Err(ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Idempotency-Key was already used with a different request body or actor",
        )
        .dry_run(effective_dry_run)),
    }
}

async fn get_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let protected = !state.config.force_dry_run || headers.contains_key(AUTHORIZATION_HEADER);
    let result = get_lobby_inner(&state, &lobby_id, &headers, protected).await;
    match result {
        Ok(response) if protected => sensitive_json_response(StatusCode::OK, &response),
        Ok(response) => Json(AnonymousDryLobbyResponse::from(response)).into_response(),
        Err(error) if protected => sensitive_response_headers(error.into_response()),
        Err(error) => error.into_response(),
    }
}

async fn get_lobby_inner(
    state: &AppState,
    lobby_id: &str,
    headers: &HeaderMap,
    protected: bool,
) -> Result<LobbyResponse, ApiError> {
    let header_dry_run = parse_dry_run_header(headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = if protected {
        LobbyId::parse(lobby_id).map_err(|_| lobby_not_found(false))?
    } else {
        parse_lobby_id(lobby_id, dry_hint)?
    };
    if protected {
        let verifier = capability_from_headers(headers).ok_or_else(|| lobby_not_found(false))?;
        state
            .store
            .get_authorized_network_view(lobby_id, verifier, state.clock.now())
            .await
            .ok_or_else(|| lobby_not_found(false))?;
    }
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let stored = load_maintained_lobby(state, lobby_id, now, dry_hint).await?;
    Ok(LobbyResponse {
        lobby: stored.snapshot(),
        metadata: metadata_for(stored.dry_run || dry_hint),
    })
}

async fn get_lobby_network(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match get_lobby_network_inner(&state, &lobby_id, &headers).await {
        Ok(view) => sensitive_json_response(StatusCode::OK, &view),
        Err(error) => sensitive_response_headers(error.into_response()),
    }
}

async fn get_lobby_network_inner(
    state: &AppState,
    lobby_id: &str,
    headers: &HeaderMap,
) -> Result<LobbyNetworkView, ApiError> {
    // Capability parsing and verification happen before any exact-lobby state is
    // returned. Every failure shares one 404 body to prevent existence oracles.
    let lobby_id = LobbyId::parse(lobby_id).map_err(|_| lobby_not_found(false))?;
    let verifier = capability_from_headers(headers).ok_or_else(|| lobby_not_found(false))?;
    let now = state.clock.now();
    let stored = state
        .store
        .get_authorized_network_view(lobby_id, verifier, now)
        .await
        .ok_or_else(|| lobby_not_found(false))?;
    let observation = state
        .provider_observations
        .read()
        .await
        .get(&lobby_id)
        .copied()
        .filter(|observation| observation.network_generation == stored.network_generation);
    build_network_view(&stored, observation, now).map_err(|_| internal_error(false))
}

async fn join_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<JoinLobbyRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    if actor != request.player_id {
        return Err(actor_mismatch(dry_hint));
    }

    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    let idempotency_key = require_idempotency_key(&headers, dry_run)?;
    let request_fingerprint = fingerprint(&request, dry_run)?;

    if let Some(replay) = stored.join_replays.get(&idempotency_key) {
        if replay.fingerprint != request_fingerprint || replay.player_id != actor {
            return Err(idempotency_conflict(dry_run));
        }
        return Ok(sensitive_json_response(
            StatusCode::OK,
            &JoinLobbyReplayResponse {
                join_credential: replay.receipt.clone(),
                lobby: stored.snapshot(),
                metadata: metadata_for(dry_run),
            },
        ));
    }

    request
        .validate(WIRE_VERSION)
        .map_err(|error| ApiError::validation(&error, dry_run))?;
    if request.authority_formula_version.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_authority_formula",
            "authority_formula_version must not be empty",
        )
        .dry_run(dry_run));
    }
    ensure_joinable(stored.lobby.state, dry_run)?;

    if let Some(existing) = stored
        .lobby
        .roster
        .iter()
        .find(|player| player.player_id == request.player_id)
    {
        if existing.display_name != request.display_name
            || existing.wire_version != request.client_wire_version
            || existing.formula_version != request.authority_formula_version
            || existing.horse_selection != request.horse_selection
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "duplicate_player",
                "player_id is already present with different join attributes",
            )
            .dry_run(dry_run));
        }
        if let Some(credential) = stored.credentials.get(&request.player_id) {
            if !credential.revoked && credential.expires_at > now {
                let receipt = credential.receipt();
                stored.join_replays.insert(
                    idempotency_key,
                    StoredJoinReplay {
                        fingerprint: request_fingerprint,
                        player_id: request.player_id,
                        receipt: receipt.clone(),
                        created_at: now,
                    },
                );
                state
                    .store
                    .replace(stored.clone())
                    .await
                    .map_err(|error| store_api_error(&error, dry_run))?;
                return Ok(sensitive_json_response(
                    StatusCode::OK,
                    &JoinLobbyReplayResponse {
                        join_credential: receipt,
                        lobby: stored.snapshot(),
                        metadata: metadata_for(dry_run),
                    },
                ));
            }
        }
    } else {
        if stored.lobby.roster.len() >= usize::from(stored.lobby.max_players) {
            return Err(
                ApiError::new(StatusCode::CONFLICT, "roster_full", "lobby roster is full")
                    .dry_run(dry_run),
            );
        }
        if stored
            .credentials
            .get(&request.player_id)
            .is_some_and(|credential| !credential.revoked && credential.expires_at > now)
        {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "credential_cleanup_pending",
                "a previous live credential must be revoked or expire before rejoin",
            )
            .dry_run(dry_run));
        }
    }

    if let Some(reservation) = stored.pending_issuances.get(&request.player_id) {
        if reservation.expires_at > now {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "credential_issuance_pending",
                "a prior credential issuance has an ambiguous outcome; retry after its expiry",
            )
            .dry_run(dry_run));
        }
    }
    enforce_join_rate_limit(&mut stored, request.player_id, now, dry_run)?;
    let provider_permit = state
        .provider_limit
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "provider_busy",
                "credential provider concurrency limit reached; retry later",
            )
            .dry_run(dry_run)
        })?;

    let expires_at = now.saturating_add(JOIN_CREDENTIAL_TTL_MS);
    stored.pending_issuances.insert(
        request.player_id,
        StoredIssuanceReservation {
            fingerprint: request_fingerprint.clone(),
            idempotency_key: idempotency_key.clone(),
            created_at: now,
            expires_at,
        },
    );
    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;

    let minted = tokio::time::timeout(
        PROVIDER_OPERATION_TIMEOUT,
        state.provider.mint_credential(MintCredentialRequest {
            lobby_id,
            network_generation: stored.network_generation,
            identity: provider_identity(&stored),
            mode: stored.lobby.provisioning_mode,
            player_id: request.player_id,
            tailnet: stored.tailnet.clone(),
            tag: stored.tag.clone(),
            expires_at,
            dry_run,
        }),
    )
    .await
    .unwrap_or(Err(ProviderError::Unavailable {
        operation: "auth_keys",
    }));
    drop(provider_permit);
    let minted = match minted {
        Ok(minted) => minted,
        Err(error) => {
            if !matches!(error, ProviderError::Unavailable { .. }) {
                stored.pending_issuances.remove(&request.player_id);
            }
            let reason = error.state_reason().to_owned();
            if matches!(stored.lobby.state, LobbyState::Forming | LobbyState::Ready) {
                transition(&mut stored.lobby, LobbyState::Failed, dry_run)?;
            }
            stored.lobby.state_reason = Some(reason.clone());
            stored.lobby.authority = None;
            stored.last_election = None;
            stored.terminal_at = Some(now);
            if matches!(
                error,
                ProviderError::ChildSecretUnavailable | ProviderError::IdentityMismatch
            ) {
                stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
            }
            stored.cleanup_pending = !dry_run
                || !stored.credentials.is_empty()
                || matches!(
                    error,
                    ProviderError::ChildSecretUnavailable | ProviderError::IdentityMismatch
                );
            if stored.cleanup_pending
                && stored.network_lifecycle != NetworkLifecycle::ManualRemediation
            {
                stored.network_lifecycle = NetworkLifecycle::CleanupPending;
            }
            state
                .store
                .replace(stored)
                .await
                .map_err(|store_error| store_api_error(&store_error, dry_run))?;
            return Err(provider_api_error(&error, dry_run).state_reason(reason));
        }
    };

    stored.pending_issuances.remove(&request.player_id);
    let is_new_player = !stored
        .lobby
        .roster
        .iter()
        .any(|player| player.player_id == request.player_id);
    if is_new_player {
        roster_changed(&mut stored, dry_run)?;
        stored.lobby.roster.push(Player {
            player_id: request.player_id,
            display_name: request.display_name,
            join_state: PlayerJoinState::CredentialIssued,
            wire_version: request.client_wire_version,
            formula_version: request.authority_formula_version,
            horse_selection: request.horse_selection,
            route_summary: Default::default(),
            joined_at: now,
            cleanup_pending: false,
        });
        stored
            .lobby
            .roster
            .sort_unstable_by_key(|player| player.player_id);
    } else if let Some(player) = stored
        .lobby
        .roster
        .iter_mut()
        .find(|player| player.player_id == request.player_id)
    {
        player.join_state = PlayerJoinState::CredentialIssued;
        stored.measurements.remove(&request.player_id);
        stored.lobby.authority = None;
        stored.last_election = None;
        if stored.lobby.state == LobbyState::Ready {
            transition(&mut stored.lobby, LobbyState::Forming, dry_run)?;
        }
    }
    refresh_idle_expiry(&mut stored, now);

    let credential = JoinCredential::new(
        minted.credential_id.clone(),
        minted.auth_key.into_exposed(),
        minted.tailnet,
        vec![stored.tag.clone()],
        expires_at,
    );
    debug_assert!(
        minted.metadata.dry_run || credential.expose_auth_key() != DRY_RUN_AUTH_KEY,
        "only dry-run may emit the synthetic credential marker"
    );
    let receipt = JoinCredentialReceipt::from(&credential);
    stored.credentials.insert(
        request.player_id,
        StoredCredential {
            credential_id: receipt.credential_id.clone(),
            expires_at,
            revoked: false,
            cleanup_pending: false,
            dry_run: minted.metadata.dry_run,
        },
    );
    stored.join_replays.insert(
        idempotency_key,
        StoredJoinReplay {
            fingerprint: request_fingerprint,
            player_id: request.player_id,
            receipt,
            created_at: now,
        },
    );
    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;

    Ok(sensitive_json_response(
        StatusCode::CREATED,
        &JoinLobbyResponse {
            join_credential: credential,
            lobby: stored.snapshot(),
            metadata: minted.metadata,
        },
    ))
}

async fn leave_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<LeaveLobbyRequest>, JsonRejection>,
) -> Result<Json<LeaveLobbyResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    if actor != request.player_id {
        return Err(actor_mismatch(dry_hint));
    }
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    if matches!(
        stored.lobby.state,
        LobbyState::Provisioning
            | LobbyState::Starting
            | LobbyState::Closing
            | LobbyState::Failed
            | LobbyState::Expired
            | LobbyState::Destroyed
    ) {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "lobby does not accept leaves in its current state",
            dry_run,
        ));
    }
    if !stored
        .lobby
        .roster
        .iter()
        .any(|player| player.player_id == request.player_id)
    {
        return Ok(Json(LeaveLobbyResponse {
            left: true,
            cleanup_pending: false,
            metadata: metadata_for(dry_run),
        }));
    }

    let cleanup = provider_cleanup(
        &state,
        cleanup_request_for_player(&stored, request.player_id, now, false),
    )
    .await;
    let metadata = cleanup.as_ref().map_or_else(
        |_| metadata_for(dry_run),
        |outcome| outcome.metadata.clone(),
    );
    apply_cleanup_result(&mut stored, now, cleanup, false);
    stored
        .lobby
        .roster
        .retain(|player| player.player_id != request.player_id);
    stored.measurements.clear();
    stored.pending_issuances.remove(&request.player_id);
    stored
        .join_replays
        .retain(|_, replay| replay.player_id != request.player_id);
    stored.lobby.authority = None;
    stored.last_election = None;
    if stored.lobby.state == LobbyState::Ready {
        transition(&mut stored.lobby, LobbyState::Forming, dry_run)?;
    }
    refresh_idle_expiry(&mut stored, now);
    let credential_pending = stored
        .credentials
        .get(&request.player_id)
        .is_some_and(|credential| !credential.revoked && credential.expires_at > now);
    // The current Tailscale device list does not expose a trustworthy
    // player-to-device association. Keep real leave cleanup visibly queued
    // rather than deleting every device carrying the shared lobby tag.
    let cleanup_pending = credential_pending || !dry_run;
    stored.cleanup_pending |= cleanup_pending;
    state
        .store
        .replace(stored)
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    Ok(Json(LeaveLobbyResponse {
        left: true,
        cleanup_pending,
        metadata,
    }))
}

async fn submit_measurements(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<SubmitMeasurementsRequest>, JsonRejection>,
) -> Result<Json<SubmitMeasurementsResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    if actor != request.player_id {
        return Err(actor_mismatch(dry_hint));
    }

    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    if !stored.lobby.state.accepts_measurements() {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "lobby does not accept measurements in its current state",
            dry_run,
        ));
    }
    let roster_size = stored.lobby.roster.len();
    let player_id = request.player_id;
    if !stored
        .lobby
        .roster
        .iter()
        .any(|player| player.player_id == player_id)
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "player_not_found",
            "player_id is not a member of this lobby",
        )
        .dry_run(dry_run));
    }
    let sample = request
        .into_validated_sample(now, roster_size)
        .map_err(|error| ApiError::validation(&error, dry_run))?;

    if stored.lobby.state == LobbyState::Ready {
        transition(&mut stored.lobby, LobbyState::Forming, dry_run)?;
    }
    let player = stored
        .lobby
        .roster
        .iter_mut()
        .find(|player| player.player_id == player_id)
        .expect("membership was checked immediately above");
    player.route_summary = sample.route_summary;
    player.join_state = PlayerJoinState::Connected;
    stored.measurements.insert(player_id, sample);
    refresh_idle_expiry(&mut stored, now);
    if stored.lobby.state == LobbyState::InMatch {
        let _ = migrate_silent_authority(&mut stored, now, dry_run)?;
    } else {
        let _ = recompute_authority(&mut stored, now, dry_run)?;
    }

    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    Ok(Json(SubmitMeasurementsResponse {
        accepted: true,
        state: stored.lobby.state,
        authority: stored.lobby.authority.clone(),
        metadata: metadata_for(dry_run),
    }))
}

async fn elect_lobby_authority(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<AuthorityEnvelope>, ApiError> {
    authority_handler(state, lobby_id, headers).await
}

async fn get_lobby_authority(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let lobby_id = LobbyId::parse(&lobby_id).map_err(|_| lobby_not_found(false))?;
    let verifier = capability_from_headers(&headers).ok_or_else(|| lobby_not_found(false))?;
    let now = state.clock.now();
    let stored = state
        .store
        .get_authorized_network_view(lobby_id, verifier, now)
        .await
        .ok_or_else(|| lobby_not_found(false))?;
    let election = stored
        .last_election
        .as_ref()
        .ok_or_else(|| lobby_not_found(false))?;
    Ok(sensitive_json_response(
        StatusCode::OK,
        &AuthorityEnvelope {
            authority: AuthorityResponse::from(election),
            metadata: metadata_for(stored.dry_run),
        },
    ))
}

async fn authority_handler(
    state: AppState,
    lobby_id: String,
    headers: HeaderMap,
) -> Result<Json<AuthorityEnvelope>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    if !stored.lobby.state.accepts_measurements() {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "authority cannot be elected in the current lobby state",
            dry_run,
        ));
    }

    if stored.lobby.state == LobbyState::InMatch {
        let _ = migrate_silent_authority(&mut stored, now, dry_run)?;
    } else if stored.last_election.is_none() {
        let _ = recompute_authority(&mut stored, now, dry_run)?;
    }
    let election = stored.last_election.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "authority_unavailable",
            "every roster member must submit a fresh measurement before election",
        )
        .dry_run(dry_run)
    })?;
    state
        .store
        .replace(stored)
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    Ok(Json(AuthorityEnvelope {
        authority: AuthorityResponse::from(&election),
        metadata: metadata_for(dry_run),
    }))
}

async fn start_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<StartLobbyRequest>, JsonRejection>,
) -> Result<Json<StartLobbyResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    if actor != request.creator_player_id {
        return Err(actor_mismatch(dry_hint));
    }
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    ensure_creator(&stored, actor, dry_run)?;
    let idempotency_key = require_idempotency_key(&headers, dry_run)?;
    let request_fingerprint = fingerprint(&request, dry_run)?;

    if let Some(replay) = stored.start_replays.get(&idempotency_key) {
        if replay.actor != actor || replay.fingerprint != request_fingerprint {
            return Err(idempotency_conflict(dry_run));
        }
        if matches!(
            stored.lobby.state,
            LobbyState::Starting | LobbyState::InMatch
        ) {
            return Ok(Json(start_response(&stored, dry_run)?));
        }
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "started lobby is already closed",
            dry_run,
        ));
    }
    if matches!(
        stored.lobby.state,
        LobbyState::Starting | LobbyState::InMatch
    ) {
        if request
            .map_seed
            .is_some_and(|seed| stored.lobby.map_seed != Some(seed))
        {
            return Err(idempotency_conflict(dry_run));
        }
        stored.start_replays.insert(
            idempotency_key,
            StoredMutationReplay {
                fingerprint: request_fingerprint,
                actor,
                created_at: now,
            },
        );
        state
            .store
            .replace(stored.clone())
            .await
            .map_err(|error| store_api_error(&error, dry_run))?;
        return Ok(Json(start_response(&stored, dry_run)?));
    }
    if stored.lobby.state != LobbyState::Ready {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "lobby must be READY before start",
            dry_run,
        ));
    }
    if stored.lobby.roster.len() < usize::from(PROTOTYPE_MIN_PLAYERS) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_enough_players",
            "prototype start requires at least two players",
        )
        .dry_run(dry_run));
    }
    validate_start_roster(&stored.lobby.roster)
        .map_err(|error| ApiError::validation(&error, dry_run))?;
    if !all_measurements_fresh(&stored, now) {
        transition(&mut stored.lobby, LobbyState::Forming, dry_run)?;
        stored.lobby.authority = None;
        stored.last_election = None;
        state
            .store
            .replace(stored)
            .await
            .map_err(|error| store_api_error(&error, dry_run))?;
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "authority_unavailable",
            "all roster measurements must be fresher than 60 seconds",
        )
        .dry_run(dry_run));
    }
    if stored.last_election.is_none() || stored.lobby.authority.is_none() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "authority_unavailable",
            "a deterministic authority winner is required before start",
        )
        .dry_run(dry_run));
    }

    stored.lobby.map_seed = Some(request.map_seed.unwrap_or_else(random_map_seed));
    transition(&mut stored.lobby, LobbyState::Starting, dry_run)?;
    stored.started_at = Some(now);
    stored.last_authority_heartbeat_at = None;
    stored.start_replays.insert(
        idempotency_key,
        StoredMutationReplay {
            fingerprint: request_fingerprint,
            actor,
            created_at: now,
        },
    );
    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    Ok(Json(start_response(&stored, dry_run)?))
}

async fn authority_heartbeat(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<AuthorityHeartbeatRequest>, JsonRejection>,
) -> Result<Json<AuthorityHeartbeatResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    if actor != request.player_id {
        return Err(actor_mismatch(dry_hint));
    }
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    if !matches!(
        stored.lobby.state,
        LobbyState::Starting | LobbyState::InMatch
    ) {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "heartbeat is accepted only while STARTING or IN_MATCH",
            dry_run,
        ));
    }
    let election = stored.last_election.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "authority_unavailable",
            "lobby has no authority election",
        )
        .dry_run(dry_run)
    })?;
    if election.winner_player_id != actor || election.input_hash != request.input_hash {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_authority",
            "heartbeat actor or input hash does not match the current authority",
        )
        .dry_run(dry_run));
    }
    if stored.lobby.state == LobbyState::Starting {
        transition(&mut stored.lobby, LobbyState::InMatch, dry_run)?;
    }
    stored.last_authority_heartbeat_at = Some(now);
    stored.last_accepted_heartbeat = Some(StoredAcceptedHeartbeat {
        player_id: actor,
        epoch: stored.authority_epoch,
        input_hash: request.input_hash,
        received_at: now,
    });
    let authority = stored
        .lobby
        .authority
        .clone()
        .ok_or_else(|| internal_error(dry_run))?;
    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    Ok(Json(AuthorityHeartbeatResponse {
        accepted: true,
        state: stored.lobby.state,
        authority,
        metadata: metadata_for(dry_run),
    }))
}

async fn submit_results(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<SubmitResultsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    if actor != request.submitted_by {
        return Err(actor_mismatch(dry_hint));
    }
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    let idempotency_key = require_idempotency_key(&headers, dry_run)?;
    let request_fingerprint = fingerprint(&request, dry_run)?;
    if let Some(replay) = stored.results_replays.get(&idempotency_key) {
        if replay.actor != actor || replay.fingerprint != request_fingerprint {
            return Err(idempotency_conflict(dry_run));
        }
        return Ok((
            StatusCode::ACCEPTED,
            Json(SubmitResultsResponse {
                accepted: true,
                state: LobbyState::Closing,
                metadata: metadata_for(dry_run),
            }),
        )
            .into_response());
    }
    if stored.lobby.state != LobbyState::InMatch {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "results are accepted only while IN_MATCH",
            dry_run,
        ));
    }
    let election = stored
        .last_election
        .as_ref()
        .ok_or_else(|| internal_error(dry_run))?;
    if election.winner_player_id != request.submitted_by {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_authority",
            "results must be submitted by the last known authority",
        )
        .dry_run(dry_run));
    }
    if election.input_hash != request.input_hash {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "authority_input_mismatch",
            "results input_hash does not match the last authority election",
        )
        .dry_run(dry_run));
    }
    validate_results(&stored, &request, dry_run)?;
    transition(&mut stored.lobby, LobbyState::Closing, dry_run)?;
    stored.results_replays.insert(
        idempotency_key,
        StoredMutationReplay {
            fingerprint: request_fingerprint,
            actor,
            created_at: now,
        },
    );
    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    let metadata = cleanup_resources(&state, &mut stored, now, true, true).await;
    state
        .store
        .replace(stored)
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitResultsResponse {
            accepted: true,
            state: LobbyState::Closing,
            metadata,
        }),
    )
        .into_response())
}

async fn delete_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DestroyLobbyResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers, state.config.force_dry_run)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let actor = require_actor(&headers, dry_hint)?;
    let _lobby = state.lock_lobby(lobby_id).await;
    let now = state.clock.now();
    let mut stored = load_maintained_lobby(&state, lobby_id, now, dry_hint).await?;
    let dry_run = mutation_dry_run(&stored, header_dry_run, state.config.force_dry_run)?;
    // Authorization is deliberately checked before destroyed replay or provider work.
    ensure_creator(&stored, actor, dry_run)?;

    if stored.lobby.state == LobbyState::Destroyed && !stored.cleanup_pending {
        return Ok(Json(DestroyLobbyResponse {
            state: LobbyState::Destroyed,
            cleanup_pending: false,
            metadata: metadata_for(dry_run),
        }));
    }
    if !matches!(
        stored.lobby.state,
        LobbyState::Closing | LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
    ) {
        transition(&mut stored.lobby, LobbyState::Closing, dry_run)?;
        state
            .store
            .replace(stored.clone())
            .await
            .map_err(|error| store_api_error(&error, dry_run))?;
    }
    let metadata = cleanup_resources(&state, &mut stored, now, true, true).await;
    let cleanup_pending = stored.cleanup_pending;
    state
        .store
        .replace(stored)
        .await
        .map_err(|error| store_api_error(&error, dry_run))?;

    Ok(Json(DestroyLobbyResponse {
        state: LobbyState::Destroyed,
        cleanup_pending,
        metadata,
    }))
}

#[derive(Serialize)]
struct AuthorityEnvelope {
    #[serde(flatten)]
    authority: AuthorityResponse,
    #[serde(flatten)]
    metadata: ResponseMetadata,
}

fn capability_verifier(token: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(CAPABILITY_HASH_DOMAIN.len() + token.len());
    input.extend_from_slice(CAPABILITY_HASH_DOMAIN);
    input.extend_from_slice(token);
    let verifier = sha256(&input);
    input.zeroize();
    verifier
}

fn capability_from_headers(headers: &HeaderMap) -> Option<[u8; 32]> {
    let value = headers.get(AUTHORIZATION_HEADER)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme != CAPABILITY_SCHEME || token.is_empty() || token.contains(char::is_whitespace) {
        return None;
    }
    let mut decoded = base64url_decode(token)?;
    if decoded.len() != 48 {
        decoded.zeroize();
        return None;
    }
    decoded.zeroize();
    Some(capability_verifier(token.as_bytes()))
}

fn sensitive_json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    sensitive_response_headers((status, Json(value)).into_response())
}

fn sensitive_response_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Authorization"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn provider_identity(stored: &StoredLobby) -> Option<ProviderNetworkIdentity> {
    let identity = stored.network_identity.as_ref()?;
    if stored.network_generation == 0
        || identity.network_generation != stored.network_generation
        || identity.tailnet_dns_name.as_str() != stored.tailnet
    {
        return None;
    }
    let identity = ProviderNetworkIdentity {
        provider_tailnet_id: identity.provider_tailnet_id.clone(),
        tailnet_dns_name: identity.tailnet_dns_name.clone(),
    };
    identity
        .validate_for_mode(stored.lobby.provisioning_mode)
        .ok()?;
    Some(identity)
}

fn observe_request(stored: &StoredLobby) -> Result<ObserveNetworkRequest, ProviderError> {
    let identity = provider_identity(stored).ok_or(ProviderError::IdentityMismatch)?;
    if identity.tailnet_dns_name.as_str() != stored.tailnet {
        return Err(ProviderError::IdentityMismatch);
    }
    Ok(ObserveNetworkRequest {
        lobby_id: stored.lobby.lobby_id,
        network_generation: stored.network_generation,
        mode: stored.lobby.provisioning_mode,
        identity: Some(identity),
        tailnet: stored.tailnet.clone(),
        tag: stored.tag.clone(),
        dry_run: stored.dry_run,
    })
}

const fn observation_failure(error: &ProviderError) -> CachedObservationFailure {
    match error {
        ProviderError::RealMutationsDisabled => CachedObservationFailure::SourceError,
        ProviderError::InsufficientScopes { .. } => CachedObservationFailure::PermissionDenied,
        ProviderError::IdentityMismatch => CachedObservationFailure::Conflict,
        ProviderError::ChildSecretUnavailable
        | ProviderError::Upstream { .. }
        | ProviderError::Unavailable { .. } => CachedObservationFailure::SourceError,
    }
}

fn control_fact<T>(value: T, as_of: UnixMillis, served_at: UnixMillis) -> Fact<T> {
    Fact::known(
        value,
        FactSource::ControlStore,
        FactAssurance::Authoritative,
        Some(as_of),
        served_at,
        Freshness::Current,
    )
}

fn derived_fact<T>(value: T, served_at: UnixMillis) -> Fact<T> {
    Fact::known(
        value,
        FactSource::Derived,
        FactAssurance::Derived,
        Some(served_at),
        served_at,
        Freshness::Fresh,
    )
}

fn unknown_control<T>(reason: UnknownReason, received_at: Option<UnixMillis>) -> Fact<T> {
    Fact::unknown(FactSource::ControlStore, reason, received_at)
}

fn provider_enrollment_fact(
    observation: Option<CachedProviderObservation>,
    now: UnixMillis,
) -> Fact<u32> {
    let Some(observation) = observation else {
        return Fact::unknown(FactSource::ProviderApi, UnknownReason::NeverObserved, None);
    };
    match (observation.enrolled_device_count, observation.successful_at) {
        (Some(value), Some(successful_at)) => {
            let failed_after_success = observation
                .last_failure
                .is_some_and(|(_, failed_at)| failed_at >= successful_at);
            let fresh = !failed_after_success
                && now
                    .checked_duration_since(successful_at)
                    .is_some_and(|age| age < PROVIDER_DEVICE_FRESHNESS_MS);
            Fact::known(
                value,
                FactSource::ProviderApi,
                FactAssurance::Observed,
                Some(successful_at),
                successful_at,
                if fresh {
                    Freshness::Fresh
                } else {
                    Freshness::Stale
                },
            )
        }
        _ => {
            let (failure, failed_at) = observation
                .last_failure
                .unwrap_or((CachedObservationFailure::SourceError, now));
            Fact::unknown(
                FactSource::ProviderApi,
                failure.unknown_reason(),
                Some(failed_at),
            )
        }
    }
}

fn provider_online_fact(observation: Option<CachedProviderObservation>) -> Fact<u32> {
    let received_at = observation.and_then(|observation| {
        observation
            .last_failure
            .map(|(_, at)| at)
            .or(observation.successful_at)
    });
    Fact::unknown(
        FactSource::ProviderApi,
        UnknownReason::Unsupported,
        received_at,
    )
}

fn cleanup_time_fact(value: Option<UnixMillis>, now: UnixMillis) -> Fact<UnixMillis> {
    value.map_or_else(
        || unknown_control(UnknownReason::NeverObserved, None),
        |value| control_fact(value, value, now),
    )
}

fn build_network_view(
    stored: &StoredLobby,
    observation: Option<CachedProviderObservation>,
    now: UnixMillis,
) -> Result<LobbyNetworkView, ()> {
    let dry_run = stored.dry_run || stored.lobby.provisioning_mode == ProvisioningMode::DryRun;
    let identity = provider_identity(stored);
    let identity_required = !dry_run
        && matches!(
            stored.network_lifecycle,
            NetworkLifecycle::Active
                | NetworkLifecycle::CleanupRequested
                | NetworkLifecycle::CleanupPending
                | NetworkLifecycle::VerifyingAbsence
        );
    let lifecycle = if identity_required && identity.is_none() {
        NetworkLifecycle::ManualRemediation
    } else if dry_run {
        NetworkLifecycle::Simulated
    } else {
        stored.network_lifecycle
    };

    let (truth_label, mut backing) = if dry_run {
        (
            NetworkTruthLabel::SimulatedNoTailnet,
            NetworkBacking::simulated(stored.network_generation.max(1), now),
        )
    } else {
        let (truth_label, backing_mode, isolation) = match stored.lobby.provisioning_mode {
            ProvisioningMode::TailnetPerLobby => (
                NetworkTruthLabel::RealDedicatedTailnet,
                BackingMode::TailnetPerLobby,
                NetworkIsolation::Dedicated,
            ),
            ProvisioningMode::SharedTailnet => (
                NetworkTruthLabel::RealSharedCompatibility,
                BackingMode::SharedTailnet,
                NetworkIsolation::Shared,
            ),
            ProvisioningMode::DryRun => return Err(()),
        };
        let tailnet_dns_name = if matches!(
            lifecycle,
            NetworkLifecycle::CreateRejected
                | NetworkLifecycle::DedicatedAbsent
                | NetworkLifecycle::SharedResourcesClean
        ) {
            Fact::not_applicable()
        } else if let (Some(identity), Some(stored_identity)) =
            (identity.as_ref(), stored.network_identity.as_ref())
        {
            Fact::known(
                identity.tailnet_dns_name.clone(),
                FactSource::ControlStore,
                FactAssurance::Authoritative,
                Some(stored_identity.captured_at),
                stored_identity.captured_at,
                Freshness::Current,
            )
        } else {
            unknown_control(UnknownReason::ReconciliationPending, None)
        };
        (
            truth_label,
            NetworkBacking {
                backing_mode,
                simulates_mode: None,
                isolation,
                network_generation: stored.network_generation,
                network_lifecycle: lifecycle,
                tailnet_dns_name,
                control_service_member: control_fact(false, now, now),
            },
        )
    };
    backing.network_lifecycle = lifecycle;

    let roster_count = u32::try_from(stored.lobby.roster.len()).map_err(|_| ())?;
    let expected_directions = roster_count
        .checked_mul(roster_count.saturating_sub(1))
        .ok_or(())?;
    let roster_ids: BTreeSet<PlayerId> = stored
        .lobby
        .roster
        .iter()
        .map(|player| player.player_id)
        .collect();
    let fresh_measurements: Vec<_> = stored
        .measurements
        .iter()
        .filter(|(player_id, sample)| {
            roster_ids.contains(player_id)
                && now
                    .checked_duration_since(sample.measured_at)
                    .is_some_and(|age| age < INSPECTOR_REPORT_FRESHNESS_MS)
        })
        .map(|(_, sample)| sample)
        .collect();
    // The current measurement route uses client-asserted player identifiers;
    // it is not the generation/session-bound participant-report contract.
    // Do not relabel those rows as authenticated fresh reporters.
    let fresh_reporter_count = 0;
    let (direct_count, peer_relay_count, derp_relay_count, fresh_directions) = fresh_measurements
        .iter()
        .try_fold((0_u32, 0_u32, 0_u32, 0_u32), |counts, sample| {
            Some((
                counts.0.checked_add(sample.route_summary.direct_count)?,
                counts
                    .1
                    .checked_add(sample.route_summary.peer_relay_count)?,
                counts.2.checked_add(sample.route_summary.derp_count)?,
                counts.3.checked_add(sample.observed_peer_count)?,
            ))
        })
        .ok_or(())?;
    if fresh_directions > expected_directions {
        return Err(());
    }
    let unknown_count = expected_directions - fresh_directions;
    let unavailable_count = 0;
    let reported_direction_count = fresh_directions.checked_add(unknown_count).ok_or(())?;
    let reachable_known_count = direct_count
        .checked_add(peer_relay_count)
        .and_then(|value| value.checked_add(derp_relay_count))
        .ok_or(())?;
    let direct_ratio_milli = if reachable_known_count == 0 {
        Fact::not_applicable()
    } else {
        let ratio =
            u32::try_from((1_000_u64 * u64::from(direct_count)) / u64::from(reachable_known_count))
                .map_err(|_| ())?;
        derived_fact(ratio, now)
    };

    let (provider_enrolled_device_count, provider_online_device_count) = if dry_run {
        (Fact::not_applicable(), Fact::not_applicable())
    } else {
        (
            provider_enrollment_fact(observation, now),
            provider_online_fact(observation),
        )
    };

    let control_election = stored.last_election.as_ref().map_or_else(
        || Fact::unknown(FactSource::Derived, UnknownReason::NeverObserved, None),
        |election| {
            let evaluated_at = election.input.election_at;
            let freshness = if now
                .checked_duration_since(evaluated_at)
                .is_some_and(|age| age < MEASUREMENT_FRESHNESS_MS)
            {
                Freshness::Fresh
            } else {
                Freshness::Stale
            };
            let score_milli = election
                .eligible
                .iter()
                .find(|score| score.player_id == election.winner_player_id)
                .map_or(0, |score| score.score_milli);
            Fact::known(
                ControlElectionReference {
                    formula_version: election.formula_version.clone(),
                    winner_player_id: election.winner_player_id,
                    score_milli,
                    input_hash: election.input_hash,
                    evaluated_at,
                    degraded: election.degraded,
                    input_assurance: FactAssurance::Reported,
                },
                FactSource::Derived,
                FactAssurance::Authoritative,
                Some(evaluated_at),
                evaluated_at,
                freshness,
            )
        },
    );
    let last_accepted_heartbeat = stored.last_accepted_heartbeat.map_or_else(
        || unknown_control(UnknownReason::NeverObserved, None),
        |heartbeat| {
            Fact::known(
                AcceptedHeartbeatReference {
                    player_id: heartbeat.player_id,
                    epoch: heartbeat.epoch,
                    input_hash: heartbeat.input_hash,
                    received_at: heartbeat.received_at,
                },
                FactSource::ControlStore,
                FactAssurance::Authoritative,
                Some(heartbeat.received_at),
                heartbeat.received_at,
                if now
                    .checked_duration_since(heartbeat.received_at)
                    .is_some_and(|age| age < MEASUREMENT_FRESHNESS_MS)
                {
                    Freshness::Fresh
                } else {
                    Freshness::Stale
                },
            )
        },
    );

    let cleanup = if dry_run {
        NetworkCleanup {
            network_lifecycle: NetworkLifecycle::Simulated,
            requested_at: Fact::not_applicable(),
            delete_acknowledged_at: Fact::not_applicable(),
            absence_confirmed_at: Fact::not_applicable(),
            participant_safe_reason: ParticipantCleanupReason::SimulatedNoTailnet,
        }
    } else {
        let participant_safe_reason = match lifecycle {
            NetworkLifecycle::Simulated => return Err(()),
            NetworkLifecycle::Reserved
            | NetworkLifecycle::Creating
            | NetworkLifecycle::Active
            | NetworkLifecycle::CreateRejected => ParticipantCleanupReason::NotRequested,
            NetworkLifecycle::CreateUnknown | NetworkLifecycle::ManualRemediation => {
                ParticipantCleanupReason::ManualRemediationRequired
            }
            NetworkLifecycle::CleanupRequested => ParticipantCleanupReason::CleanupRequested,
            NetworkLifecycle::CleanupPending => ParticipantCleanupReason::CleanupPending,
            NetworkLifecycle::VerifyingAbsence => ParticipantCleanupReason::VerifyingExactAbsence,
            NetworkLifecycle::DedicatedAbsent => ParticipantCleanupReason::DedicatedTailnetAbsent,
            NetworkLifecycle::SharedResourcesClean => {
                ParticipantCleanupReason::SharedResourcesClean
            }
            NetworkLifecycle::Unknown => return Err(()),
        };
        NetworkCleanup {
            network_lifecycle: lifecycle,
            requested_at: cleanup_time_fact(stored.cleanup_requested_at, now),
            delete_acknowledged_at: cleanup_time_fact(stored.delete_acknowledged_at, now),
            absence_confirmed_at: cleanup_time_fact(stored.absence_confirmed_at, now),
            participant_safe_reason,
        }
    };

    let view = LobbyNetworkView {
        schema_version: LOBBY_NETWORK_SCHEMA_VERSION,
        lobby_id: stored.lobby.lobby_id,
        served_at: now,
        truth_label,
        backing,
        lobby_lifecycle: control_fact(InspectedLobbyLifecycle::from(stored.lobby.state), now, now),
        counts: NetworkCounts {
            roster_count: control_fact(roster_count, now, now),
            provider_enrolled_device_count,
            provider_online_device_count,
            fresh_reporter_count: derived_fact(fresh_reporter_count, now),
            fresh_directional_observation_count: derived_fact(fresh_directions, now),
        },
        routes: RouteAggregate {
            expected_direction_count: derived_fact(expected_directions, now),
            reported_direction_count: derived_fact(reported_direction_count, now),
            direct_count: derived_fact(direct_count, now),
            peer_relay_count: derived_fact(peer_relay_count, now),
            derp_relay_count: derived_fact(derp_relay_count, now),
            unavailable_count: derived_fact(unavailable_count, now),
            unknown_count: derived_fact(unknown_count, now),
            reachable_known_count: derived_fact(reachable_known_count, now),
            direct_ratio_milli,
        },
        application_quality: ApplicationQuality {
            sample_count: derived_fact(0, now),
            application_rtt_ms_median: Fact::unknown(
                FactSource::ParticipantReport,
                UnknownReason::Unsupported,
                None,
            ),
            application_rtt_ms_p95: Fact::unknown(
                FactSource::ParticipantReport,
                UnknownReason::Unsupported,
                None,
            ),
            application_rtt_ms_worst: Fact::unknown(
                FactSource::ParticipantReport,
                UnknownReason::Unsupported,
                None,
            ),
            application_loss_ppm_median: Fact::unknown(
                FactSource::ParticipantReport,
                UnknownReason::Unsupported,
                None,
            ),
        },
        authority: NetworkAuthority {
            control_election,
            last_accepted_heartbeat,
            peer_reported_match_authority: Fact::unknown(
                FactSource::ParticipantReport,
                UnknownReason::NeverObserved,
                None,
            ),
        },
        cleanup,
    };
    view.validate().map_err(|_| ())?;
    Ok(view)
}

fn create_response(stored: &StoredLobby, metadata: ResponseMetadata) -> CreateLobbyResponse {
    CreateLobbyResponse {
        lobby_id: stored.lobby.lobby_id,
        state: LobbyState::Provisioning,
        wire_version: stored.lobby.wire_version,
        created_at: stored.lobby.created_at,
        expires_at: stored.lobby.ttl.absolute_expires_at,
        metadata,
    }
}

fn start_response(stored: &StoredLobby, dry_run: bool) -> Result<StartLobbyResponse, ApiError> {
    Ok(StartLobbyResponse {
        lobby_id: stored.lobby.lobby_id,
        state: stored.lobby.state,
        map_seed: stored
            .lobby
            .map_seed
            .ok_or_else(|| internal_error(dry_run))?,
        authority: stored
            .lobby
            .authority
            .clone()
            .ok_or_else(|| internal_error(dry_run))?,
        metadata: metadata_for(dry_run),
    })
}

fn parse_json<T>(payload: Result<Json<T>, JsonRejection>, dry_run: bool) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|rejection| {
        let status = rejection.status();
        let (code, message) = match status {
            StatusCode::PAYLOAD_TOO_LARGE => (
                "payload_too_large",
                "request body exceeds the 64 KiB service limit",
            ),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => (
                "unsupported_media_type",
                "request content type must be application/json",
            ),
            _ => (
                "invalid_json",
                "request body must be valid JSON with the expected shape",
            ),
        };
        ApiError::new(status, code, message).dry_run(dry_run)
    })
}

fn parse_lobby_id(value: &str, dry_run: bool) -> Result<LobbyId, ApiError> {
    LobbyId::parse(value).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_lobby_id",
            "lobby_id must be a canonical UUIDv4",
        )
        .dry_run(dry_run)
    })
}

fn require_actor(headers: &HeaderMap, dry_run: bool) -> Result<PlayerId, ApiError> {
    let value = headers.get(ACTOR_HEADER).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing_actor",
            "X-Spurfire-Player-Id header is required for this prototype action",
        )
        .dry_run(dry_run)
    })?;
    let value = value.to_str().map_err(|_| actor_mismatch(dry_run))?;
    PlayerId::parse(value).map_err(|_| actor_mismatch(dry_run))
}

fn require_idempotency_key(headers: &HeaderMap, dry_run: bool) -> Result<String, ApiError> {
    let value = headers.get(IDEMPOTENCY_KEY).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_idempotency_key",
            "Idempotency-Key header is required",
        )
        .dry_run(dry_run)
    })?;
    let value = value.to_str().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key must contain visible UTF-8 text",
        )
        .dry_run(dry_run)
    })?;
    if value.trim().is_empty() || value.len() > MAX_IDEMPOTENCY_KEY_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key must contain 1 to 128 bytes",
        )
        .dry_run(dry_run));
    }
    Ok(value.to_owned())
}

fn parse_dry_run_header(headers: &HeaderMap, force_dry_run: bool) -> Result<bool, ApiError> {
    let Some(value) = headers.get(DRY_RUN_HEADER) else {
        return Ok(false);
    };
    match value.to_str() {
        Ok("1") => Ok(true),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_dry_run_header",
            "X-Spurfire-Dry-Run must be exactly 1 when present",
        )
        .dry_run(force_dry_run)),
    }
}

fn mutation_dry_run(
    stored: &StoredLobby,
    header_dry_run: bool,
    force_dry_run: bool,
) -> Result<bool, ApiError> {
    if header_dry_run && !stored.dry_run && !force_dry_run {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "dry_run_mode_mismatch",
            "request-scoped dry-run cannot mutate an existing real lobby",
        )
        .dry_run(true));
    }
    Ok(stored.dry_run || force_dry_run)
}

fn effective_request_dry_run(
    state: &AppState,
    request: &CreateLobbyRequest,
    header_dry_run: bool,
) -> bool {
    state.config.force_dry_run
        || header_dry_run
        || request.provisioning_mode == ProvisioningMode::DryRun
}

fn fingerprint<T: Serialize>(value: &T, dry_run: bool) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec(value).map_err(|_| internal_error(dry_run))
}

fn new_lobby_id() -> LobbyId {
    LobbyId::parse(&Uuid::new_v4().to_string()).expect("uuid crate generated UUIDv4")
}

fn random_map_seed() -> u64 {
    let id = Uuid::new_v4();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&id.as_bytes()[..8]);
    u64::from_be_bytes(bytes)
}

fn lobby_tag(lobby_id: LobbyId) -> String {
    format!("tag:spurfire-lobby-{lobby_id}")
}

fn metadata_for(dry_run: bool) -> ResponseMetadata {
    ResponseMetadata {
        dry_run,
        planned_actions: Vec::new(),
    }
}

async fn load_maintained_lobby(
    state: &AppState,
    lobby_id: LobbyId,
    now: UnixMillis,
    dry_hint: bool,
) -> Result<StoredLobby, ApiError> {
    let mut stored = state
        .store
        .get(lobby_id)
        .await
        .ok_or_else(|| lobby_not_found(dry_hint))?;
    let mut transitioned = false;
    let requires_provider_access = stored.cleanup_pending
        || !matches!(
            stored.lobby.state,
            LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
        );
    if let Some(error) = requires_provider_access
        .then(|| {
            state.provider.lobby_access_error(
                stored.lobby.lobby_id,
                stored.lobby.provisioning_mode,
                stored.network_lifecycle,
                stored.dry_run,
            )
        })
        .flatten()
    {
        if !matches!(
            stored.lobby.state,
            LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
        ) && stored
            .lobby
            .state
            .validate_transition(LobbyState::Failed)
            .is_ok()
        {
            stored.lobby.state = LobbyState::Failed;
        }
        stored.lobby.state_reason = Some(error.state_reason().to_owned());
        stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
        stored.lobby.authority = None;
        stored.last_election = None;
        stored.cleanup_pending = true;
        stored.terminal_at.get_or_insert(now);
        transitioned = true;
    }
    transitioned |= apply_time_transitions(&mut stored, now);
    let freshness_changed = refresh_freshness_state(&mut stored, now);
    if transitioned || freshness_changed {
        state
            .store
            .replace(stored.clone())
            .await
            .map_err(|error| store_api_error(&error, stored.dry_run || dry_hint))?;
    }
    if transitioned {
        let _ = cleanup_resources(state, &mut stored, now, true, false).await;
        state
            .store
            .replace(stored.clone())
            .await
            .map_err(|error| store_api_error(&error, stored.dry_run || dry_hint))?;
    }
    Ok(stored)
}

fn refresh_freshness_state(stored: &mut StoredLobby, now: UnixMillis) -> bool {
    if stored.lobby.state == LobbyState::Ready
        && !all_measurements_fresh(stored, now)
        && stored
            .lobby
            .state
            .validate_transition(LobbyState::Forming)
            .is_ok()
    {
        stored.lobby.state = LobbyState::Forming;
        stored.lobby.authority = None;
        stored.last_election = None;
        return true;
    }
    false
}

fn refresh_idle_expiry(stored: &mut StoredLobby, now: UnixMillis) {
    stored.lobby.ttl.idle_expires_at = now
        .saturating_add(stored.idle_ttl_ms)
        .min(stored.lobby.ttl.absolute_expires_at);
}

fn roster_changed(stored: &mut StoredLobby, dry_run: bool) -> Result<(), ApiError> {
    if stored.lobby.state == LobbyState::Ready {
        transition(&mut stored.lobby, LobbyState::Forming, dry_run)?;
    }
    stored.measurements.clear();
    stored.lobby.authority = None;
    stored.last_election = None;
    Ok(())
}

fn ensure_joinable(state: LobbyState, dry_run: bool) -> Result<(), ApiError> {
    if matches!(state, LobbyState::Forming | LobbyState::Ready) {
        return Ok(());
    }
    Err(lobby_closed_or_transition(
        state,
        "lobby does not accept joins in its current state",
        dry_run,
    ))
}

fn ensure_creator(stored: &StoredLobby, actor: PlayerId, dry_run: bool) -> Result<(), ApiError> {
    if stored.creator_player_id == actor {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::FORBIDDEN,
        "not_creator",
        "action is restricted to the lobby creator",
    )
    .dry_run(dry_run))
}

fn actor_mismatch(dry_run: bool) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "actor_mismatch",
        "request actor does not match the client-asserted player identity",
    )
    .dry_run(dry_run)
}

fn idempotency_conflict(dry_run: bool) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "idempotency_conflict",
        "Idempotency-Key was already used with a different request body or actor",
    )
    .dry_run(dry_run)
}

fn lobby_closed_or_transition(state: LobbyState, message: &str, dry_run: bool) -> ApiError {
    let code = if matches!(
        state,
        LobbyState::Closing | LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
    ) {
        "lobby_closed"
    } else {
        "lobby_not_joinable"
    };
    ApiError::new(StatusCode::CONFLICT, code, message).dry_run(dry_run)
}

fn transition(lobby: &mut Lobby, next: LobbyState, dry_run: bool) -> Result<(), ApiError> {
    lobby.state.validate_transition(next).map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "invalid_state_transition",
            "requested operation is not valid in the current lobby state",
        )
        .dry_run(dry_run)
    })?;
    lobby.state = next;
    Ok(())
}

fn enforce_join_rate_limit(
    stored: &mut StoredLobby,
    player_id: PlayerId,
    now: UnixMillis,
    dry_run: bool,
) -> Result<(), ApiError> {
    while stored.join_attempts.front().is_some_and(|attempt| {
        now.checked_duration_since(attempt.attempted_at)
            .is_some_and(|age| age >= JOIN_RATE_WINDOW_MS)
    }) {
        stored.join_attempts.pop_front();
    }
    let player_attempts = stored
        .join_attempts
        .iter()
        .filter(|attempt| attempt.player_id == player_id)
        .count();
    if stored.join_attempts.len() >= MAX_JOIN_ATTEMPTS_PER_LOBBY
        || player_attempts >= MAX_JOIN_ATTEMPTS_PER_PLAYER
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "join_rate_limited",
            "join mint rate limit exceeded; retry after 60 seconds",
        )
        .dry_run(dry_run));
    }
    stored.join_attempts.push_back(StoredJoinAttempt {
        player_id,
        attempted_at: now,
    });
    Ok(())
}

fn authority_candidates(stored: &StoredLobby) -> Vec<AuthorityCandidate> {
    stored
        .lobby
        .roster
        .iter()
        .filter_map(|player| {
            stored
                .measurements
                .get(&player.player_id)
                .cloned()
                .map(|measurement| AuthorityCandidate {
                    player_id: player.player_id,
                    wire_version: player.wire_version,
                    joined_at: player.joined_at,
                    measurement,
                })
        })
        .collect()
}

fn all_measurements_fresh(stored: &StoredLobby, now: UnixMillis) -> bool {
    stored.lobby.roster.len() >= usize::from(PROTOTYPE_MIN_PLAYERS)
        && stored.lobby.roster.iter().all(|player| {
            stored
                .measurements
                .get(&player.player_id)
                .and_then(|sample| now.checked_duration_since(sample.measured_at))
                .is_some_and(|age| age < MEASUREMENT_FRESHNESS_MS)
        })
}

fn recompute_authority(
    stored: &mut StoredLobby,
    now: UnixMillis,
    dry_run: bool,
) -> Result<Option<AuthorityElection>, ApiError> {
    if !all_measurements_fresh(stored, now) {
        stored.lobby.authority = None;
        stored.last_election = None;
        return Ok(None);
    }
    let candidates = authority_candidates(stored);
    let election =
        elect_authority(&candidates, now).map_err(|error| authority_error(&error, dry_run))?;
    apply_election(stored, election.clone(), now, dry_run)?;
    Ok(Some(election))
}

fn migrate_silent_authority(
    stored: &mut StoredLobby,
    now: UnixMillis,
    dry_run: bool,
) -> Result<Option<AuthorityElection>, ApiError> {
    let Some(current) = stored
        .lobby
        .authority
        .as_ref()
        .map(|authority| authority.candidate_player_id)
    else {
        return Ok(None);
    };
    let silent = stored
        .last_authority_heartbeat_at
        .or(stored.started_at)
        .and_then(|heartbeat| now.checked_duration_since(heartbeat))
        .is_some_and(|age| age >= AUTHORITY_SILENCE_MS);
    if !silent {
        return Ok(stored.last_election.clone());
    }
    let candidates: Vec<_> = authority_candidates(stored)
        .into_iter()
        .filter(|candidate| candidate.player_id != current)
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    let election =
        elect_authority_for_roster(&candidates, now, WIRE_VERSION, stored.lobby.roster.len())
            .map_err(|error| authority_error(&error, dry_run))?;
    apply_election(stored, election.clone(), now, dry_run)?;
    stored.last_authority_heartbeat_at = Some(now);
    Ok(Some(election))
}

fn apply_election(
    stored: &mut StoredLobby,
    election: AuthorityElection,
    now: UnixMillis,
    dry_run: bool,
) -> Result<(), ApiError> {
    let score_milli = election
        .eligible
        .iter()
        .find(|score| score.player_id == election.winner_player_id)
        .map_or(0, |score| score.score_milli);
    let election_changed = stored.last_election.as_ref().is_none_or(|previous| {
        previous.winner_player_id != election.winner_player_id
            || previous.input_hash != election.input_hash
    });
    if election_changed {
        stored.authority_epoch = stored.authority_epoch.saturating_add(1).max(1);
    }
    stored.lobby.authority = Some(AuthoritySummary {
        candidate_player_id: election.winner_player_id,
        formula_version: election.formula_version.clone(),
        score_milli,
    });
    stored.last_election = Some(election);
    if stored.lobby.state == LobbyState::Forming && all_measurements_fresh(stored, now) {
        transition(&mut stored.lobby, LobbyState::Ready, dry_run)?;
    }
    Ok(())
}

fn authority_error(error: &AuthorityElectionError, dry_run: bool) -> ApiError {
    let message = match error {
        AuthorityElectionError::NotEnoughCandidates => {
            "authority election requires at least one migration candidate or two normal candidates"
        }
        AuthorityElectionError::DuplicatePlayer { .. } => {
            "authority input contains a duplicate player"
        }
        AuthorityElectionError::InvalidRosterSize { .. } => {
            "authority input roster context is invalid"
        }
        AuthorityElectionError::NoFreshCompleteCandidates => {
            "no fresh complete measurement rows are available"
        }
    };
    ApiError::new(StatusCode::CONFLICT, "authority_unavailable", message).dry_run(dry_run)
}

fn cleanup_request(
    stored: &StoredLobby,
    now: UnixMillis,
    include_devices: bool,
) -> CleanupLobbyRequest {
    CleanupLobbyRequest {
        lobby_id: stored.lobby.lobby_id,
        network_generation: stored.network_generation,
        identity: provider_identity(stored),
        mode: stored.lobby.provisioning_mode,
        tailnet: stored.tailnet.clone(),
        tag: stored.tag.clone(),
        credentials: stored
            .credentials
            .values()
            .filter(|credential| !credential.revoked)
            .map(|credential| CredentialCleanup {
                credential_id: credential.credential_id.clone(),
                expires_at: credential.expires_at,
            })
            .collect(),
        include_devices,
        now,
        dry_run: stored.dry_run,
    }
}

fn cleanup_request_for_player(
    stored: &StoredLobby,
    player_id: PlayerId,
    now: UnixMillis,
    include_devices: bool,
) -> CleanupLobbyRequest {
    let mut request = cleanup_request(stored, now, include_devices);
    request.credentials = stored
        .credentials
        .get(&player_id)
        .filter(|credential| !credential.revoked)
        .map(|credential| {
            vec![CredentialCleanup {
                credential_id: credential.credential_id.clone(),
                expires_at: credential.expires_at,
            }]
        })
        .unwrap_or_default();
    request
}

async fn provider_cleanup(
    state: &AppState,
    request: CleanupLobbyRequest,
) -> Result<CleanupOutcome, ProviderError> {
    tokio::time::timeout(PROVIDER_OPERATION_TIMEOUT, async {
        let _permit =
            state
                .provider_limit
                .acquire()
                .await
                .map_err(|_| ProviderError::Unavailable {
                    operation: "cleanup",
                })?;
        state.provider.cleanup_lobby(request).await
    })
    .await
    .unwrap_or(Err(ProviderError::Unavailable {
        operation: "cleanup",
    }))
}

async fn cleanup_resources(
    state: &AppState,
    stored: &mut StoredLobby,
    now: UnixMillis,
    include_devices: bool,
    finalize: bool,
) -> ResponseMetadata {
    // Serialize invalidation with provider observation refresh so a poll that
    // started before teardown cannot repopulate the cache afterward.
    let _observation = state.lock_observation(stored.lobby.lobby_id).await;
    let dry_run = stored.dry_run;
    if stored.cleanup_requested_at.is_none() {
        stored.measurements.clear();
        for player in &mut stored.lobby.roster {
            player.route_summary = Default::default();
        }
        state
            .provider_observations
            .write()
            .await
            .remove(&stored.lobby.lobby_id);
    }

    if stored.lobby.provisioning_mode == ProvisioningMode::TailnetPerLobby
        && stored.network_lifecycle == NetworkLifecycle::VerifyingAbsence
    {
        reconcile_dedicated_absence(state, stored, now).await;
        finalize_lobby_cleanup(stored, now, finalize);
        return metadata_for(dry_run);
    }
    if matches!(
        stored.network_lifecycle,
        NetworkLifecycle::DedicatedAbsent | NetworkLifecycle::SharedResourcesClean
    ) {
        finalize_lobby_cleanup(stored, now, finalize);
        return metadata_for(dry_run);
    }
    if !dry_run
        && stored.lobby.provisioning_mode == ProvisioningMode::TailnetPerLobby
        && matches!(
            stored.network_lifecycle,
            NetworkLifecycle::CreateUnknown | NetworkLifecycle::ManualRemediation
        )
    {
        stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
        stored.cleanup_pending = true;
        finalize_lobby_cleanup(stored, now, finalize);
        return metadata_for(false);
    }

    if !dry_run {
        stored.cleanup_requested_at.get_or_insert(now);
        stored.network_lifecycle = NetworkLifecycle::CleanupRequested;
        stored.cleanup_pending = true;
        // No destructive call is allowed unless cleanup intent and the held
        // identity/generation are durably replaceable first.
        if state.store.replace(stored.clone()).await.is_err() {
            stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
            stored.lobby.state_reason = Some("cleanup_intent_persistence_failed".to_owned());
            finalize_lobby_cleanup(stored, now, finalize);
            return metadata_for(false);
        }
    }

    let result = provider_cleanup(state, cleanup_request(stored, now, include_devices)).await;
    let metadata = result.as_ref().map_or_else(
        |_| metadata_for(dry_run),
        |outcome| outcome.metadata.clone(),
    );
    apply_cleanup_result(stored, now, result, true);
    finalize_lobby_cleanup(stored, now, finalize);
    metadata
}

fn finalize_lobby_cleanup(stored: &mut StoredLobby, now: UnixMillis, finalize: bool) {
    if finalize && stored.lobby.state != LobbyState::Destroyed {
        if stored
            .lobby
            .state
            .validate_transition(LobbyState::Destroyed)
            .is_ok()
        {
            stored.lobby.state = LobbyState::Destroyed;
        }
        if !stored.cleanup_pending {
            stored.lobby.state_reason = None;
        }
        stored.lobby.authority = None;
        stored.terminal_at = Some(now);
    }
}

async fn reconcile_dedicated_absence(state: &AppState, stored: &mut StoredLobby, now: UnixMillis) {
    let Some(identity) = provider_identity(stored) else {
        stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
        stored.cleanup_pending = true;
        stored.lobby.state_reason = Some("provider_identity_missing_manual_remediation".to_owned());
        return;
    };
    let request = TailnetPresenceRequest {
        lobby_id: stored.lobby.lobby_id,
        network_generation: stored.network_generation,
        identity,
    };
    let present = tokio::time::timeout(
        PROVIDER_OBSERVATION_TIMEOUT,
        state.provider.tailnet_present(request.clone()),
    )
    .await;
    match present {
        Ok(Ok(true)) => {
            stored.first_absence_observed_at = None;
            stored.cleanup_pending = true;
        }
        Ok(Ok(false)) => match stored.first_absence_observed_at {
            None => {
                stored.first_absence_observed_at = Some(now);
                stored.cleanup_pending = true;
            }
            Some(first)
                if now
                    .checked_duration_since(first)
                    .is_some_and(|age| age >= CLEANUP_ABSENCE_MIN_SEPARATION_MS) =>
            {
                // Absence evidence is complete; only now may the exact
                // generation-bound vault tuple be erased. Failure remains
                // fail-closed and retains the singleton lease.
                match tokio::time::timeout(
                    PROVIDER_OPERATION_TIMEOUT,
                    state.provider.erase_child_secret(request),
                )
                .await
                {
                    Ok(Ok(())) => {
                        stored.child_secret_erased_at = Some(now);
                        stored.absence_confirmed_at = Some(now);
                        stored.network_lifecycle = NetworkLifecycle::DedicatedAbsent;
                        stored.cleanup_pending = false;
                        stored.lobby.state_reason = None;
                    }
                    Ok(Err(error)) => {
                        stored.cleanup_pending = true;
                        stored.lobby.state_reason = Some(error.state_reason().to_owned());
                    }
                    Err(_) => {
                        stored.cleanup_pending = true;
                        stored.lobby.state_reason = Some("child_secret_erasure_pending".to_owned());
                    }
                }
            }
            Some(_) => {
                stored.cleanup_pending = true;
            }
        },
        Ok(Err(ProviderError::IdentityMismatch)) => {
            stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
            stored.cleanup_pending = true;
            stored.lobby.state_reason =
                Some("provider_identity_mismatch_manual_remediation".to_owned());
        }
        Ok(Err(_)) | Err(_) => {
            // Preserve the last absence evidence without converting uncertainty
            // into absence. The singleton lease remains held.
            stored.cleanup_pending = true;
        }
    }
}

fn apply_cleanup_result(
    stored: &mut StoredLobby,
    now: UnixMillis,
    result: Result<CleanupOutcome, ProviderError>,
    full_cleanup: bool,
) {
    if let Err(error @ (ProviderError::ChildSecretUnavailable | ProviderError::IdentityMismatch)) =
        &result
    {
        stored.lobby.state_reason = Some(error.state_reason().to_owned());
    }
    if let Ok(outcome) = &result {
        let revoked: BTreeSet<&str> = outcome
            .revoked_credential_ids
            .iter()
            .map(String::as_str)
            .collect();
        for credential in stored.credentials.values_mut() {
            if credential.expires_at <= now || revoked.contains(credential.credential_id.as_str()) {
                credential.revoked = true;
                credential.cleanup_pending = false;
            }
        }
        if full_cleanup && outcome.delete_acknowledged {
            stored.delete_acknowledged_at.get_or_insert(now);
        }
        if full_cleanup && outcome.child_secret_erased {
            stored.child_secret_erased_at.get_or_insert(now);
        }
    }
    let credential_pending = stored
        .credentials
        .values_mut()
        .filter(|credential| !credential.revoked && credential.expires_at > now)
        .any(|credential| {
            credential.cleanup_pending = true;
            true
        });
    let provider_pending = result
        .as_ref()
        .map_or(true, |outcome| outcome.cleanup_pending);

    if full_cleanup && !stored.dry_run {
        match stored.lobby.provisioning_mode {
            ProvisioningMode::TailnetPerLobby => match &result {
                Ok(outcome) if outcome.delete_acknowledged => {
                    stored.network_lifecycle = NetworkLifecycle::VerifyingAbsence;
                    stored.cleanup_pending = true;
                }
                Err(ProviderError::IdentityMismatch | ProviderError::ChildSecretUnavailable) => {
                    stored.network_lifecycle = NetworkLifecycle::ManualRemediation;
                    stored.cleanup_pending = true;
                }
                _ => {
                    stored.network_lifecycle = NetworkLifecycle::CleanupPending;
                    stored.cleanup_pending = true;
                }
            },
            ProvisioningMode::SharedTailnet => {
                if !provider_pending && !credential_pending {
                    stored.network_lifecycle = NetworkLifecycle::SharedResourcesClean;
                    stored.cleanup_pending = false;
                } else {
                    stored.network_lifecycle = NetworkLifecycle::CleanupPending;
                    stored.cleanup_pending = true;
                }
            }
            ProvisioningMode::DryRun => unreachable!("real record cannot use dry-run mode"),
        }
    } else {
        stored.cleanup_pending = provider_pending || credential_pending;
    }

    for player in &mut stored.lobby.roster {
        player.cleanup_pending = stored.cleanup_pending;
        if stored.cleanup_pending {
            player.join_state = PlayerJoinState::CleanupPending;
        } else if matches!(
            stored.lobby.state,
            LobbyState::Closing | LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed
        ) {
            player.join_state = PlayerJoinState::Left;
        }
    }
}

fn validate_results(
    stored: &StoredLobby,
    request: &SubmitResultsRequest,
    dry_run: bool,
) -> Result<(), ApiError> {
    if request.match_duration_s == 0 || request.match_duration_s > MAX_MATCH_DURATION_S {
        return Err(invalid_results(
            "match_duration_s must be between 1 and 3600",
            dry_run,
        ));
    }
    if request.final_scores.len() != stored.lobby.roster.len() {
        return Err(invalid_results(
            "final_scores must contain exactly one row per roster member",
            dry_run,
        ));
    }
    let roster: BTreeSet<PlayerId> = stored
        .lobby
        .roster
        .iter()
        .map(|player| player.player_id)
        .collect();
    let mut score_players = BTreeSet::new();
    for FinalScore {
        player_id,
        score,
        eliminations,
        assists,
        deaths,
    } in &request.final_scores
    {
        if !roster.contains(player_id)
            || !score_players.insert(*player_id)
            || !(-MAX_FINAL_SCORE_ABS..=MAX_FINAL_SCORE_ABS).contains(score)
            || *eliminations > 100_000
            || *assists > 100_000
            || *deaths > 100_000
        {
            return Err(invalid_results(
                "final score rows failed membership or plausibility validation",
                dry_run,
            ));
        }
    }
    let mut co_signers = BTreeSet::new();
    if request.co_signers.iter().any(|player_id| {
        !roster.contains(player_id)
            || *player_id == request.submitted_by
            || !co_signers.insert(*player_id)
    }) {
        return Err(invalid_results(
            "co_signers must be unique non-authority roster members",
            dry_run,
        ));
    }
    Ok(())
}

fn invalid_results(message: &str, dry_run: bool) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_results", message).dry_run(dry_run)
}

fn provider_api_error(error: &ProviderError, dry_run: bool) -> ApiError {
    let (status, code, message) = match error {
        ProviderError::RealMutationsDisabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            "real_mutations_disabled",
            "real provider mutations are disabled by the independent safety switch",
        ),
        ProviderError::IdentityMismatch => (
            StatusCode::SERVICE_UNAVAILABLE,
            "manual_remediation_required",
            "provider identity evidence does not match the selected lobby generation",
        ),
        ProviderError::ChildSecretUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "manual_remediation_required",
            "provider credentials needed for this lobby are unavailable after restart",
        ),
        ProviderError::InsufficientScopes { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_scopes_insufficient",
            "network provider scopes do not permit this operation",
        ),
        ProviderError::Upstream { .. } => (
            StatusCode::BAD_GATEWAY,
            "provider_error",
            "network provider rejected the operation",
        ),
        ProviderError::Unavailable { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_unavailable",
            "network provider is unavailable",
        ),
    };
    ApiError::new(status, code, message)
        .dry_run(dry_run)
        .state_reason(error.state_reason())
}

fn store_api_error(error: &crate::store::StoreError, dry_run: bool) -> ApiError {
    match error {
        crate::store::StoreError::Capacity => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_capacity_reached",
            "lobby service capacity is temporarily exhausted",
        )
        .dry_run(dry_run),
        crate::store::StoreError::RealMutationsDisabled => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "real_mutations_disabled",
            "real provider mutations are disabled by the independent safety switch",
        )
        .dry_run(dry_run),
        crate::store::StoreError::RealLobbyCapacityReached => ApiError::new(
            StatusCode::CONFLICT,
            "real_lobby_capacity_reached",
            "the single real-lobby safety lease is currently held",
        )
        .dry_run(dry_run),
        _ => internal_error(dry_run),
    }
}

fn internal_error(dry_run: bool) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "the service could not complete the request",
    )
    .dry_run(dry_run)
}

fn lobby_not_found(dry_run: bool) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "lobby_not_found",
        "lobby_id does not exist",
    )
    .dry_run(dry_run)
}

async fn not_found(State(state): State<AppState>) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "route_not_found",
        "requested route does not exist",
    )
    .dry_run(state.config.force_dry_run)
}

async fn method_not_allowed(State(state): State<AppState>) -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "HTTP method is not allowed for this route",
    )
    .dry_run(state.config.force_dry_run)
}
