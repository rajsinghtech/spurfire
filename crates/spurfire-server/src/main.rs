//! `spurfire-server` binary.

use std::{future::pending, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use clap::Parser;
use spurfire_protocol::ProvisioningMode;
use spurfire_server::{
    build_router, AppState, Config, DryRunProvider, InMemoryStore, JsonFileStore, LobbyStore,
    NetworkProvider, TailscaleProvider,
};

#[derive(Debug, Parser)]
#[command(
    name = "spurfire-server",
    version,
    about = "Prototype Spurfire lobby control service (gameplay remains peer-to-peer)"
)]
struct Args {
    /// Override SPURFIRE_BIND_ADDR.
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Force zero-mutation dry-run mode.
    #[arg(long)]
    dry_run: bool,
    /// Override SPURFIRE_STATE_PATH for durable non-secret state.
    #[arg(long)]
    state_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse first so `--help` never depends on a local .env file or credentials.
    let args = Args::parse();
    if let Err(error) = dotenvy::dotenv() {
        if !error.not_found() {
            return Err(error.into());
        }
    }
    let mut config = Config::from_env()?;
    if let Some(bind) = args.bind {
        config.bind_addr = bind;
    }
    if let Some(state_path) = args.state_path {
        config.state_path = state_path;
    }
    if args.dry_run {
        config.force_dry_run = true;
        config.provisioning_mode = ProvisioningMode::DryRun;
    }

    let provider: Arc<dyn NetworkProvider> = if config.force_dry_run {
        Arc::new(DryRunProvider::new())
    } else {
        Arc::new(TailscaleProvider::from_env(config.shared_tailnet.clone()).await?)
    };
    let capabilities = provider.refresh_capabilities().await;
    let store: Arc<dyn LobbyStore> = if config.force_dry_run {
        Arc::new(InMemoryStore::new())
    } else {
        Arc::new(JsonFileStore::open(config.state_path.clone()).await?)
    };
    let state = AppState::new(config.clone(), store, provider);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let reaper = tokio::spawn(expiry_reaper(state.clone()));

    eprintln!(
        "spurfire-server listening on {} (mode={:?}, dry_run={}, shared_tailnet_ready={})",
        config.bind_addr,
        config.provisioning_mode,
        config.force_dry_run,
        capabilities.shared_tailnet_available()
    );
    let result = axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    reaper.abort();
    result?;
    Ok(())
}

async fn expiry_reaper(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let _ = state.cleanup_expired_now().await;
    }
}

async fn shutdown_signal() {
    let control_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};

        if let Ok(mut signal) = signal(SignalKind::terminate()) {
            let _ = signal.recv().await;
        } else {
            pending::<()>().await;
        }
    };

    #[cfg(windows)]
    let terminate = async {
        use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};

        let mut break_signal = ctrl_break().ok();
        let mut close_signal = ctrl_close().ok();
        let mut logoff_signal = ctrl_logoff().ok();
        let mut shutdown_signal = ctrl_shutdown().ok();
        tokio::select! {
            _ = async {
                if let Some(signal) = break_signal.as_mut() {
                    signal.recv().await;
                } else {
                    pending::<()>().await;
                }
            } => {},
            _ = async {
                if let Some(signal) = close_signal.as_mut() {
                    signal.recv().await;
                } else {
                    pending::<()>().await;
                }
            } => {},
            _ = async {
                if let Some(signal) = logoff_signal.as_mut() {
                    signal.recv().await;
                } else {
                    pending::<()>().await;
                }
            } => {},
            _ = async {
                if let Some(signal) = shutdown_signal.as_mut() {
                    signal.recv().await;
                } else {
                    pending::<()>().await;
                }
            } => {},
        }
    };

    #[cfg(not(any(unix, windows)))]
    let terminate = pending::<()>();

    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
}
