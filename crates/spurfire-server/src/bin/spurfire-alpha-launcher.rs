//! Protected Linux PID1 launcher with fixed measured sibling supervision.

#[cfg(target_os = "linux")]
static TERMINATE_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "linux")]
extern "C" fn request_termination(_: libc::c_int) {
    TERMINATE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

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
        verify_protected_alpha_receipt, verify_protected_alpha_recovery_receipt, JsonFileStore,
        KubernetesLeaseAuthority, LobbyStore, ProtectedAlphaReceipt,
        ProtectedAlphaVerificationContext, ProtectedPhase, ALPHA_CLEANUP_MS, ALPHA_PLAY_MS,
    };
    use std::{
        collections::BTreeMap,
        fs::File,
        io::Write,
        net::TcpListener,
        os::fd::OwnedFd,
        sync::atomic::Ordering,
        time::{Duration, SystemTime, UNIX_EPOCH},
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
    // The credential-owning broker independently measures its own executable;
    // measuring a runtime-image sibling here would attest the wrong process.

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "runtime unavailable")?;
    let store = runtime
        .block_on(JsonFileStore::open("/var/lib/spurfire/server-state.json"))
        .map_err(|_| "state store unavailable")?;
    let binding = runtime.block_on(store.store_binding());
    let cleanup_recovery = runtime.block_on(store.protected_alpha_recovery()).is_some();
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
    let pod_binding = |name: &str| -> Result<String, &'static str> {
        std::fs::read_to_string(format!("/run/alpha-pod/{name}"))
            .map(|value| value.trim().to_owned())
            .map_err(|_| "deployment binding unavailable")
    };
    let context = ProtectedAlphaVerificationContext {
        now: UnixMillis::new(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "clock unavailable")?
                .as_millis()
                .try_into()
                .map_err(|_| "clock unavailable")?,
        ),
        source_sha: pod_binding("source-sha")?,
        runtime_image_digest: pod_binding("runtime-image-digest")?,
        broker_image_digest: pod_binding("broker-image-digest")?,
        worker_sha256: worker_digest,
        broker_sha256: broker_digest,
        provenance_sha256: decode(&pod_binding("provenance-sha256")?)?,
        artifact_set_sha256: decode(&pod_binding("artifact-set-sha256")?)?,
        policy_profile_sha256: decode(&pod_binding("policy-profile-sha256")?)?,
        public_origin: pod_binding("public-origin")?,
        internal_listener: pod_binding("internal-listener")?,
        installation_id: lease.binding.installation_id.clone(),
        store_instance_id_sha256: binding.instance_id_sha256,
        canonical_state_path_sha256: binding.canonical_state_path_sha256,
        initial_state_sha256: if cleanup_recovery {
            decode(&receipt.claims.initial_state_sha256)?
        } else {
            state_sha256
        },
        lease_uid: lease.uid.clone(),
        lease_resource_version: if cleanup_recovery {
            receipt.claims.lease_resource_version.clone()
        } else {
            lease.resource_version.clone()
        },
        launch_code_sha256: decode(&receipt.claims.launch_code_sha256)?,
    };
    let keys = BTreeMap::from([(
        OWNER_KEY_ID.to_owned(),
        verifying_key().map_err(|_| "compiled owner key invalid")?,
    )]);
    let qualification = if cleanup_recovery {
        verify_protected_alpha_recovery_receipt(&mut receipt_bytes, &keys, &context)
    } else {
        verify_protected_alpha_receipt(&mut receipt_bytes, &keys, &context)
    }
    .map_err(|_| "receipt rejected")?;
    let immutable_lease_matches = lease.binding.installation_id == receipt.claims.installation_id
        && lease.binding.state_store_id_sha256 == binding.instance_id_sha256
        && lease.binding.receipt_digest == qualification.receipt_digest()
        && lease.binding.lobby_id == qualification.lobby_id()
        && lease.binding.generation == qualification.generation()
        && lease.binding.admission_play_deadline == qualification.final_io_deadline()
        && lease.binding.cleanup_deadline == qualification.absolute_deadline();
    let phase_matches = if cleanup_recovery {
        lease.binding.supervisor_epoch >= qualification.initial_epoch()
            && matches!(
                lease.binding.phase,
                ProtectedPhase::Admission | ProtectedPhase::CleanupOnly
            )
    } else {
        lease.binding.supervisor_epoch == qualification.initial_epoch()
            && lease.binding.state_sha256 == state_sha256
            && lease.binding.phase == ProtectedPhase::Admission
    };
    if !immutable_lease_matches || !phase_matches {
        return Err("Lease binding mismatch");
    }
    let final_io_deadline = qualification.final_io_deadline().as_millis();
    let absolute_deadline = qualification.absolute_deadline().as_millis();
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
    // PID1 must turn Kubernetes termination into cleanup, rather than relying
    // on PDEATHSIG to kill the only process capable of requesting it.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_termination as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            request_termination as *const () as libc::sighandler_t,
        );
    }
    let wall_now = || -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(u64::MAX, |value| {
                value.as_millis().try_into().unwrap_or(u64::MAX)
            })
    };
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
        if !cleanup_sent
            && (cleanup_recovery
                || TERMINATE_REQUESTED.load(Ordering::SeqCst)
                || wall_now() >= final_io_deadline)
        {
            launcher_control
                .write_all(b"C")
                .map_err(|_| "cleanup transition failed")?;
            cleanup_sent = true;
        }
        if wall_now() >= absolute_deadline {
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
