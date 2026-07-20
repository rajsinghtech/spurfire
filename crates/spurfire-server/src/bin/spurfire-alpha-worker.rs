//! Credential-free HTTP worker. Linux inherited-descriptor activation only.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use spurfire_protocol::ProvisioningMode;
    use spurfire_server::{
        alpha_execution::{open_worker_authority, reject_worker_credential_environment},
        build_protected_alpha_public_router, build_router, AppState, BrokerFence, BrokerProvider,
        BrokerProviderTransport, CleanupOnlyBrokerTransport, Config, DryRunProvider, InMemoryStore,
        JsonFileStore, LobbyStore, MtlsBrokerProviderTransport, NetworkProvider,
    };
    use std::{
        io::{ErrorKind, Read},
        net::SocketAddr,
        os::fd::{FromRawFd, OwnedFd},
        sync::Arc,
        time::Duration,
    };
    use zeroize::{Zeroize, Zeroizing};

    reject_worker_credential_environment()?;
    if std::env::args_os().len() != 1 {
        return Err("protected worker accepts no argv".into());
    }
    // SAFETY: the measured PID1 launcher duplicates only its sealed authority
    // socket and pre-bound HTTP listener to these fixed descriptors.
    let control_fd = unsafe { OwnedFd::from_raw_fd(3) };
    let listener_fd = unsafe { OwnedFd::from_raw_fd(4) };
    let mut control = std::os::unix::net::UnixStream::from(control_fd);
    let mut length = [0; 4];
    if let Err(error) = control.read_exact(&mut length) {
        if error.kind() != ErrorKind::UnexpectedEof {
            return Err(error.into());
        }
        let std_listener = std::net::TcpListener::from(listener_fd);
        std_listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let config = Config {
            force_dry_run: true,
            provisioning_mode: ProvisioningMode::DryRun,
            real_mutations_enabled: false,
            real_admission_enabled: false,
            max_players: 2,
            ..Config::default()
        };
        let state = AppState::new_deny_all(
            config,
            Arc::new(InMemoryStore::new()),
            Arc::new(DryRunProvider::new()),
        );
        axum::serve(
            listener,
            build_router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        return Ok(());
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > 64 * 1024 {
        return Err("sealed authority oversized".into());
    }
    let mut sealed = Zeroizing::new(vec![0; length]);
    control.read_exact(&mut sealed)?;
    let (qualification, run_id, broker_address, lease) = open_worker_authority(&sealed)?;
    sealed.zeroize();

    let fence = BrokerFence {
        run_id,
        lobby_id: qualification.lobby_id(),
        generation: qualification.generation(),
        supervisor_epoch: qualification.initial_epoch(),
        phase: lease.binding.phase,
        admission_play_deadline: qualification.final_io_deadline(),
        cleanup_deadline: qualification.absolute_deadline(),
    };
    let transport = Arc::new(MtlsBrokerProviderTransport::new(
        broker_address,
        "spurfire-provider-broker",
        "/run/runtime-tls/ca.crt",
        "/run/runtime-tls/tls.crt",
        "/run/runtime-tls/tls.key",
        "/run/broker-mac/mac.key",
        fence,
        lease,
    )?);
    let cleanup_transport = CleanupOnlyBrokerTransport(Arc::clone(&transport));
    let broker_transport: Arc<dyn BrokerProviderTransport> = transport.clone();
    let provider: Arc<dyn NetworkProvider> = Arc::new(BrokerProvider::new(
        broker_transport,
        qualification.lobby_id(),
        qualification.generation(),
    ));
    let mut config = Config {
        bind_addr: "0.0.0.0:8080".parse()?,
        state_path: "/var/lib/spurfire/server-state.json".into(),
        max_players: 2,
        ..Config::default()
    };
    // Only the opaque verified constructor may turn these effective values on.
    config.force_dry_run = false;
    let store: Arc<dyn LobbyStore> = Arc::new(JsonFileStore::open(&config.state_path).await?);
    let protected_lobby_id = qualification.lobby_id();
    let cleanup_store = Arc::clone(&store);
    let recovering = store.protected_alpha_recovery().await.is_some();
    let state = if recovering {
        AppState::new_protected_alpha_recovery(config, store, provider, qualification).await?
    } else {
        AppState::new_protected_alpha(config, store, provider, qualification).await?
    };
    if !state.reconcile_startup().await {
        return Err("protected startup reconciliation failed".into());
    }
    if recovering {
        state.begin_protected_cleanup().await?;
    }
    control.set_nonblocking(true)?;
    let mut cleanup_signal = tokio::net::UnixStream::from_std(control)?;
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut byte = [0; 1];
        if cleanup_signal.read_exact(&mut byte).await.is_ok() && byte == *b"C" {
            let _ = cleanup_state.begin_protected_cleanup().await;
            loop {
                if cleanup_store
                    .get(protected_lobby_id)
                    .await
                    .is_none_or(|lobby| {
                        let snapshot = lobby.snapshot();
                        snapshot.state == spurfire_protocol::LobbyState::Destroyed
                            && !snapshot.cleanup_pending
                    })
                    && cleanup_transport.release_lease().await.is_ok()
                {
                    std::process::exit(0);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    });
    let reaper_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let _ = reaper_state.cleanup_expired_now().await;
        }
    });

    let std_listener = std::net::TcpListener::from(listener_fd);
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;
    axum::serve(
        listener,
        build_protected_alpha_public_router(state)
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("protected Alpha worker unsupported: Unix/Linux activation only");
    std::process::exit(78);
}
