//! Protected Linux PID1 launcher with fixed measured sibling supervision.

#[cfg(target_os = "linux")]
fn main() {
    if let Err(message) = run() {
        eprintln!("protected Alpha launcher failed closed: {message}");
        std::process::exit(78);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), &'static str> {
    use sha2::{Digest, Sha256};
    use spurfire_protocol::UnixMillis;
    use spurfire_server::{
        alpha_execution::{
            open_fixed_sibling, seal_worker_authority, spawn_protected, ProtectedRole,
        },
        owner_key::{verifying_key, OWNER_KEY_ID},
        verify_protected_alpha_receipt, JsonFileStore, KubernetesLeaseAuthority, LobbyStore,
        ProtectedAlphaReceipt, ProtectedAlphaVerificationContext, ALPHA_CLEANUP_MS, ALPHA_PLAY_MS,
    };
    use std::{
        collections::BTreeMap,
        fs::File,
        io::Write,
        net::TcpListener,
        os::fd::OwnedFd,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use zeroize::{Zeroize, Zeroizing};

    if std::env::args_os().len() != 1 {
        return Err("argv is forbidden");
    }
    let mut receipt_bytes = Zeroizing::new(
        std::fs::read("/run/alpha-receipt/receipt.json").map_err(|_| "receipt unavailable")?,
    );
    let receipt: ProtectedAlphaReceipt =
        serde_json::from_slice(&receipt_bytes).map_err(|_| "receipt malformed")?;
    if receipt.claims.owner_key_id != OWNER_KEY_ID
        || receipt.claims.participant_cap != 2
        || receipt.claims.lease_phase != "admission"
        || receipt.claims.final_io_deadline.as_millis()
            != receipt
                .claims
                .issued_at
                .as_millis()
                .saturating_add(ALPHA_PLAY_MS)
        || receipt.claims.absolute_deadline.as_millis()
            != receipt
                .claims
                .final_io_deadline
                .as_millis()
                .saturating_add(ALPHA_CLEANUP_MS)
    {
        return Err("receipt rejected");
    }
    let decode = |value: &str| -> Result<[u8; 32], &'static str> {
        hex::decode(value)
            .map_err(|_| "digest malformed")?
            .try_into()
            .map_err(|_| "digest malformed")
    };
    let worker_digest = decode(&receipt.claims.worker_sha256)?;
    let broker_digest = decode(&receipt.claims.broker_sha256)?;
    let worker = open_fixed_sibling(ProtectedRole::Worker, worker_digest)
        .map_err(|_| "worker measurement failed")?;
    let _broker = open_fixed_sibling(ProtectedRole::Broker, broker_digest)
        .map_err(|_| "broker measurement failed")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "runtime unavailable")?;
    let store = runtime
        .block_on(JsonFileStore::open("/var/lib/spurfire/server-state.json"))
        .map_err(|_| "state store unavailable")?;
    let binding = runtime.block_on(store.store_binding());
    let namespace =
        std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
            .map_err(|_| "namespace unavailable")?;
    let lease_authority = KubernetesLeaseAuthority::from_service_account(
        namespace.trim(),
        "spurfire-protected-alpha",
        "/var/run/secrets/kubernetes.io/serviceaccount/token",
        "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
    )
    .map_err(|_| "Lease authority unavailable")?;
    let lease = runtime
        .block_on(lease_authority.read())
        .map_err(|_| "Lease authority unavailable")?;
    let state_bytes =
        std::fs::read("/var/lib/spurfire/server-state.json").map_err(|_| "state unavailable")?;
    let state_sha256: [u8; 32] = Sha256::digest(&state_bytes).into();
    let context = ProtectedAlphaVerificationContext {
        now: UnixMillis::new(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "clock unavailable")?
                .as_millis()
                .try_into()
                .map_err(|_| "clock unavailable")?,
        ),
        source_sha: receipt.claims.source_sha.clone(),
        runtime_image_digest: receipt.claims.runtime_image_digest.clone(),
        broker_image_digest: receipt.claims.broker_image_digest.clone(),
        worker_sha256: worker_digest,
        broker_sha256: broker_digest,
        provenance_sha256: decode(&receipt.claims.provenance_sha256)?,
        artifact_set_sha256: decode(&receipt.claims.artifact_set_sha256)?,
        policy_profile_sha256: decode(&receipt.claims.policy_profile_sha256)?,
        public_origin: receipt.claims.public_origin.clone(),
        internal_listener: receipt.claims.internal_listener.clone(),
        installation_id: lease.binding.installation_id.clone(),
        store_instance_id_sha256: binding.instance_id_sha256,
        canonical_state_path_sha256: binding.canonical_state_path_sha256,
        initial_state_sha256: state_sha256,
        lease_uid: lease.uid.clone(),
        lease_resource_version: lease.resource_version.clone(),
        launch_code_sha256: decode(&receipt.claims.launch_code_sha256)?,
    };
    let qualification = verify_protected_alpha_receipt(
        &mut receipt_bytes,
        &BTreeMap::from([(
            OWNER_KEY_ID.to_owned(),
            verifying_key().map_err(|_| "compiled owner key invalid")?,
        )]),
        &context,
    )
    .map_err(|_| "receipt rejected")?;
    if lease.binding.state_store_id_sha256 != binding.instance_id_sha256
        || lease.binding.receipt_digest != qualification.receipt_digest()
        || lease.binding.lobby_id != qualification.lobby_id()
        || lease.binding.generation != qualification.generation()
        || lease.binding.supervisor_epoch != qualification.initial_epoch()
        || lease.binding.state_sha256 != state_sha256
    {
        return Err("Lease binding mismatch");
    }
    let sealed = Zeroizing::new(
        seal_worker_authority(
            qualification,
            receipt.claims.supervisor_run_id.clone(),
            receipt.claims.internal_listener.clone(),
            lease,
        )
        .map_err(|_| "authority sealing failed")?,
    );
    receipt_bytes.zeroize();

    let listener = TcpListener::bind("0.0.0.0:8080").map_err(|_| "listener bind failed")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "listener setup failed")?;
    let (mut launcher_control, child_control) =
        std::os::unix::net::UnixStream::pair().map_err(|_| "socketpair failed")?;
    let child_control = File::from(OwnedFd::from(child_control));
    let child_listener = File::from(OwnedFd::from(
        listener.try_clone().map_err(|_| "listener clone failed")?,
    ));
    let mut child = spawn_protected(&worker, vec![(child_control, 3), (child_listener, 4)])
        .map_err(|_| "worker spawn failed")?;
    launcher_control
        .write_all(
            &u32::try_from(sealed.len())
                .map_err(|_| "authority oversized")?
                .to_be_bytes(),
        )
        .and_then(|()| launcher_control.write_all(&sealed))
        .map_err(|_| "authority transfer failed")?;
    let started = Instant::now();
    let mut cleanup_sent = false;
    loop {
        if child
            .try_wait()
            .map_err(|_| "worker wait failed")?
            .is_some()
        {
            if !cleanup_sent {
                return Err("measured worker exited before cleanup");
            }
            drop(launcher_control);
            let (parent, deny_control) =
                std::os::unix::net::UnixStream::pair().map_err(|_| "deny socketpair failed")?;
            let deny_control = File::from(OwnedFd::from(deny_control));
            let deny_listener = File::from(OwnedFd::from(
                listener.try_clone().map_err(|_| "listener clone failed")?,
            ));
            let mut deny = spawn_protected(&worker, vec![(deny_control, 3), (deny_listener, 4)])
                .map_err(|_| "deny worker spawn failed")?;
            drop(parent);
            let _ = deny.wait();
            return Err("credential-free worker exited");
        }
        if !cleanup_sent && started.elapsed() >= Duration::from_millis(ALPHA_PLAY_MS) {
            launcher_control
                .write_all(b"C")
                .map_err(|_| "cleanup transition failed")?;
            cleanup_sent = true;
        }
        if started.elapsed() >= Duration::from_millis(ALPHA_PLAY_MS + ALPHA_CLEANUP_MS) {
            let process_group = -(i32::try_from(child.id()).map_err(|_| "worker pid invalid")?);
            unsafe {
                libc::kill(process_group, libc::SIGKILL);
            }
            let _ = child.wait();
            drop(launcher_control);
            let (parent, deny_control) =
                std::os::unix::net::UnixStream::pair().map_err(|_| "deny socketpair failed")?;
            let deny_control = File::from(OwnedFd::from(deny_control));
            let deny_listener = File::from(OwnedFd::from(
                listener.try_clone().map_err(|_| "listener clone failed")?,
            ));
            let mut deny = spawn_protected(&worker, vec![(deny_control, 3), (deny_listener, 4)])
                .map_err(|_| "deny worker spawn failed")?;
            drop(parent);
            let _ = deny.wait();
            return Err("credential-free worker exited");
        }
        std::thread::park_timeout(Duration::from_millis(250));
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("protected Alpha launcher unsupported: Unix/Linux activation only");
    std::process::exit(78);
}
