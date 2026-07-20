//! Separate credential-owning provider broker pod.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use serde::Deserialize;
    use spurfire_protocol::UnixMillis;
    use spurfire_server::{
        alpha_execution::{open_fixed_sibling, ProtectedRole},
        owner_key::{verifying_key, OWNER_KEY_ID},
        verify_protected_alpha_recovery_receipt, BrokerFence, BrokerServer, EncryptedChildVault,
        KubernetesLeaseAuthority, NetworkProvider, ProtectedAlphaReceipt,
        ProtectedAlphaVerificationContext, ProtectedPhase, TailscaleProvider,
    };
    use std::{
        collections::BTreeMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use zeroize::{Zeroize, Zeroizing};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicConfig {
        run_id: String,
        namespace: String,
        lease_name: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OAuth {
        api_base: String,
        client_id: String,
        client_secret: String,
    }

    if std::env::args_os().len() != 1 {
        return Err("broker accepts no argv".into());
    }
    let config: PublicConfig =
        serde_json::from_slice(&std::fs::read("/run/alpha-config/broker.json")?)?;
    let mut receipt_bytes = Zeroizing::new(std::fs::read("/run/alpha-receipt/receipt.json")?);
    let receipt: ProtectedAlphaReceipt = serde_json::from_slice(&receipt_bytes)?;
    let decode = |value: &str| -> Result<[u8; 32], Box<dyn std::error::Error>> {
        Ok(hex::decode(value)?
            .try_into()
            .map_err(|_| "digest malformed")?)
    };
    let broker_digest = decode(&receipt.claims.broker_sha256)?;
    // This measures the executable that actually owns credentials, rather than
    // a same-named sibling in the runtime image.
    let _broker = open_fixed_sibling(ProtectedRole::Broker, broker_digest)?;
    let pod_binding = |name: &str| -> Result<String, Box<dyn std::error::Error>> {
        Ok(std::fs::read_to_string(format!("/run/alpha-pod/{name}"))?
            .trim()
            .to_owned())
    };
    let runtime_image_digest = pod_binding("runtime-image-digest")?;
    let broker_image_digest = pod_binding("broker-image-digest")?;
    if config.run_id != receipt.claims.supervisor_run_id {
        return Err("broker run binding mismatch".into());
    }
    let lease = Arc::new(KubernetesLeaseAuthority::from_service_account(
        config.namespace,
        config.lease_name,
        "/var/run/secrets/kubernetes.io/serviceaccount/token",
        "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
    )?);
    let snapshot = lease.read().await?;
    let now = UnixMillis::new(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()?,
    );
    let context = ProtectedAlphaVerificationContext {
        now,
        source_sha: pod_binding("source-sha")?,
        runtime_image_digest,
        broker_image_digest,
        worker_sha256: decode(&receipt.claims.worker_sha256)?,
        broker_sha256: broker_digest,
        provenance_sha256: decode(&pod_binding("provenance-sha256")?)?,
        artifact_set_sha256: decode(&pod_binding("artifact-set-sha256")?)?,
        policy_profile_sha256: decode(&pod_binding("policy-profile-sha256")?)?,
        public_origin: pod_binding("public-origin")?,
        internal_listener: pod_binding("internal-listener")?,
        installation_id: receipt.claims.installation_id.clone(),
        store_instance_id_sha256: decode(&receipt.claims.store_instance_id_sha256)?,
        canonical_state_path_sha256: decode(&receipt.claims.canonical_state_path_sha256)?,
        initial_state_sha256: decode(&receipt.claims.initial_state_sha256)?,
        lease_uid: receipt.claims.lease_uid.clone(),
        lease_resource_version: receipt.claims.lease_resource_version.clone(),
        launch_code_sha256: decode(&receipt.claims.launch_code_sha256)?,
    };
    let qualification = verify_protected_alpha_recovery_receipt(
        &mut receipt_bytes,
        &BTreeMap::from([(OWNER_KEY_ID.to_owned(), verifying_key()?)]),
        &context,
    )?;
    if snapshot.uid != receipt.claims.lease_uid
        || snapshot.binding.installation_id != receipt.claims.installation_id
        || snapshot.binding.state_store_id_sha256 != context.store_instance_id_sha256
        || snapshot.binding.receipt_digest != qualification.receipt_digest()
        || snapshot.binding.lobby_id != receipt.claims.lobby_id
        || snapshot.binding.generation != receipt.claims.network_generation
        || snapshot.binding.supervisor_epoch < receipt.claims.initial_epoch
        || snapshot.binding.admission_play_deadline != receipt.claims.final_io_deadline
        || snapshot.binding.cleanup_deadline != receipt.claims.absolute_deadline
        || !matches!(
            snapshot.binding.phase,
            ProtectedPhase::Admission | ProtectedPhase::CleanupOnly
        )
    {
        return Err("owner-signed Lease binding mismatch".into());
    }
    let fence = BrokerFence {
        run_id: config.run_id,
        lobby_id: snapshot.binding.lobby_id,
        generation: snapshot.binding.generation,
        supervisor_epoch: snapshot.binding.supervisor_epoch,
        phase: snapshot.binding.phase,
        admission_play_deadline: receipt.claims.final_io_deadline,
        cleanup_deadline: receipt.claims.absolute_deadline,
    };
    // Provider custody is opened only after owner signature, executable/image
    // measurements and the immutable Lease binding all verify.
    let mut oauth_bytes =
        Zeroizing::new(std::fs::read("/run/alpha-custody/organization-oauth.json")?);
    let mut oauth: OAuth = serde_json::from_slice(&oauth_bytes)?;
    oauth_bytes.zeroize();
    let vault = Arc::new(
        EncryptedChildVault::open(
            "/var/lib/spurfire/child-vault.json",
            "/run/alpha-vault-key/vault.key",
        )
        .await?,
    );
    let client_id = Zeroizing::new(std::mem::take(&mut oauth.client_id));
    let client_secret = Zeroizing::new(std::mem::take(&mut oauth.client_secret));
    oauth.api_base.shrink_to_fit();
    let provider: Arc<dyn NetworkProvider> =
        Arc::new(TailscaleProvider::from_mounted_credentials_with_vault(
            oauth.api_base,
            client_id,
            client_secret,
            "-",
            vault,
        ));
    let capabilities = provider.refresh_capabilities().await;
    if !capabilities.tailnet_per_lobby_available() {
        return Err("provider capabilities unavailable".into());
    }
    let server = BrokerServer::bind(
        "0.0.0.0:9443",
        "/run/broker-tls/ca.crt",
        "/run/broker-tls/tls.crt",
        "/run/broker-tls/tls.key",
        "/run/broker-mac/mac.key",
        fence,
        lease,
        provider,
    )
    .await?;
    server.serve().await?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("protected provider broker unsupported: Unix/Linux activation only");
    std::process::exit(78);
}
