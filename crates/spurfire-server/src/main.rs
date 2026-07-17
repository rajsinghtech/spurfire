//! `spurfire-server` binary.

use std::{future::pending, sync::Arc, time::Duration};

use spurfire_server::{
    build_router, AppState, Config, DryRunProvider, InMemoryStore, NetworkProvider,
    TailscaleProvider,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An absent .env is fine; production should inject variables directly.
    let _ = dotenvy::dotenv();
    let config = Config::from_env()?;
    let provider: Arc<dyn NetworkProvider> = if config.force_dry_run {
        Arc::new(DryRunProvider::new())
    } else {
        Arc::new(TailscaleProvider::from_env(config.shared_tailnet.clone()).await?)
    };
    let state = AppState::new(config.clone(), Arc::new(InMemoryStore::new()), provider);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let reaper = tokio::spawn(expiry_reaper(state.clone()));

    eprintln!(
        "spurfire-server listening on {} (mode={:?}, dry_run={})",
        config.bind_addr, config.provisioning_mode, config.force_dry_run
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

    #[cfg(not(unix))]
    let terminate = pending::<()>();

    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
}
