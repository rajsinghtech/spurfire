//! Separate credential-owning provider broker pod.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use serde::Deserialize;
    use spurfire_server::{
        BrokerFence, BrokerServer, EncryptedChildVault, KubernetesLeaseAuthority, NetworkProvider,
        TailscaleProvider,
    };
    use std::sync::Arc;
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
    let mut oauth_bytes =
        Zeroizing::new(std::fs::read("/run/alpha-custody/organization-oauth.json")?);
    let mut oauth: OAuth = serde_json::from_slice(&oauth_bytes)?;
    oauth_bytes.zeroize();
    let lease = Arc::new(KubernetesLeaseAuthority::from_service_account(
        config.namespace,
        config.lease_name,
        "/var/run/secrets/kubernetes.io/serviceaccount/token",
        "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
    )?);
    let snapshot = lease.read().await?;
    let fence = BrokerFence {
        run_id: config.run_id,
        lobby_id: snapshot.binding.lobby_id,
        generation: snapshot.binding.generation,
        supervisor_epoch: snapshot.binding.supervisor_epoch,
        phase: snapshot.binding.phase,
        admission_play_deadline: snapshot.binding.admission_play_deadline,
        cleanup_deadline: snapshot.binding.cleanup_deadline,
    };
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
