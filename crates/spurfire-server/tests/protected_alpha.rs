use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ed25519_dalek::{Signer, SigningKey};
use spurfire_protocol::{LobbyId, ProvisioningMode, UnixMillis};
use spurfire_server::{
    build_protected_alpha_public_router, verify_protected_alpha_receipt, AppState, Config,
    DryRunProvider, InMemoryStore, LobbyStore, ProtectedAlphaClaims, ProtectedAlphaReceipt,
    ProtectedAlphaVerificationContext, ALPHA_CLEANUP_MS, ALPHA_PLAY_MS, PROTECTED_ALPHA_AUDIENCE,
    PROTECTED_ALPHA_PURPOSE,
};
use std::{collections::BTreeMap, sync::Arc};
use tower::ServiceExt;

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn protected_public_router_has_only_literal_authorized_lobby_paths() {
    let store = Arc::new(InMemoryStore::new());
    let binding = store.store_binding().await;
    let lobby_id = LobbyId::parse("00000000-0000-4000-8000-0000000000ca").unwrap();
    let context = ProtectedAlphaVerificationContext {
        now: UnixMillis::new(1_000),
        source_sha: "source-sha".into(),
        runtime_image_digest: format!("sha256:{}", "1".repeat(64)),
        broker_image_digest: format!("sha256:{}", "2".repeat(64)),
        worker_sha256: [1; 32],
        broker_sha256: [2; 32],
        provenance_sha256: [3; 32],
        artifact_set_sha256: [4; 32],
        policy_profile_sha256: [5; 32],
        public_origin: "https://alpha.example.invalid".into(),
        internal_listener: "/run/spurfire/alpha.sock".into(),
        installation_id: "installation-alpha-0001".into(),
        store_instance_id_sha256: binding.instance_id_sha256,
        canonical_state_path_sha256: binding.canonical_state_path_sha256,
        initial_state_sha256: [6; 32],
        lease_uid: "lease-uid-1".into(),
        lease_resource_version: "17".into(),
        launch_code_sha256: [7; 32],
    };
    let signing = SigningKey::from_bytes(&[7; 32]);
    let claims = ProtectedAlphaClaims {
        audience: PROTECTED_ALPHA_AUDIENCE.into(),
        receipt_id: "0123456789abcdef0123456789abcdef".into(),
        source_sha: context.source_sha.clone(),
        runtime_image_digest: context.runtime_image_digest.clone(),
        broker_image_digest: context.broker_image_digest.clone(),
        worker_sha256: hex(&context.worker_sha256),
        broker_sha256: hex(&context.broker_sha256),
        provenance_sha256: hex(&context.provenance_sha256),
        artifact_set_sha256: hex(&context.artifact_set_sha256),
        policy_profile_sha256: hex(&context.policy_profile_sha256),
        public_origin: context.public_origin.clone(),
        internal_listener: context.internal_listener.clone(),
        lobby_id,
        network_generation: 1,
        installation_id: context.installation_id.clone(),
        store_instance_id_sha256: hex(&binding.instance_id_sha256),
        canonical_state_path_sha256: hex(&binding.canonical_state_path_sha256),
        initial_state_sha256: hex(&context.initial_state_sha256),
        lease_uid: context.lease_uid.clone(),
        lease_resource_version: context.lease_resource_version.clone(),
        lease_phase: "admission".into(),
        supervisor_run_id: "run-0123456789abcdef".into(),
        initial_epoch: 1,
        launch_code_sha256: hex(&context.launch_code_sha256),
        participant_cap: 2,
        issued_at: UnixMillis::new(999),
        expires_at: UnixMillis::new(61_000),
        final_io_deadline: UnixMillis::new(999 + ALPHA_PLAY_MS),
        absolute_deadline: UnixMillis::new(999 + ALPHA_PLAY_MS + ALPHA_CLEANUP_MS),
        provisioning_mode: ProvisioningMode::TailnetPerLobby,
        hosted: true,
        purpose: PROTECTED_ALPHA_PURPOSE.into(),
        owner_key_id: "owner".into(),
    };
    let signature = signing
        .sign(&serde_json::to_vec(&claims).unwrap())
        .to_bytes()
        .to_vec();
    let mut receipt = serde_json::to_vec(&ProtectedAlphaReceipt { claims, signature }).unwrap();
    let qualification = verify_protected_alpha_receipt(
        &mut receipt,
        &BTreeMap::from([("owner".into(), signing.verifying_key())]),
        &context,
    )
    .unwrap();
    let config = Config {
        bind_addr: "127.0.0.1:8081".parse().unwrap(),
        ..Config::default()
    };
    let state = AppState::new_protected_alpha(
        config,
        store,
        Arc::new(DryRunProvider::new()),
        qualification,
    )
    .await
    .unwrap();
    let router = build_protected_alpha_public_router(state);
    let create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/lobbies")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(create.status(), StatusCode::NOT_FOUND);
    for (method, path) in [
        ("GET", "/v1/lobbies/00000000-0000-4000-8000-0000000000cb"),
        ("GET", "/inspect"),
        ("GET", "/protected-alpha/internal/create"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}
