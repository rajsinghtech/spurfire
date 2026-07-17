//! Axum router and lobby application service.

use std::{fmt, sync::Arc};

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use spurfire_protocol::{
    elect_authority, AuthorityCandidate, AuthorityElection, AuthorityElectionError,
    AuthorityResponse, AuthoritySummary, CreateLobbyRequest, CreateLobbyResponse,
    DestroyLobbyResponse, JoinCredential, JoinCredentialReceipt, JoinLobbyReplayResponse,
    JoinLobbyRequest, JoinLobbyResponse, Lobby, LobbyId, LobbyResponse, LobbyState, LobbyTtl,
    Player, PlayerJoinState, ProvisioningMode, ResponseMetadata, SubmitMeasurementsRequest,
    SubmitMeasurementsResponse, UnixMillis, AUTHORITY_FORMULA_VERSION, DRY_RUN_AUTH_KEY,
    IDLE_TTL_MS, JOIN_CREDENTIAL_TTL_MS, MEASUREMENT_FRESHNESS_MS, PROTOTYPE_MIN_PLAYERS,
    WIRE_VERSION,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    clock::{Clock, SystemClock},
    config::Config,
    error::ApiError,
    provider::{
        CleanupLobbyRequest, CleanupOutcome, MintCredentialRequest, NetworkProvider,
        PrepareLobbyRequest, ProviderError,
    },
    store::{CreateStoreOutcome, LobbyStore, StoredCredential, StoredJoinReplay, StoredLobby},
};

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const DRY_RUN_HEADER: &str = "x-spurfire-dry-run";
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const DRY_RUN_MAX_TTL_MS: u64 = 5 * 60 * 1_000;

/// Cloneable application dependencies shared by every Axum handler.
#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    store: Arc<dyn LobbyStore>,
    provider: Arc<dyn NetworkProvider>,
    clock: Arc<dyn Clock>,
    /// Serializes read/modify/provider/write sequences in this prototype store.
    operations: Arc<Mutex<()>>,
}

impl AppState {
    /// Creates application state using the production wall clock.
    #[must_use]
    pub fn new(
        config: Config,
        store: Arc<dyn LobbyStore>,
        provider: Arc<dyn NetworkProvider>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            store,
            provider,
            clock: Arc::new(SystemClock),
            operations: Arc::new(Mutex::new(())),
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

    /// Runs deterministic expiry transitions and best-effort teardown.
    pub async fn cleanup_expired_at(&self, now: UnixMillis) -> Vec<LobbyId> {
        let _operation = self.operations.lock().await;
        let lobby_ids = self.store.cleanup_expired(now).await;
        for lobby_id in &lobby_ids {
            let Some(mut stored) = self.store.get(*lobby_id).await else {
                continue;
            };
            revoke_stored_credentials(&mut stored);
            let cleanup = self
                .provider
                .cleanup_lobby(cleanup_request(&stored, stored.dry_run))
                .await;
            apply_cleanup_result(&mut stored, cleanup);
            let _ = self.store.replace(stored).await;
        }
        lobby_ids
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
            .finish()
    }
}

/// Builds the complete HTTP router.
#[must_use]
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/lobbies", post(create_lobby))
        .route(
            "/v1/lobbies/{lobby_id}",
            get(get_lobby).delete(delete_lobby),
        )
        .route("/v1/lobbies/{lobby_id}/join", post(join_lobby))
        .route(
            "/v1/lobbies/{lobby_id}/measurements",
            post(submit_measurements),
        )
        .route(
            "/v1/lobbies/{lobby_id}/elect-authority",
            post(elect_lobby_authority),
        )
        .route("/v1/lobbies/{lobby_id}/authority", get(get_lobby_authority))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(64 * 1_024))
        .with_state(state)
}

/// Alias that reads naturally in embedders.
#[must_use]
pub fn router(state: AppState) -> Router {
    build_router(state)
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_lobby(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CreateLobbyRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers)?;
    let request = parse_json(payload, state.config.force_dry_run || header_dry_run)?;
    let idempotency_key = require_idempotency_key(&headers)?;
    request.validate().map_err(|error| {
        ApiError::validation(
            &error,
            effective_request_dry_run(&state, &request, header_dry_run),
        )
    })?;

    let effective_dry_run = effective_request_dry_run(&state, &request, header_dry_run);
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
    let fingerprint = fingerprint(&(request.clone(), effective_mode))?;
    let now = state.clock.now();
    let lobby_id = new_lobby_id();
    let prepared = state
        .provider
        .prepare_lobby(PrepareLobbyRequest {
            lobby_id,
            mode: effective_mode,
            dry_run: effective_dry_run,
        })
        .await
        .map_err(|error| provider_api_error(&error, effective_dry_run))?;

    let absolute_ttl_ms = if prepared.dry_run {
        state.config.default_ttl_ms().min(DRY_RUN_MAX_TTL_MS)
    } else {
        state.config.default_ttl_ms()
    };
    let idle_ttl_ms = IDLE_TTL_MS.min(absolute_ttl_ms);
    let absolute_expires_at = now.saturating_add(absolute_ttl_ms);
    let lobby = Lobby {
        lobby_id,
        display_name: request.display_name.clone(),
        // Preparation is record-local and complete, but create intentionally reports
        // PROVISIONING once. The first GET observes this stored FORMING state.
        state: LobbyState::Forming,
        state_reason: None,
        roster: Vec::new(),
        max_players: request.max_players,
        map_seed: None,
        authority: None,
        ttl: LobbyTtl {
            idle_expires_at: now.saturating_add(idle_ttl_ms),
            absolute_expires_at,
        },
        wire_version: WIRE_VERSION,
        provisioning_mode: effective_mode,
        created_at: now,
    };
    let tag = lobby_tag(lobby_id);
    let stored = StoredLobby::new(lobby, prepared.tailnet, tag, prepared.dry_run, idle_ttl_ms);

    let outcome = state
        .store
        .create(idempotency_key, fingerprint, stored)
        .await
        .map_err(|_| internal_error(prepared.dry_run))?;
    match outcome {
        CreateStoreOutcome::Created(stored) => Ok((
            StatusCode::CREATED,
            Json(create_response(&stored, prepared.metadata)),
        )
            .into_response()),
        CreateStoreOutcome::Replay(stored) => Ok((
            StatusCode::OK,
            Json(create_response(&stored, metadata_for(stored.dry_run))),
        )
            .into_response()),
        CreateStoreOutcome::Conflict => Err(ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Idempotency-Key was already used with a different request body",
        )
        .dry_run(effective_dry_run)),
    }
}

async fn get_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
) -> Result<Json<LobbyResponse>, ApiError> {
    let lobby_id = parse_lobby_id(&lobby_id, state.config.force_dry_run)?;
    let now = state.clock.now();
    let _ = state.store.cleanup_expired(now).await;
    let stored = state
        .store
        .get(lobby_id)
        .await
        .ok_or_else(|| lobby_not_found(state.config.force_dry_run))?;
    Ok(Json(LobbyResponse {
        lobby: stored.snapshot(),
        metadata: metadata_for(stored.dry_run),
    }))
}

async fn join_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<JoinLobbyRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;
    let idempotency_key = require_idempotency_key(&headers)?;
    let fingerprint = fingerprint(&(request.clone(), dry_hint))?;

    let _operation = state.operations.lock().await;
    let now = state.clock.now();
    let _ = state.store.cleanup_expired(now).await;
    let mut stored = state
        .store
        .get(lobby_id)
        .await
        .ok_or_else(|| lobby_not_found(dry_hint))?;
    let dry_run = stored.dry_run || dry_hint;

    if let Some(replay) = stored.join_replays.get(&idempotency_key) {
        if replay.fingerprint != fingerprint || replay.player_id != request.player_id {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency-Key was already used with a different request body or actor",
            )
            .dry_run(dry_run));
        }
        return Ok((
            StatusCode::OK,
            Json(JoinLobbyReplayResponse {
                join_credential: replay.receipt.clone(),
                lobby: stored.snapshot(),
                metadata: metadata_for(dry_run),
            }),
        )
            .into_response());
    }

    request
        .validate(WIRE_VERSION)
        .map_err(|error| ApiError::validation(&error, dry_run))?;
    ensure_joinable(stored.lobby.state, dry_run)?;

    if let Some(existing) = stored
        .lobby
        .roster
        .iter()
        .find(|player| player.player_id == request.player_id)
    {
        if existing.display_name != request.display_name
            || existing.wire_version != request.client_wire_version
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
                        fingerprint,
                        player_id: request.player_id,
                        receipt: receipt.clone(),
                    },
                );
                state
                    .store
                    .replace(stored.clone())
                    .await
                    .map_err(|_| internal_error(dry_run))?;
                return Ok((
                    StatusCode::OK,
                    Json(JoinLobbyReplayResponse {
                        join_credential: receipt,
                        lobby: stored.snapshot(),
                        metadata: metadata_for(dry_run),
                    }),
                )
                    .into_response());
            }
        }
    } else if stored.lobby.roster.len() >= usize::from(stored.lobby.max_players) {
        return Err(
            ApiError::new(StatusCode::CONFLICT, "roster_full", "lobby roster is full")
                .dry_run(dry_run),
        );
    }

    let expires_at = now.saturating_add(JOIN_CREDENTIAL_TTL_MS);
    let minted = state
        .provider
        .mint_credential(MintCredentialRequest {
            lobby_id,
            player_id: request.player_id,
            tailnet: stored.tailnet.clone(),
            tag: stored.tag.clone(),
            expires_at,
            dry_run,
        })
        .await;
    let minted = match minted {
        Ok(minted) => minted,
        Err(error) => {
            let reason = error.state_reason().to_owned();
            stored.lobby.state = LobbyState::Failed;
            stored.lobby.state_reason = Some(reason.clone());
            stored.lobby.authority = None;
            state
                .store
                .replace(stored)
                .await
                .map_err(|_| internal_error(dry_run))?;
            return Err(provider_api_error(&error, dry_run).state_reason(reason));
        }
    };

    let is_new_player = !stored
        .lobby
        .roster
        .iter()
        .any(|player| player.player_id == request.player_id);
    if is_new_player {
        if stored.lobby.state == LobbyState::Ready {
            transition(&mut stored.lobby, LobbyState::Forming, dry_run)?;
        }
        stored.lobby.roster.push(Player {
            player_id: request.player_id,
            display_name: request.display_name,
            join_state: PlayerJoinState::CredentialIssued,
            wire_version: request.client_wire_version,
            formula_version: AUTHORITY_FORMULA_VERSION.to_owned(),
            horse_selection: request.horse_selection,
            route_summary: Default::default(),
            joined_at: now,
            cleanup_pending: false,
        });
        stored
            .lobby
            .roster
            .sort_unstable_by_key(|player| player.player_id);
        stored.lobby.authority = None;
    } else if let Some(player) = stored
        .lobby
        .roster
        .iter_mut()
        .find(|player| player.player_id == request.player_id)
    {
        player.join_state = PlayerJoinState::CredentialIssued;
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
            dry_run: minted.metadata.dry_run,
        },
    );
    stored.join_replays.insert(
        idempotency_key,
        StoredJoinReplay {
            fingerprint,
            player_id: request.player_id,
            receipt,
        },
    );
    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|_| internal_error(dry_run))?;

    Ok((
        StatusCode::CREATED,
        Json(JoinLobbyResponse {
            join_credential: credential,
            lobby: stored.snapshot(),
            metadata: minted.metadata,
        }),
    )
        .into_response())
}

async fn submit_measurements(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<SubmitMeasurementsRequest>, JsonRejection>,
) -> Result<Json<SubmitMeasurementsResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;
    let request = parse_json(payload, dry_hint)?;

    let _operation = state.operations.lock().await;
    let now = state.clock.now();
    let _ = state.store.cleanup_expired(now).await;
    let mut stored = state
        .store
        .get(lobby_id)
        .await
        .ok_or_else(|| lobby_not_found(dry_hint))?;
    let dry_run = stored.dry_run || dry_hint;
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
    let _ = recompute_authority(&mut stored, now, dry_run)?;

    state
        .store
        .replace(stored.clone())
        .await
        .map_err(|_| internal_error(dry_run))?;
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
) -> Result<Json<AuthorityEnvelope>, ApiError> {
    authority_handler(state, lobby_id, headers).await
}

async fn authority_handler(
    state: AppState,
    lobby_id: String,
    headers: HeaderMap,
) -> Result<Json<AuthorityEnvelope>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;

    let _operation = state.operations.lock().await;
    let now = state.clock.now();
    let _ = state.store.cleanup_expired(now).await;
    let mut stored = state
        .store
        .get(lobby_id)
        .await
        .ok_or_else(|| lobby_not_found(dry_hint))?;
    let dry_run = stored.dry_run || dry_hint;
    if !stored.lobby.state.accepts_measurements() {
        return Err(lobby_closed_or_transition(
            stored.lobby.state,
            "authority cannot be elected in the current lobby state",
            dry_run,
        ));
    }
    let candidates = authority_candidates(&stored);
    if candidates.len() != stored.lobby.roster.len() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "authority_unavailable",
            "every roster member must submit measurements before election",
        )
        .dry_run(dry_run));
    }
    let election =
        elect_authority(&candidates, now).map_err(|error| authority_error(&error, dry_run))?;
    apply_election(&mut stored, &election, now, dry_run)?;
    state
        .store
        .replace(stored)
        .await
        .map_err(|_| internal_error(dry_run))?;
    Ok(Json(AuthorityEnvelope {
        authority: AuthorityResponse::from(&election),
        metadata: metadata_for(dry_run),
    }))
}

async fn delete_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DestroyLobbyResponse>, ApiError> {
    let header_dry_run = parse_dry_run_header(&headers)?;
    let dry_hint = state.config.force_dry_run || header_dry_run;
    let lobby_id = parse_lobby_id(&lobby_id, dry_hint)?;

    let _operation = state.operations.lock().await;
    let now = state.clock.now();
    let _ = state.store.cleanup_expired(now).await;
    let mut stored = state
        .store
        .get(lobby_id)
        .await
        .ok_or_else(|| lobby_not_found(dry_hint))?;
    let dry_run = stored.dry_run || dry_hint;

    if stored.lobby.state == LobbyState::Destroyed && !stored.cleanup_pending {
        return Ok(Json(DestroyLobbyResponse {
            state: LobbyState::Destroyed,
            cleanup_pending: false,
            metadata: metadata_for(dry_run),
        }));
    }
    match stored.lobby.state {
        LobbyState::Forming | LobbyState::Ready | LobbyState::InMatch => {
            transition(&mut stored.lobby, LobbyState::Closing, dry_run)?;
        }
        LobbyState::Starting => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "invalid_state_transition",
                "starting lobby cannot be destroyed until startup resolves",
            )
            .dry_run(dry_run));
        }
        LobbyState::Provisioning => {
            transition(&mut stored.lobby, LobbyState::Failed, dry_run)?;
            stored.lobby.state_reason = Some("destroyed_during_provisioning".to_owned());
        }
        LobbyState::Closing | LobbyState::Failed | LobbyState::Expired | LobbyState::Destroyed => {}
    }

    revoke_stored_credentials(&mut stored);
    let cleanup = state
        .provider
        .cleanup_lobby(cleanup_request(&stored, dry_run))
        .await;
    let metadata = cleanup.as_ref().map_or_else(
        |_| metadata_for(dry_run),
        |outcome| outcome.metadata.clone(),
    );
    apply_cleanup_result(&mut stored, cleanup);

    if stored.lobby.state == LobbyState::Closing {
        transition(&mut stored.lobby, LobbyState::Destroyed, dry_run)?;
    } else {
        // FAILED/EXPIRED are terminal observations, but explicit deletion finalizes
        // retained cleanup metadata into the idempotent DESTROYED tombstone.
        stored.lobby.state = LobbyState::Destroyed;
    }
    stored.lobby.state_reason = None;
    stored.lobby.authority = None;
    let cleanup_pending = stored.cleanup_pending;
    state
        .store
        .replace(stored)
        .await
        .map_err(|_| internal_error(dry_run))?;

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

fn parse_json<T>(payload: Result<Json<T>, JsonRejection>, dry_run: bool) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be valid JSON with the expected content type",
        )
        .dry_run(dry_run)
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

fn require_idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let value = headers.get(IDEMPOTENCY_KEY).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_idempotency_key",
            "Idempotency-Key header is required",
        )
    })?;
    let value = value.to_str().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key must contain visible UTF-8 text",
        )
    })?;
    if value.trim().is_empty() || value.len() > MAX_IDEMPOTENCY_KEY_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key must contain 1 to 128 bytes",
        ));
    }
    Ok(value.to_owned())
}

fn parse_dry_run_header(headers: &HeaderMap) -> Result<bool, ApiError> {
    let Some(value) = headers.get(DRY_RUN_HEADER) else {
        return Ok(false);
    };
    match value.to_str() {
        Ok("1") => Ok(true),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_dry_run_header",
            "X-Spurfire-Dry-Run must be exactly 1 when present",
        )),
    }
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

fn fingerprint<T: Serialize>(value: &T) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec(value).map_err(|_| internal_error(false))
}

fn new_lobby_id() -> LobbyId {
    LobbyId::parse(&Uuid::new_v4().to_string()).expect("uuid crate generated UUIDv4")
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

fn refresh_idle_expiry(stored: &mut StoredLobby, now: UnixMillis) {
    stored.lobby.ttl.idle_expires_at = now
        .saturating_add(stored.idle_ttl_ms)
        .min(stored.lobby.ttl.absolute_expires_at);
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
        if stored.lobby.state != LobbyState::InMatch {
            stored.lobby.authority = None;
        }
        return Ok(None);
    }
    let candidates = authority_candidates(stored);
    let election =
        elect_authority(&candidates, now).map_err(|error| authority_error(&error, dry_run))?;
    apply_election(stored, &election, now, dry_run)?;
    Ok(Some(election))
}

fn apply_election(
    stored: &mut StoredLobby,
    election: &AuthorityElection,
    now: UnixMillis,
    dry_run: bool,
) -> Result<(), ApiError> {
    let score_milli = election
        .eligible
        .iter()
        .find(|score| score.player_id == election.winner_player_id)
        .map_or(0, |score| score.score_milli);
    stored.lobby.authority = Some(AuthoritySummary {
        candidate_player_id: election.winner_player_id,
        formula_version: election.formula_version.clone(),
        score_milli,
    });
    if stored.lobby.state == LobbyState::Forming && all_measurements_fresh(stored, now) {
        transition(&mut stored.lobby, LobbyState::Ready, dry_run)?;
    }
    Ok(())
}

fn authority_error(error: &AuthorityElectionError, dry_run: bool) -> ApiError {
    let message = match error {
        AuthorityElectionError::NotEnoughCandidates => {
            "authority election requires at least two roster members"
        }
        AuthorityElectionError::DuplicatePlayer { .. } => {
            "authority input contains a duplicate player"
        }
        AuthorityElectionError::NoFreshCompleteCandidates => {
            "no fresh complete measurement rows are available"
        }
    };
    ApiError::new(StatusCode::CONFLICT, "authority_unavailable", message).dry_run(dry_run)
}

fn cleanup_request(stored: &StoredLobby, dry_run: bool) -> CleanupLobbyRequest {
    CleanupLobbyRequest {
        lobby_id: stored.lobby.lobby_id,
        tailnet: stored.tailnet.clone(),
        tag: stored.tag.clone(),
        credential_count: stored.credentials.len(),
        dry_run,
    }
}

fn revoke_stored_credentials(stored: &mut StoredLobby) {
    for credential in stored.credentials.values_mut() {
        credential.revoked = true;
    }
}

fn apply_cleanup_result(stored: &mut StoredLobby, result: Result<CleanupOutcome, ProviderError>) {
    stored.cleanup_pending = result
        .as_ref()
        .map_or(true, |outcome| outcome.cleanup_pending);
    for player in &mut stored.lobby.roster {
        player.cleanup_pending = stored.cleanup_pending;
        if stored.cleanup_pending {
            player.join_state = PlayerJoinState::CleanupPending;
        }
    }
}

fn provider_api_error(error: &ProviderError, dry_run: bool) -> ApiError {
    let (status, code, message) = match error {
        ProviderError::ModeUnavailable => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "mode_unavailable",
            "requested network provisioning mode is unavailable",
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
