use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use spurfire_protocol::{
    LobbyId, ProvisioningMode, ResponseMetadata, UnixMillis, DRY_RUN_AUTH_KEY,
};
use spurfire_server::{
    build_router, AppState, CleanupLobbyRequest, CleanupOutcome, Config, DryRunProvider,
    InMemoryStore, ManualClock, MintCredentialRequest, MintedCredential, NetworkProvider,
    PrepareLobbyRequest, PreparedNetwork, ProviderError, SecretString,
};
use tower::ServiceExt;

const PLAYER_1: &str = "00000000-0000-4000-8000-000000000001";
const PLAYER_2: &str = "00000000-0000-4000-8000-000000000002";

#[derive(Default)]
struct CountingProvider {
    mints: AtomicU64,
    fail_mint_scopes: bool,
}

impl CountingProvider {
    fn successful() -> Self {
        Self::default()
    }

    fn blocked() -> Self {
        Self {
            mints: AtomicU64::new(0),
            fail_mint_scopes: true,
        }
    }

    fn mint_count(&self) -> u64 {
        self.mints.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl NetworkProvider for CountingProvider {
    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        if request.mode == ProvisioningMode::TailnetPerLobby {
            return Err(ProviderError::ModeUnavailable);
        }
        Ok(PreparedNetwork {
            tailnet: "-".to_owned(),
            dry_run: request.dry_run,
            metadata: ResponseMetadata {
                dry_run: request.dry_run,
                planned_actions: Vec::new(),
            },
        })
    }

    async fn mint_credential(
        &self,
        request: MintCredentialRequest,
    ) -> Result<MintedCredential, ProviderError> {
        self.mints.fetch_add(1, Ordering::SeqCst);
        if self.fail_mint_scopes {
            return Err(ProviderError::InsufficientScopes {
                operation: "auth_keys",
            });
        }
        Ok(MintedCredential {
            credential_id: format!("credential-{}", request.player_id),
            auth_key: SecretString::new("tskey-auth-router-canary-secret"),
            tailnet: request.tailnet,
            metadata: ResponseMetadata::default(),
        })
    }

    async fn cleanup_lobby(
        &self,
        _request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        Ok(CleanupOutcome::default())
    }
}

fn dry_app(
    clock: Arc<ManualClock>,
    provider: Arc<DryRunProvider>,
) -> (Router, AppState, Arc<InMemoryStore>) {
    let config = Config {
        force_dry_run: true,
        provisioning_mode: ProvisioningMode::DryRun,
        ..Config::default()
    };
    let store = Arc::new(InMemoryStore::new());
    let state = AppState::new(config, store.clone(), provider).with_clock(clock);
    (build_router(state.clone()), state, store)
}

fn live_app(
    clock: Arc<ManualClock>,
    provider: Arc<CountingProvider>,
) -> (Router, AppState, Arc<InMemoryStore>) {
    let store = Arc::new(InMemoryStore::new());
    let state = AppState::new(Config::default(), store.clone(), provider).with_clock(clock);
    (build_router(state.clone()), state, store)
}

async fn json_request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap();
    (status, value)
}

async fn create(app: &Router, key: &str, mode: &str, max_players: u8) -> (StatusCode, Value) {
    json_request(
        app,
        Method::POST,
        "/v1/lobbies",
        Some(json!({
            "display_name": "High Noon",
            "max_players": max_players,
            "provisioning_mode": mode
        })),
        &[("idempotency-key", key)],
    )
    .await
}

async fn join(app: &Router, lobby_id: &str, player_id: &str, key: &str) -> (StatusCode, Value) {
    json_request(
        app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/join"),
        Some(json!({
            "player_id": player_id,
            "display_name": format!("Rider {player_id}"),
            "client_wire_version": "1.0"
        })),
        &[("idempotency-key", key)],
    )
    .await
}

async fn measurement(
    app: &Router,
    lobby_id: &str,
    player_id: &str,
    median: u32,
) -> (StatusCode, Value) {
    json_request(
        app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/measurements"),
        Some(json!({
            "player_id": player_id,
            "route_summary": {
                "direct_count": 1,
                "peer_relay_count": 0,
                "derp_count": 0
            },
            "rtt_ms_median": median,
            "rtt_ms_worst": median + 10,
            "jitter_ms": 2,
            "loss_pct_milli": 0,
            "upload_mbps_sustained": 20,
            "device_perf_score": 900,
            "observed_peer_count": 1,
            "future_additive_field": true
        })),
        &[],
    )
    .await
}

#[tokio::test]
async fn success_path_reaches_ready_elects_and_destroys() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(1_000_000)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, _, _) = dry_app(clock, provider.clone());

    let (status, health) = json_request(&app, Method::GET, "/healthz", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health, json!({"status":"ok"}));

    let (status, created) = create(&app, "create-success", "shared_tailnet", 2).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["state"], "PROVISIONING");
    assert_eq!(created["dry_run"], true);
    let lobby_id = created["lobby_id"].as_str().unwrap();

    let (status, lobby) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lobby["state"], "FORMING");
    assert!(lobby.get("auth_key").is_none());

    assert_eq!(
        join(&app, lobby_id, PLAYER_1, "join-1").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        join(&app, lobby_id, PLAYER_2, "join-2").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        measurement(&app, lobby_id, PLAYER_1, 20).await.0,
        StatusCode::OK
    );
    let (status, ready) = measurement(&app, lobby_id, PLAYER_2, 40).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["state"], "READY");

    let (status, election) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/elect-authority"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(election["formula_version"], "election_v1");
    assert_eq!(election["winner_player_id"], PLAYER_1);
    assert_eq!(election["input_hash"].as_str().unwrap().len(), 64);

    let (status, destroyed) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(destroyed["state"], "DESTROYED");
    assert_eq!(destroyed["dry_run"], true);
    assert_eq!(provider.mutating_call_count(), 0);
}

#[tokio::test]
async fn create_idempotency_replays_and_conflicts() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(10_000)));
    let (app, _, store) = dry_app(clock, Arc::new(DryRunProvider::new()));

    let (first_status, first) = create(&app, "same-key", "dry_run", 8).await;
    let (second_status, second) = create(&app, "same-key", "dry_run", 8).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(first["lobby_id"], second["lobby_id"]);
    assert_eq!(store.len().await, 1);

    let (conflict_status, conflict) = create(&app, "same-key", "dry_run", 7).await;
    assert_eq!(conflict_status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");
    assert_eq!(store.len().await, 1);
}

#[tokio::test]
async fn duplicate_join_returns_receipt_and_never_reemits_key() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(20_000)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, _, _) = dry_app(clock, provider.clone());
    let (_, created) = create(&app, "create-duplicate", "dry_run", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();

    let (first_status, first) = join(&app, lobby_id, PLAYER_1, "join-original").await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(first["join_credential"]["auth_key"], DRY_RUN_AUTH_KEY);
    let credential_id = first["join_credential"]["credential_id"].clone();

    let (idem_status, idem) = join(&app, lobby_id, PLAYER_1, "join-original").await;
    assert_eq!(idem_status, StatusCode::OK);
    assert_eq!(idem["join_credential"]["credential_id"], credential_id);
    assert!(idem["join_credential"].get("auth_key").is_none());

    let (duplicate_status, duplicate) = join(&app, lobby_id, PLAYER_1, "join-new-key").await;
    assert_eq!(duplicate_status, StatusCode::OK);
    assert_eq!(duplicate["join_credential"]["credential_id"], credential_id);
    assert!(duplicate["join_credential"].get("auth_key").is_none());
    assert_eq!(provider.mint_count(), 1);
    assert_eq!(provider.mutating_call_count(), 0);
}

#[tokio::test]
async fn destroyed_lobby_rejects_mutations_with_typed_error() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(30_000)));
    let (app, _, _) = dry_app(clock, Arc::new(DryRunProvider::new()));
    let (_, created) = create(&app, "create-transition", "dry_run", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let _ = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[],
    )
    .await;

    let (status, error) = join(&app, lobby_id, PLAYER_1, "join-after-delete").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "lobby_closed");
    assert_eq!(error["dry_run"], true);
    assert!(error["message"].is_string());
}

#[tokio::test]
async fn deterministic_idle_expiry_cleanup_marks_lobby_expired() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(100_000)));
    let provider = Arc::new(CountingProvider::successful());
    let (app, state, _) = live_app(clock.clone(), provider);
    let (_, created) = create(&app, "create-expiry", "shared_tailnet", 2).await;
    let lobby_id_text = created["lobby_id"].as_str().unwrap();
    let lobby_id = LobbyId::parse(lobby_id_text).unwrap();

    clock.advance(10 * 60 * 1_000 - 1);
    assert!(state.cleanup_expired_now().await.is_empty());
    clock.advance(1);
    assert_eq!(state.cleanup_expired_now().await, vec![lobby_id]);

    let (_, lobby) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id_text}"),
        None,
        &[],
    )
    .await;
    assert_eq!(lobby["state"], "EXPIRED");
}

#[tokio::test]
async fn insufficient_live_scopes_fail_closed_and_persist_reason() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(40_000)));
    let provider = Arc::new(CountingProvider::blocked());
    let (app, _, _) = live_app(clock, provider.clone());
    let (_, created) = create(&app, "create-blocked", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();

    let (status, error) = join(&app, lobby_id, PLAYER_1, "blocked-join").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "provider_scopes_insufficient");
    assert_eq!(error["state_reason"], "provisioning_blocked_auth_keys_403");
    let encoded = error.to_string();
    assert!(!encoded.contains("tskey-"));
    assert_eq!(provider.mint_count(), 1);

    let (_, lobby) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(lobby["state"], "FAILED");
    assert_eq!(lobby["state_reason"], "provisioning_blocked_auth_keys_403");
}

#[tokio::test]
async fn malformed_requests_and_unknown_routes_are_json_errors() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(50_000)));
    let (app, _, _) = dry_app(clock, Arc::new(DryRunProvider::new()));

    let (missing_status, missing) = json_request(
        &app,
        Method::POST,
        "/v1/lobbies",
        Some(json!({
            "display_name": "No Key",
            "provisioning_mode": "dry_run"
        })),
        &[],
    )
    .await;
    assert_eq!(missing_status, StatusCode::BAD_REQUEST);
    assert_eq!(missing["code"], "missing_idempotency_key");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/lobbies")
        .header("content-type", "application/json")
        .header("idempotency-key", "bad-json")
        .body(Body::from("{"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], "invalid_json");

    let (mode_status, mode) = create(&app, "bad-mode", "tailnet_per_lobby", 8).await;
    assert_eq!(mode_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(mode["code"], "mode_unavailable");

    let (not_found_status, not_found) =
        json_request(&app, Method::GET, "/not-a-route", None, &[]).await;
    assert_eq!(not_found_status, StatusCode::NOT_FOUND);
    assert_eq!(not_found["code"], "route_not_found");
}

#[tokio::test]
async fn config_cap_is_enforced_before_provider_mutation() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(60_000)));
    let provider = Arc::new(CountingProvider::successful());
    let store = Arc::new(InMemoryStore::new());
    let config = Config {
        max_players: 4,
        default_ttl: Duration::from_secs(3_600),
        ..Config::default()
    };
    let state = AppState::new(config, store, provider.clone()).with_clock(clock);
    let app = build_router(state);

    let (status, error) = create(&app, "too-large", "shared_tailnet", 5).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "max_players_exceeds_service_limit");
    assert_eq!(provider.mint_count(), 0);
}
