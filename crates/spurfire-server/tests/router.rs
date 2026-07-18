use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use spurfire_protocol::{
    NetworkLifecycle, ProvisioningMode, ResponseMetadata, TailnetDnsName, UnixMillis,
    DRY_RUN_AUTH_KEY,
};
use spurfire_server::{
    build_router, AppState, CleanupLobbyRequest, CleanupOutcome, Config, DryRunProvider,
    InMemoryStore, JsonFileStore, ManualClock, MintCredentialRequest, MintedCredential,
    NetworkProvider, ObserveNetworkRequest, PrepareLobbyRequest, PreparedNetwork,
    ProviderCapabilities, ProviderDeviceObservation, ProviderError, ProviderNetworkIdentity,
    SecretString, TailnetPresenceRequest,
};
use tower::ServiceExt;

const PLAYER_1: &str = "00000000-0000-4000-8000-000000000001";
const PLAYER_2: &str = "00000000-0000-4000-8000-000000000002";
const PLAYER_3: &str = "00000000-0000-4000-8000-000000000003";
const PLAYER_4: &str = "00000000-0000-4000-8000-000000000004";

struct RecordingProvider {
    capabilities: ProviderCapabilities,
    mints: AtomicU64,
    cleanups: AtomicU64,
    mutations: AtomicU64,
    fail_mint_scopes: AtomicBool,
    missing_child_secret: AtomicBool,
    pending_cleanup_calls: AtomicU64,
    observations: AtomicU64,
    observed_device_count: AtomicU64,
    fail_observations: AtomicBool,
    presence_polls: AtomicU64,
    tailnet_present: AtomicBool,
    fail_identity_cleanup: AtomicBool,
    fail_secret_erasure: AtomicBool,
    cleanup_requests: Mutex<Vec<CleanupLobbyRequest>>,
}

impl RecordingProvider {
    fn available() -> Self {
        Self {
            capabilities: ProviderCapabilities::available(),
            mints: AtomicU64::new(0),
            cleanups: AtomicU64::new(0),
            mutations: AtomicU64::new(0),
            fail_mint_scopes: AtomicBool::new(false),
            missing_child_secret: AtomicBool::new(false),
            pending_cleanup_calls: AtomicU64::new(0),
            observations: AtomicU64::new(0),
            observed_device_count: AtomicU64::new(2),
            fail_observations: AtomicBool::new(false),
            presence_polls: AtomicU64::new(0),
            tailnet_present: AtomicBool::new(false),
            fail_identity_cleanup: AtomicBool::new(false),
            fail_secret_erasure: AtomicBool::new(false),
            cleanup_requests: Mutex::new(Vec::new()),
        }
    }

    fn blocked_keys() -> Self {
        Self {
            capabilities: ProviderCapabilities {
                oauth_token_ok: true,
                can_manage_organization_tailnets: true,
                can_mint_auth_keys: false,
                can_list_devices: false,
                can_manage_acl: false,
            },
            ..Self::available()
        }
    }

    fn fail_mint() -> Self {
        let provider = Self::available();
        provider.fail_mint_scopes.store(true, Ordering::SeqCst);
        provider
    }

    fn with_pending_cleanups(count: u64) -> Self {
        let provider = Self::available();
        provider
            .pending_cleanup_calls
            .store(count, Ordering::SeqCst);
        provider
    }

    fn mint_count(&self) -> u64 {
        self.mints.load(Ordering::SeqCst)
    }

    fn cleanup_count(&self) -> u64 {
        self.cleanups.load(Ordering::SeqCst)
    }

    fn mutation_count(&self) -> u64 {
        self.mutations.load(Ordering::SeqCst)
    }

    fn lose_child_secrets(&self) {
        self.missing_child_secret.store(true, Ordering::SeqCst);
    }

    fn cleanup_requests(&self) -> Vec<CleanupLobbyRequest> {
        self.cleanup_requests.lock().unwrap().clone()
    }

    fn observation_count(&self) -> u64 {
        self.observations.load(Ordering::SeqCst)
    }

    fn fail_observations(&self) {
        self.fail_observations.store(true, Ordering::SeqCst);
    }

    fn fail_identity_cleanup(&self) {
        self.fail_identity_cleanup.store(true, Ordering::SeqCst);
    }

    fn fail_secret_erasure(&self) {
        self.fail_secret_erasure.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl NetworkProvider for RecordingProvider {
    fn cached_capabilities(&self) -> ProviderCapabilities {
        self.capabilities
    }

    fn lobby_access_error(
        &self,
        _lobby_id: spurfire_protocol::LobbyId,
        mode: ProvisioningMode,
        _network_lifecycle: NetworkLifecycle,
        dry_run: bool,
    ) -> Option<ProviderError> {
        (!dry_run
            && mode == ProvisioningMode::TailnetPerLobby
            && self.missing_child_secret.load(Ordering::SeqCst))
        .then_some(ProviderError::ChildSecretUnavailable)
    }

    async fn prepare_lobby(
        &self,
        request: PrepareLobbyRequest,
    ) -> Result<PreparedNetwork, ProviderError> {
        if request.mode == ProvisioningMode::TailnetPerLobby && !request.dry_run {
            self.mutations.fetch_add(1, Ordering::SeqCst);
        }
        let tailnet = if request.dry_run {
            "dry-run.invalid"
        } else if request.mode == ProvisioningMode::TailnetPerLobby {
            "child-test.ts.net"
        } else {
            "shared-test.ts.net"
        };
        Ok(PreparedNetwork {
            tailnet: tailnet.to_owned(),
            identity: (!request.dry_run).then(|| ProviderNetworkIdentity {
                provider_tailnet_id: (request.mode == ProvisioningMode::TailnetPerLobby)
                    .then(|| "TtRouterCNTRL".to_owned()),
                tailnet_dns_name: TailnetDnsName::parse(tailnet).unwrap(),
            }),
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
        if !request.dry_run {
            self.mutations.fetch_add(1, Ordering::SeqCst);
        }
        if self.fail_mint_scopes.load(Ordering::SeqCst) {
            return Err(ProviderError::InsufficientScopes {
                operation: "auth_keys",
            });
        }
        Ok(MintedCredential {
            credential_id: format!("credential-{}-{}", request.player_id, self.mint_count()),
            auth_key: SecretString::new(if request.dry_run {
                DRY_RUN_AUTH_KEY
            } else {
                "synthetic-auth-key-router-canary-secret"
            }),
            tailnet: request.tailnet,
            metadata: ResponseMetadata {
                dry_run: request.dry_run,
                planned_actions: Vec::new(),
            },
        })
    }

    async fn cleanup_lobby(
        &self,
        request: CleanupLobbyRequest,
    ) -> Result<CleanupOutcome, ProviderError> {
        self.cleanups.fetch_add(1, Ordering::SeqCst);
        if self.fail_identity_cleanup.load(Ordering::SeqCst) {
            return Err(ProviderError::IdentityMismatch);
        }
        if request.mode == ProvisioningMode::TailnetPerLobby
            && self.missing_child_secret.load(Ordering::SeqCst)
        {
            return Err(ProviderError::ChildSecretUnavailable);
        }
        if !request.dry_run {
            self.mutations.fetch_add(1, Ordering::SeqCst);
        }
        self.cleanup_requests.lock().unwrap().push(request.clone());
        if self
            .pending_cleanup_calls
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Ok(CleanupOutcome {
                cleanup_pending: true,
                ..CleanupOutcome::default()
            });
        }
        Ok(CleanupOutcome {
            delete_acknowledged: request.mode == ProvisioningMode::TailnetPerLobby
                && request.include_devices,
            child_secret_erased: request.mode == ProvisioningMode::TailnetPerLobby
                && request.include_devices
                && !self.fail_secret_erasure.load(Ordering::SeqCst),
            revoked_credential_ids: request
                .credentials
                .iter()
                .map(|credential| credential.credential_id.clone())
                .collect(),
            attempted_device_deletes: usize::from(request.include_devices),
            metadata: ResponseMetadata {
                dry_run: request.dry_run,
                planned_actions: Vec::new(),
            },
            ..CleanupOutcome::default()
        })
    }

    async fn observe_network(
        &self,
        _request: ObserveNetworkRequest,
    ) -> Result<ProviderDeviceObservation, ProviderError> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        if self.fail_observations.load(Ordering::SeqCst) {
            return Err(ProviderError::InsufficientScopes {
                operation: "device_inventory",
            });
        }
        Ok(ProviderDeviceObservation {
            enrolled_device_count: u32::try_from(self.observed_device_count.load(Ordering::SeqCst))
                .unwrap(),
        })
    }

    async fn tailnet_present(
        &self,
        _request: TailnetPresenceRequest,
    ) -> Result<bool, ProviderError> {
        self.presence_polls.fetch_add(1, Ordering::SeqCst);
        Ok(self.tailnet_present.load(Ordering::SeqCst))
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
    provider: Arc<RecordingProvider>,
) -> (Router, AppState, Arc<InMemoryStore>) {
    let store = Arc::new(InMemoryStore::new());
    let config = Config {
        real_mutations_enabled: true,
        shared_tailnet: "shared-test.ts.net".to_owned(),
        ..Config::default()
    };
    let state = AppState::new(config, store.clone(), provider).with_clock(clock);
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
        &[("idempotency-key", key), ("x-spurfire-player-id", PLAYER_1)],
    )
    .await
}

async fn join(app: &Router, lobby_id: &str, player_id: &str, key: &str) -> (StatusCode, Value) {
    join_with(app, lobby_id, player_id, key, "1.0", "election_v1").await
}

async fn join_with(
    app: &Router,
    lobby_id: &str,
    player_id: &str,
    key: &str,
    wire: &str,
    formula: &str,
) -> (StatusCode, Value) {
    json_request(
        app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/join"),
        Some(json!({
            "player_id": player_id,
            "display_name": format!("Rider {player_id}"),
            "client_wire_version": wire,
            "authority_formula_version": formula
        })),
        &[
            ("idempotency-key", key),
            ("x-spurfire-player-id", player_id),
        ],
    )
    .await
}

async fn measurement(
    app: &Router,
    lobby_id: &str,
    player_id: &str,
    median: u32,
    peer_count: u32,
) -> (StatusCode, Value) {
    json_request(
        app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/measurements"),
        Some(json!({
            "player_id": player_id,
            "route_summary": {
                "direct_count": peer_count,
                "peer_relay_count": 0,
                "derp_count": 0
            },
            "rtt_ms_median": median,
            "rtt_ms_worst": median + 10,
            "jitter_ms": 2,
            "loss_pct_milli": 0,
            "upload_mbps_sustained": 20,
            "device_perf_score": 900,
            "observed_peer_count": peer_count,
            "future_additive_field": true
        })),
        &[("x-spurfire-player-id", player_id)],
    )
    .await
}

async fn network(app: &Router, lobby_id: &str, capability: &str) -> (StatusCode, Value) {
    let authorization = format!("Spurfire-Capability {capability}");
    json_request(
        app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}/network"),
        None,
        &[("authorization", authorization.as_str())],
    )
    .await
}

async fn get(app: &Router, lobby_id: &str) -> (StatusCode, Value) {
    json_request(
        app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[],
    )
    .await
}

async fn get_authorized(app: &Router, lobby_id: &str, capability: &str) -> (StatusCode, Value) {
    let authorization = format!("Spurfire-Capability {capability}");
    json_request(
        app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("authorization", authorization.as_str())],
    )
    .await
}

async fn make_ready(app: &Router, lobby_id: &str, players: &[(&str, u32)]) {
    let peer_count = u32::try_from(players.len() - 1).unwrap();
    for (player, _) in players {
        assert_eq!(
            join(app, lobby_id, player, &format!("join-{player}"))
                .await
                .0,
            StatusCode::CREATED
        );
    }
    for (index, (player, median)) in players.iter().enumerate() {
        let (status, _) = measurement(app, lobby_id, player, *median, peer_count).await;
        assert_eq!(status, StatusCode::OK, "measurement {index}");
    }
}

#[tokio::test]
async fn landing_page_links_public_downloads_and_sets_browser_headers() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(0)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, _, _) = dry_app(clock, provider);
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    assert!(response.headers().contains_key("content-security-policy"));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();
    assert!(html.contains("High noon. Low ping."));
    assert!(html.contains("A temporary posse, not a game server."));
    assert!(html.contains("What the lobby knows—and what it does not."));
    assert!(html.contains("Public preview: zero Tailscale mutations"));
    assert!(html.contains("brew install --cask rajsinghtech/tap/spurfire"));
    assert!(html.contains("github.com/rajsinghtech/spurfire/releases/latest"));
    assert!(html.contains("fetch('/v1/capabilities'"));
}

#[tokio::test]
async fn inspector_shell_is_no_store_exact_selection_and_has_no_embedded_secret() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(0)));
    let (app, _, _) = dry_app(clock, Arc::new(DryRunProvider::new()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/inspect")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'none'"));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();
    assert!(html.contains("Selected lobby network"));
    assert!(html.contains("Spurfire-Capability"));
    assert!(!html.contains("localStorage"));
    assert!(!html.contains("sessionStorage"));
    assert!(!html.contains("synthetic-auth-key"));
    assert!(!html.contains("oauth"));
    assert!(!html.contains("packet capture"));
}

#[tokio::test]
async fn full_dry_run_lifecycle_reaches_destroyed_without_mutation() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(1_000_000)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, _, _) = dry_app(clock, provider.clone());

    let (status, health) = json_request(&app, Method::GET, "/healthz", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["provisioning_ready"], true);

    let (status, created) = create(&app, "create-lifecycle", "shared_tailnet", 3).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["state"], "PROVISIONING");
    assert_eq!(created["dry_run"], true);
    assert_eq!(created["planned_actions"], json!([]));
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let network_capability = created["creator_capability"].as_str().unwrap();
    assert_eq!(get(&app, lobby_id).await.1["state"], "FORMING");

    make_ready(
        &app,
        lobby_id,
        &[(PLAYER_1, 10), (PLAYER_2, 30), (PLAYER_3, 50)],
    )
    .await;
    let (_, lobby) = get(&app, lobby_id).await;
    assert_eq!(lobby["state"], "READY");

    let (status, authority) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}/authority"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(authority["winner_player_id"], PLAYER_1);
    assert_eq!(
        authority["input"]["candidates"].as_array().unwrap().len(),
        3
    );
    let input_hash = authority["input_hash"].clone();

    let (status, started) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/start"),
        Some(json!({"creator_player_id": PLAYER_1, "map_seed": 42})),
        &[
            ("idempotency-key", "start-lifecycle"),
            ("x-spurfire-player-id", PLAYER_1),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(started["state"], "STARTING");
    assert_eq!(started["map_seed"], 42);

    let (status, heartbeat) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/heartbeat"),
        Some(json!({"player_id": PLAYER_1, "input_hash": input_hash})),
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat["state"], "IN_MATCH");
    let (_, network_view) = network(&app, lobby_id, network_capability).await;
    assert_eq!(
        network_view["authority"]["last_accepted_heartbeat"]["value"]["player_id"],
        PLAYER_1
    );
    assert_eq!(
        network_view["authority"]["last_accepted_heartbeat"]["assurance"],
        "authoritative"
    );
    assert!(
        network_view["authority"]["last_accepted_heartbeat"]["value"]["epoch"]
            .as_u64()
            .unwrap()
            >= 1
    );

    let (status, results) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/results"),
        Some(json!({
            "submitted_by": PLAYER_1,
            "co_signers": [PLAYER_2],
            "final_scores": [
                {"player_id": PLAYER_1, "score": 10},
                {"player_id": PLAYER_2, "score": 5},
                {"player_id": PLAYER_3, "score": 1}
            ],
            "match_duration_s": 60,
            "input_hash": authority["input_hash"]
        })),
        &[
            ("idempotency-key", "results-lifecycle"),
            ("x-spurfire-player-id", PLAYER_1),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(results["state"], "CLOSING");
    assert_eq!(get(&app, lobby_id).await.1["state"], "DESTROYED");
    assert_eq!(provider.mutating_call_count(), 0);
}

#[tokio::test]
async fn selected_dry_run_network_view_is_capability_protected_and_never_fake_real() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_000_000)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, _, _) = dry_app(clock, provider.clone());
    let (status, created) = create(&app, "create-network-dry", "tailnet_per_lobby", 2).await;
    assert_eq!(status, StatusCode::CREATED);
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(capability).unwrap().len(), 32);
    let (_, anonymous_lobby) = get(&app, lobby_id).await;
    assert_eq!(anonymous_lobby["provisioning_mode"], "dry_run");
    assert_eq!(anonymous_lobby["roster_count"], 0);
    for omitted in [
        "display_name",
        "state_reason",
        "roster",
        "map_seed",
        "authority",
    ] {
        assert!(anonymous_lobby.get(omitted).is_none(), "exposed {omitted}");
    }

    let unauthorized_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/lobbies/{lobby_id}/network"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        unauthorized_response.headers()["cache-control"],
        "private, no-store"
    );
    assert_eq!(unauthorized_response.headers()["vary"], "Authorization");
    assert_eq!(
        unauthorized_response.headers()["referrer-policy"],
        "no-referrer"
    );
    let missing_status = unauthorized_response.status();
    let missing: Value = serde_json::from_slice(
        &unauthorized_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    let (wrong_status, wrong) = network(
        &app,
        lobby_id,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert_eq!(wrong_status, StatusCode::NOT_FOUND);
    assert_eq!(missing, wrong);
    assert_eq!(missing["code"], "lobby_not_found");

    let (status, view) = network(&app, lobby_id, capability).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["truth_label"], "SIMULATED — NO TAILNET EXISTS");
    assert_eq!(view["backing"]["backing_mode"], "dry_run");
    assert_eq!(view["backing"]["simulates_mode"], "tailnet_per_lobby");
    assert_eq!(view["backing"]["network_lifecycle"], "SIMULATED");
    assert!(view["backing"]["tailnet_dns_name"]["value"].is_null());
    assert_eq!(view["backing"]["control_service_member"]["value"], false);
    assert_eq!(provider.mutating_call_count(), 0);
    let wire = view.to_string();
    assert!(!wire.contains(capability));
    for forbidden in [
        "dry-run.invalid",
        "provisioning.invalid",
        "provider_tailnet_id",
        "auth_key",
        "oauth",
        "device_id",
    ] {
        assert!(!wire.contains(forbidden), "leaked {forbidden}");
    }

    let (replay_status, replay) = create(&app, "create-network-dry", "tailnet_per_lobby", 2).await;
    assert_eq!(replay_status, StatusCode::OK);
    assert!(replay.get("creator_capability").is_none());
}

#[tokio::test]
async fn dedicated_and_shared_views_use_precise_truth_labels_and_fqdn() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_100_000)));
    let dedicated_provider = Arc::new(RecordingProvider::available());
    let (dedicated_app, _, _) = live_app(clock.clone(), dedicated_provider);
    let (_, dedicated) = create(
        &dedicated_app,
        "create-network-dedicated",
        "tailnet_per_lobby",
        2,
    )
    .await;
    let dedicated_id = dedicated["lobby_id"].as_str().unwrap();
    let dedicated_capability = dedicated["creator_capability"].as_str().unwrap();
    let (_, view) = network(&dedicated_app, dedicated_id, dedicated_capability).await;
    assert_eq!(view["truth_label"], "REAL — DEDICATED TAILNET");
    assert_eq!(view["backing"]["isolation"], "dedicated");
    assert_eq!(view["backing"]["network_lifecycle"], "ACTIVE");
    assert_eq!(
        view["backing"]["tailnet_dns_name"]["value"],
        "child-test.ts.net"
    );
    assert!(view.get("provider_tailnet_id").is_none());
    assert!(view.to_string().find("TtRouterCNTRL").is_none());

    let shared_provider = Arc::new(RecordingProvider::available());
    let (shared_app, _, _) = live_app(clock, shared_provider);
    let (_, shared) = create(&shared_app, "create-network-shared", "shared_tailnet", 2).await;
    let shared_id = shared["lobby_id"].as_str().unwrap();
    let shared_capability = shared["creator_capability"].as_str().unwrap();
    let (_, view) = network(&shared_app, shared_id, shared_capability).await;
    assert_eq!(view["truth_label"], "REAL — SHARED COMPATIBILITY");
    assert_eq!(view["backing"]["isolation"], "shared");
    assert_eq!(
        view["backing"]["tailnet_dns_name"]["value"],
        "shared-test.ts.net"
    );
}

#[tokio::test]
async fn legacy_measurements_supply_fresh_routes_but_never_application_rtt() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_200_000)));
    let (app, _, _) = dry_app(clock.clone(), Arc::new(DryRunProvider::new()));
    let (_, created) = create(&app, "create-network-routes", "dry_run", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    make_ready(&app, lobby_id, &[(PLAYER_1, 11), (PLAYER_2, 31)]).await;

    let (_, fresh) = network(&app, lobby_id, capability).await;
    assert_eq!(fresh["counts"]["fresh_reporter_count"]["value"], 2);
    assert_eq!(
        fresh["counts"]["fresh_directional_observation_count"]["value"],
        2
    );
    assert_eq!(fresh["routes"]["expected_direction_count"]["value"], 2);
    assert_eq!(fresh["routes"]["direct_count"]["value"], 2);
    assert_eq!(fresh["routes"]["unknown_count"]["value"], 0);
    assert_eq!(fresh["routes"]["direct_ratio_milli"]["value"], 1000);
    assert!(fresh["application_quality"]["application_rtt_ms_median"]["value"].is_null());
    assert_eq!(
        fresh["application_quality"]["application_rtt_ms_median"]["unknown_reason"],
        "unsupported"
    );
    assert_eq!(
        fresh["authority"]["control_election"]["value"]["winner_player_id"],
        PLAYER_1
    );
    assert!(fresh["authority"]["last_accepted_heartbeat"]["value"].is_null());

    clock.advance(15_000);
    let (_, stale) = network(&app, lobby_id, capability).await;
    assert_eq!(stale["counts"]["fresh_reporter_count"]["value"], 0);
    assert_eq!(stale["routes"]["direct_count"]["value"], 0);
    assert_eq!(stale["routes"]["unknown_count"]["value"], 2);
    assert_eq!(stale["authority"]["control_election"]["freshness"], "fresh");
}

#[tokio::test]
async fn provider_observations_are_cached_bounded_and_fail_stale_not_zero() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_300_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, state, _) = live_app(clock.clone(), provider.clone());
    let (_, created) = create(&app, "create-network-observation", "tailnet_per_lobby", 3).await;
    let lobby_id_text = created["lobby_id"].as_str().unwrap();
    let lobby_id = spurfire_protocol::LobbyId::parse(lobby_id_text).unwrap();
    let capability = created["creator_capability"].as_str().unwrap();

    let (_, unknown) = network(&app, lobby_id_text, capability).await;
    assert!(unknown["counts"]["provider_enrolled_device_count"]["value"].is_null());
    assert_eq!(
        unknown["counts"]["provider_enrolled_device_count"]["unknown_reason"],
        "never_observed"
    );
    assert_eq!(provider.observation_count(), 0);

    state.refresh_network_observation(lobby_id).await.unwrap();
    assert_eq!(provider.observation_count(), 1);
    let (_, fresh) = network(&app, lobby_id_text, capability).await;
    assert_eq!(
        fresh["counts"]["provider_enrolled_device_count"]["value"],
        2
    );
    assert_eq!(
        fresh["counts"]["provider_enrolled_device_count"]["freshness"],
        "fresh"
    );
    assert!(fresh["counts"]["provider_online_device_count"]["value"].is_null());
    assert_eq!(
        fresh["counts"]["provider_online_device_count"]["unknown_reason"],
        "unsupported"
    );
    assert_eq!(
        provider.observation_count(),
        1,
        "GET performed provider I/O"
    );

    clock.advance(30_000);
    let (_, stale) = network(&app, lobby_id_text, capability).await;
    assert_eq!(
        stale["counts"]["provider_enrolled_device_count"]["value"],
        2
    );
    assert_eq!(
        stale["counts"]["provider_enrolled_device_count"]["freshness"],
        "stale"
    );

    provider.fail_observations();
    assert!(state.refresh_network_observation(lobby_id).await.is_err());
    let (_, failed) = network(&app, lobby_id_text, capability).await;
    assert_eq!(
        failed["counts"]["provider_enrolled_device_count"]["value"],
        2
    );
    assert_eq!(
        failed["counts"]["provider_enrolled_device_count"]["freshness"],
        "stale"
    );
    assert_eq!(provider.observation_count(), 2);
}

#[tokio::test]
async fn real_mutations_default_off_and_singleton_lease_is_atomic() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_400_000)));
    let disabled_provider = Arc::new(RecordingProvider::available());
    let disabled_store = Arc::new(InMemoryStore::new());
    let disabled_state = AppState::new(
        Config::default(),
        disabled_store.clone(),
        disabled_provider.clone(),
    )
    .with_clock(clock.clone());
    let disabled_app = build_router(disabled_state);
    let (status, error) = create(
        &disabled_app,
        "create-real-disabled",
        "tailnet_per_lobby",
        2,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "real_mutations_disabled");
    assert_eq!(disabled_provider.mutation_count(), 0);
    assert!(disabled_store.is_empty().await);

    let provider = Arc::new(RecordingProvider::available());
    let (app, _, store) = live_app(clock, provider.clone());
    let (first, second) = tokio::join!(
        create(&app, "concurrent-real-a", "tailnet_per_lobby", 2),
        create(&app, "concurrent-real-b", "tailnet_per_lobby", 2)
    );
    let statuses = [first.0, second.0];
    assert!(statuses.contains(&StatusCode::CREATED));
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let conflict = if first.0 == StatusCode::CONFLICT {
        first.1
    } else {
        second.1
    };
    assert_eq!(conflict["code"], "real_lobby_capacity_reached");
    assert_eq!(provider.mutation_count(), 1);
    assert!(store.real_lobby_lease_held().await);
}

#[tokio::test]
async fn dedicated_cleanup_needs_two_exact_absence_polls_before_lease_release() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_500_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, state, store) = live_app(clock.clone(), provider);
    let (_, created) = create(&app, "create-cleanup-proof", "tailnet_per_lobby", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    let (status, deleted) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["state"], "DESTROYED");
    assert_eq!(deleted["cleanup_pending"], true);
    assert!(store.real_lobby_lease_held().await);
    let (_, verifying) = network(&app, lobby_id, capability).await;
    assert_eq!(
        verifying["backing"]["network_lifecycle"],
        "VERIFYING_ABSENCE"
    );
    assert!(verifying["cleanup"]["absence_confirmed_at"]["value"].is_null());

    state.cleanup_expired_now().await;
    assert!(store.real_lobby_lease_held().await);
    clock.advance(4_999);
    state.cleanup_expired_now().await;
    assert!(store.real_lobby_lease_held().await);
    clock.advance(1);
    state.cleanup_expired_now().await;
    assert!(!store.real_lobby_lease_held().await);

    let (_, absent) = network(&app, lobby_id, capability).await;
    assert_eq!(absent["backing"]["network_lifecycle"], "DEDICATED_ABSENT");
    assert!(absent["backing"]["tailnet_dns_name"]["value"].is_null());
    assert!(absent["cleanup"]["absence_confirmed_at"]["value"].is_number());
}

#[tokio::test]
async fn identity_or_vault_cleanup_failure_retains_real_lease() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_600_000)));
    let identity_provider = Arc::new(RecordingProvider::available());
    identity_provider.fail_identity_cleanup();
    let (identity_app, _, identity_store) = live_app(clock.clone(), identity_provider);
    let (_, created) = create(
        &identity_app,
        "create-identity-failure",
        "tailnet_per_lobby",
        2,
    )
    .await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    json_request(
        &identity_app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    let (_, view) = network(&identity_app, lobby_id, capability).await;
    assert_eq!(view["backing"]["network_lifecycle"], "MANUAL_REMEDIATION");
    assert!(identity_store.real_lobby_lease_held().await);

    let vault_provider = Arc::new(RecordingProvider::available());
    vault_provider.fail_secret_erasure();
    let (vault_app, _, vault_store) = live_app(clock, vault_provider);
    let (_, created) = create(&vault_app, "create-vault-failure", "tailnet_per_lobby", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    json_request(
        &vault_app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    let (_, view) = network(&vault_app, lobby_id, capability).await;
    assert_eq!(view["backing"]["network_lifecycle"], "CLEANUP_PENDING");
    assert!(vault_store.real_lobby_lease_held().await);
}

#[tokio::test]
async fn durable_capability_is_hash_only_and_survives_reopen() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(2_700_000)));
    let path = std::env::temp_dir().join(format!(
        "spurfire-network-capability-{}.json",
        std::process::id()
    ));
    let _ = tokio::fs::remove_file(&path).await;
    let store = Arc::new(JsonFileStore::open(&path).await.unwrap());
    let config = Config {
        force_dry_run: true,
        provisioning_mode: ProvisioningMode::DryRun,
        ..Config::default()
    };
    let state = AppState::new(config.clone(), store, Arc::new(DryRunProvider::new()))
        .with_clock(clock.clone());
    let app = build_router(state);
    let (_, created) = create(&app, "durable-capability", "dry_run", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap().to_owned();
    let capability = created["creator_capability"].as_str().unwrap().to_owned();
    let durable = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(!durable.contains(&capability));
    assert!(!durable.contains("creator_capability\":\""));

    let reopened = Arc::new(JsonFileStore::open(&path).await.unwrap());
    let app = build_router(
        AppState::new(config, reopened, Arc::new(DryRunProvider::new())).with_clock(clock),
    );
    assert_eq!(
        network(&app, &lobby_id, &capability).await.0,
        StatusCode::OK
    );
    let _ = tokio::fs::remove_file(&path).await;
}

#[tokio::test]
async fn create_idempotency_is_actor_bound_and_conflicts_on_body_change() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(10_000)));
    let (app, _, store) = dry_app(clock, Arc::new(DryRunProvider::new()));

    let (first_status, first) = create(&app, "same-key", "dry_run", 8).await;
    let (second_status, second) = create(&app, "same-key", "dry_run", 8).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(first["lobby_id"], second["lobby_id"]);
    assert_eq!(store.len().await, 1);

    let (status, conflict) = create(&app, "same-key", "dry_run", 7).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");

    let (status, conflict) = json_request(
        &app,
        Method::POST,
        "/v1/lobbies",
        Some(json!({
            "display_name": "High Noon",
            "max_players": 8,
            "provisioning_mode": "dry_run"
        })),
        &[
            ("idempotency-key", "same-key"),
            ("x-spurfire-player-id", PLAYER_2),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");
}

#[tokio::test]
async fn credential_is_singleton_until_expiry_and_key_is_returned_once() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(20_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, _, _) = live_app(clock.clone(), provider.clone());
    let (_, created) = create(&app, "create-duplicate", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();

    let (first_status, first) = join(&app, lobby_id, PLAYER_1, "join-original").await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(
        first["join_credential"]["auth_key"],
        "synthetic-auth-key-router-canary-secret"
    );
    let credential_id = first["join_credential"]["credential_id"].clone();

    let (status, replay) = join(&app, lobby_id, PLAYER_1, "join-original").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["join_credential"]["credential_id"], credential_id);
    assert!(replay["join_credential"].get("auth_key").is_none());
    let (status, duplicate) = join(&app, lobby_id, PLAYER_1, "join-second-key").await;
    assert_eq!(status, StatusCode::OK);
    assert!(duplicate["join_credential"].get("auth_key").is_none());
    assert_eq!(provider.mint_count(), 1);

    clock.advance(300_001);
    let (status, fresh) = join(&app, lobby_id, PLAYER_1, "join-after-expiry").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(fresh["join_credential"]["credential_id"], credential_id);
    assert_eq!(provider.mint_count(), 2);
}

#[tokio::test]
async fn capabilities_fail_closed_and_mint_403_persists_reason() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(40_000)));
    let blocked = Arc::new(RecordingProvider::blocked_keys());
    let (app, _, _) = live_app(clock.clone(), blocked);
    let (_, created) = create(&app, "create-blocked", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    let (_, capabilities) = json_request(&app, Method::GET, "/v1/capabilities", None, &[]).await;
    assert_eq!(capabilities["modes"]["shared_tailnet"], "blocked_scopes");
    assert_eq!(get(&app, lobby_id).await.0, StatusCode::NOT_FOUND);
    let (_, lobby) = get_authorized(&app, lobby_id, capability).await;
    assert_eq!(lobby["state"], "FAILED");
    assert_eq!(lobby["state_reason"], "provisioning_blocked_auth_keys_403");

    let failing = Arc::new(RecordingProvider::fail_mint());
    let (app, _, _) = live_app(clock, failing.clone());
    let (_, created) = create(&app, "create-mint-fail", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    let (status, error) = join(&app, lobby_id, PLAYER_1, "mint-fail").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["state_reason"], "provisioning_blocked_auth_keys_403");
    assert!(!error.to_string().contains("synthetic-auth-key"));
    assert_eq!(failing.mint_count(), 1);
    assert_eq!(
        get_authorized(&app, lobby_id, capability).await.1["state"],
        "FAILED"
    );
}

#[tokio::test]
async fn tailnet_per_lobby_is_idempotent_and_restart_loss_fails_closed() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(45_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, _, _) = live_app(clock, provider.clone());

    let (status, created) = create(&app, "create-child", "tailnet_per_lobby", 2).await;
    assert_eq!(status, StatusCode::CREATED);
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    let (status, replay) = create(&app, "create-child", "tailnet_per_lobby", 2).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["lobby_id"], created["lobby_id"]);
    assert_eq!(provider.mutation_count(), 1);

    let (_, capabilities) = json_request(&app, Method::GET, "/v1/capabilities", None, &[]).await;
    assert_eq!(capabilities["modes"]["tailnet_per_lobby"], "available");
    assert_eq!(capabilities["can_manage_organization_tailnets"], true);
    assert_eq!(
        get_authorized(&app, lobby_id, capability).await.1["state"],
        "FORMING"
    );

    provider.lose_child_secrets();
    let (_, failed) = get_authorized(&app, lobby_id, capability).await;
    assert_eq!(failed["state"], "FAILED");
    assert_eq!(
        failed["state_reason"],
        "child_secret_unavailable_manual_remediation"
    );
    assert_eq!(failed["cleanup_pending"], true);
    assert!(!failed.to_string().contains("child-oauth"));
}

#[tokio::test]
async fn creator_authorization_and_request_dry_run_cannot_mutate_live_lobby() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(50_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, _, _) = live_app(clock, provider.clone());
    let (_, created) = create(&app, "create-authz", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();

    let (status, error) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_2)],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["code"], "not_creator");
    assert_eq!(provider.cleanup_count(), 0);

    let (status, error) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[
            ("x-spurfire-player-id", PLAYER_1),
            ("x-spurfire-dry-run", "1"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "dry_run_mode_mismatch");
    assert_eq!(
        get_authorized(&app, lobby_id, capability).await.1["state"],
        "FORMING"
    );
    assert_eq!(provider.cleanup_count(), 0);

    let (status, deleted) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["state"], "DESTROYED");
    assert_eq!(provider.cleanup_count(), 1);
}

#[tokio::test]
async fn cleanup_retries_unrevoked_credentials_after_destroy() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(60_000)));
    let provider = Arc::new(RecordingProvider::with_pending_cleanups(1));
    let (app, _, _) = live_app(clock, provider.clone());
    let (_, created) = create(&app, "create-cleanup", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    assert_eq!(
        join(&app, lobby_id, PLAYER_1, "cleanup-join").await.0,
        StatusCode::CREATED
    );

    let (_, first) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    assert_eq!(first["state"], "DESTROYED");
    assert_eq!(first["cleanup_pending"], true);

    let (_, second) = json_request(
        &app,
        Method::DELETE,
        &format!("/v1/lobbies/{lobby_id}"),
        None,
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    assert!(second["cleanup_pending"].is_null() || second["cleanup_pending"] == false);
    assert_eq!(provider.cleanup_count(), 2);
    let requests = provider.cleanup_requests();
    assert_eq!(requests[0].credentials.len(), 1);
    assert_eq!(requests[1].credentials.len(), 1);
    assert!(requests.iter().all(|request| request.include_devices));
}

#[tokio::test]
async fn leave_revokes_key_and_removes_roster_entry() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(70_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, _, _) = live_app(clock, provider.clone());
    let (_, created) = create(&app, "create-leave", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    join(&app, lobby_id, PLAYER_2, "leave-join").await;

    let (status, left) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/leave"),
        Some(json!({"player_id": PLAYER_2})),
        &[("x-spurfire-player-id", PLAYER_2)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(left["left"], true);
    assert_eq!(
        get_authorized(&app, lobby_id, capability).await.1["roster"],
        json!([])
    );
    let requests = provider.cleanup_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].credentials.len(), 1);
    assert!(!requests[0].include_devices);
}

#[tokio::test]
async fn readiness_stales_at_sixty_seconds_and_start_times_out() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(100_000)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, state, _) = dry_app(clock.clone(), provider);
    let (_, created) = create(&app, "create-time", "dry_run", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    make_ready(&app, lobby_id, &[(PLAYER_1, 10), (PLAYER_2, 20)]).await;
    assert_eq!(get(&app, lobby_id).await.1["state"], "READY");
    clock.advance(59_999);
    assert_eq!(get(&app, lobby_id).await.1["state"], "READY");
    clock.advance(1);
    assert_eq!(get(&app, lobby_id).await.1["state"], "FORMING");

    // Refresh both rows and start, then cross the exact 120-second timeout.
    measurement(&app, lobby_id, PLAYER_1, 10, 1).await;
    measurement(&app, lobby_id, PLAYER_2, 20, 1).await;
    let (_, authority) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}/authority"),
        None,
        &[],
    )
    .await;
    assert!(authority["input_hash"].is_string());
    let (status, _) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/start"),
        Some(json!({"creator_player_id": PLAYER_1})),
        &[
            ("idempotency-key", "start-timeout"),
            ("x-spurfire-player-id", PLAYER_1),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    clock.advance(119_999);
    assert_eq!(get(&app, lobby_id).await.1["state"], "STARTING");
    clock.advance(1);
    let (_, failed) = get_authorized(&app, lobby_id, capability).await;
    assert_eq!(failed["state"], "FAILED");
    assert_eq!(failed["state_reason"], "start_timeout");
    assert!(!state.cleanup_expired_now().await.is_empty() || failed["state"] == "FAILED");
}

#[tokio::test]
async fn idle_ttl_expires_and_mixed_formula_or_major_wire_cannot_start() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(200_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, state, _) = live_app(clock.clone(), provider);
    let (_, created) = create(&app, "create-expiry", "shared_tailnet", 2).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let capability = created["creator_capability"].as_str().unwrap();
    clock.advance(10 * 60 * 1_000);
    assert!(!state.cleanup_expired_now().await.is_empty());
    assert_eq!(
        get_authorized(&app, lobby_id, capability).await.1["state"],
        "EXPIRED"
    );

    let clock = Arc::new(ManualClock::new(UnixMillis::new(300_000)));
    let provider = Arc::new(RecordingProvider::available());
    let (app, _, _) = live_app(clock, provider);
    let (_, created) = create(&app, "create-wire", "shared_tailnet", 3).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    let (status, error) = join_with(
        &app,
        lobby_id,
        PLAYER_3,
        "major-mismatch",
        "2.0",
        "election_v1",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "wire_version_incompatible");

    assert_eq!(
        join(&app, lobby_id, PLAYER_1, "formula-p1").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        join_with(&app, lobby_id, PLAYER_2, "formula-p2", "1.9", "election_v2",)
            .await
            .0,
        StatusCode::CREATED
    );
    measurement(&app, lobby_id, PLAYER_1, 10, 1).await;
    measurement(&app, lobby_id, PLAYER_2, 20, 1).await;
    let (status, error) = json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/start"),
        Some(json!({"creator_player_id": PLAYER_1})),
        &[
            ("idempotency-key", "mixed-formula-start"),
            ("x-spurfire-player-id", PLAYER_1),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "authority_formula_incompatible");
}

#[tokio::test]
async fn silent_authority_migrates_deterministically_after_two_seconds() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(500_000)));
    let provider = Arc::new(DryRunProvider::new());
    let (app, _, _) = dry_app(clock.clone(), provider);
    let (_, created) = create(&app, "create-migration", "dry_run", 3).await;
    let lobby_id = created["lobby_id"].as_str().unwrap();
    make_ready(
        &app,
        lobby_id,
        &[(PLAYER_1, 10), (PLAYER_2, 20), (PLAYER_3, 40)],
    )
    .await;
    let (_, authority) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}/authority"),
        None,
        &[],
    )
    .await;
    assert_eq!(authority["winner_player_id"], PLAYER_1);
    let hash = authority["input_hash"].clone();
    json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/start"),
        Some(json!({"creator_player_id": PLAYER_1})),
        &[
            ("idempotency-key", "migration-start"),
            ("x-spurfire-player-id", PLAYER_1),
        ],
    )
    .await;
    json_request(
        &app,
        Method::POST,
        &format!("/v1/lobbies/{lobby_id}/heartbeat"),
        Some(json!({"player_id": PLAYER_1, "input_hash": hash})),
        &[("x-spurfire-player-id", PLAYER_1)],
    )
    .await;
    clock.advance(2_000);
    measurement(&app, lobby_id, PLAYER_2, 5, 2).await;
    let (_, migrated) = json_request(
        &app,
        Method::GET,
        &format!("/v1/lobbies/{lobby_id}/authority"),
        None,
        &[],
    )
    .await;
    assert_eq!(migrated["winner_player_id"], PLAYER_2);
    assert_ne!(migrated["input_hash"], authority["input_hash"]);
    assert_eq!(migrated["input"]["candidates"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn malformed_content_types_and_oversized_bodies_keep_http_statuses() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(600_000)));
    let (app, _, _) = dry_app(clock, Arc::new(DryRunProvider::new()));

    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/lobbies")
        .header("idempotency-key", "wrong-content")
        .header("x-spurfire-player-id", PLAYER_1)
        .body(Body::from("{}"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/lobbies")
        .header("content-type", "application/json")
        .header("idempotency-key", "oversized")
        .header("x-spurfire-player-id", PLAYER_1)
        .body(Body::from(format!(
            "{{\"display_name\":\"{}\",\"provisioning_mode\":\"dry_run\"}}",
            "x".repeat(70_000)
        )))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn service_capacity_validation_precedes_provider_mutation() {
    let clock = Arc::new(ManualClock::new(UnixMillis::new(700_000)));
    let provider = Arc::new(RecordingProvider::available());
    let store = Arc::new(InMemoryStore::new());
    let config = Config {
        max_players: 4,
        ..Config::default()
    };
    let state = AppState::new(config, store, provider.clone()).with_clock(clock);
    let app = build_router(state);

    let (status, error) = create(&app, "too-large", "shared_tailnet", 5).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "max_players_exceeds_service_limit");
    assert_eq!(provider.mutation_count(), 0);
}

#[test]
fn server_manifest_cannot_gain_tailnet_membership_or_gameplay_runtime_dependencies() {
    let manifest = include_str!("../Cargo.toml").to_ascii_lowercase();
    for forbidden in [
        "spurfire-net",
        "rustscale",
        "tailscale-tsnet",
        "boringtun",
        "gameplay-listener",
        "observer-node",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "server dependency boundary contains {forbidden}"
        );
    }
}

#[test]
fn test_ids_remain_distinct() {
    assert_ne!(PLAYER_1, PLAYER_2);
    assert_ne!(PLAYER_3, PLAYER_4);
}
