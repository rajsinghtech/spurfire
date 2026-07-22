//! Credential-free, non-mutating provider bootstrap for one protected Alpha.
//!
//! This initializes and measures only the fixed durable lobby-store path. It
//! has no provider, Kubernetes, receipt, or secret input and emits only the
//! three non-secret digests an owner needs to bind a receipt.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use spurfire_server::{JsonFileStore, LobbyStore};

    const STATE_PATH: &str = "/var/lib/spurfire/server-state.json";

    #[derive(Serialize)]
    struct Evidence {
        schema_version: u8,
        store_instance_id_sha256: String,
        canonical_state_path_sha256: String,
        initial_state_sha256: String,
        empty_store: bool,
        real_lobby_lease_held: bool,
        protected_recovery_present: bool,
    }

    if std::env::args_os().len() != 1 {
        return Err("protected Alpha bootstrap accepts no argv".into());
    }
    let store = JsonFileStore::open(STATE_PATH).await?;
    let binding = store.store_binding().await;
    let state = std::fs::read(STATE_PATH)?;
    let evidence = Evidence {
        schema_version: 1,
        store_instance_id_sha256: hex::encode(binding.instance_id_sha256),
        canonical_state_path_sha256: hex::encode(binding.canonical_state_path_sha256),
        initial_state_sha256: hex::encode(Sha256::digest(state)),
        empty_store: store.is_empty().await,
        real_lobby_lease_held: store.real_lobby_lease_held().await,
        protected_recovery_present: store.protected_alpha_recovery().await.is_some(),
    };
    if !evidence.empty_store
        || evidence.real_lobby_lease_held
        || evidence.protected_recovery_present
    {
        return Err("protected Alpha store is not fresh".into());
    }
    serde_json::to_writer(std::io::stdout().lock(), &evidence)?;
    println!();
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("protected Alpha bootstrap unsupported: Unix/Linux only");
    std::process::exit(78);
}
