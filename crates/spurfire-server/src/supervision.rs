//! Default-inert external rehearsal supervision state machine.
//!
//! This module contains no activation path and never reads process arguments or
//! environment variables. A separately reviewed launcher may supply a private
//! inherited descriptor to a broker on Linux/macOS. The ordinary HTTP server
//! cannot construct a [`SupervisorLedger`] or obtain a [`Fence`]. Windows stays
//! unsupported until equivalent handle, DACL, and atomic-replacement proofs exist.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spurfire_protocol::{LobbyId, UnixMillis};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const SCHEMA_VERSION: u8 = 1;
const MAX_IPC_FRAME: usize = 16 * 1024;
pub const MIN_ABSENCE_SEPARATION: Duration = Duration::from_secs(5);

/// Immutable authorization tuple checked before every broker operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub run_id: String,
    pub lobby_id: LobbyId,
    pub generation: u64,
    pub epoch: u64,
    pub token_sha256: [u8; 32],
}

/// Stable provider identity. Display names are deliberately insufficient.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisedIdentity {
    pub provider_stable_id: String,
    pub fqdn: String,
    pub vault_version: u64,
}

/// Every external operation has a durable intent and an exact outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    ApplyPolicy,
    FinalProviderIo,
    Delete,
    ObserveAbsence,
    VaultErase,
    LeaseRelease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transition {
    Reserved,
    Intent {
        operation: Operation,
    },
    Outcome {
        operation: Operation,
        outcome: OperationOutcome,
    },
    IdentityBound {
        identity: SupervisedIdentity,
    },
    CleanupOnly,
    AbsenceObserved {
        ordinal: u8,
        provider_stable_id: String,
        fully_paginated: bool,
        uncached: bool,
    },
    VaultTombstoneReadback {
        version: u64,
    },
    Released,
    Quarantined {
        reason: String,
    },
}

/// Fsynced authority record. Immutable fields are repeated in every snapshot;
/// transitions are append-only and hash chained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorLedger {
    schema_version: u8,
    pub fence: Fence,
    pub receipt_id_digest: [u8; 32],
    pub receipt_verifier: [u8; 32],
    pub worker_sha256: [u8; 32],
    pub policy_profile_digest: [u8; 32],
    pub absolute_deadline: UnixMillis,
    pub final_io_deadline: UnixMillis,
    pub cleanup_only: bool,
    pub quarantined: bool,
    pub identity: Option<SupervisedIdentity>,
    pub transitions: Vec<LedgerEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    pub sequence: u64,
    pub previous_sha256: [u8; 32],
    pub transition: Transition,
    pub entry_sha256: [u8; 32],
}

#[derive(Debug, Error)]
pub enum SupervisionError {
    #[error("supervision is unsupported on this platform")]
    Unsupported,
    #[error("supervisor ledger is invalid or unsafe")]
    InvalidLedger,
    #[error("supervisor ledger persistence failed")]
    Persistence,
    #[error("supervisor fence does not match")]
    StaleFence,
    #[error("rehearsal deadline elapsed")]
    Deadline,
    #[error("broker protocol is malformed")]
    MalformedIpc,
    #[error("broker peer authentication failed")]
    PeerAuthentication,
    #[error("provider operation failed or is ambiguous")]
    Provider,
    #[error("cleanup proof is incomplete")]
    IncompleteProof,
}

impl SupervisorLedger {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fence: Fence,
        receipt_id_digest: [u8; 32],
        receipt_verifier: [u8; 32],
        worker_sha256: [u8; 32],
        policy_profile_digest: [u8; 32],
        absolute_deadline: UnixMillis,
        final_io_deadline: UnixMillis,
    ) -> Result<Self, SupervisionError> {
        if fence.run_id.len() < 16
            || fence.generation == 0
            || fence.epoch == 0
            || final_io_deadline >= absolute_deadline
        {
            return Err(SupervisionError::InvalidLedger);
        }
        let mut ledger = Self {
            schema_version: SCHEMA_VERSION,
            fence,
            receipt_id_digest,
            receipt_verifier,
            worker_sha256,
            policy_profile_digest,
            absolute_deadline,
            final_io_deadline,
            cleanup_only: false,
            quarantined: false,
            identity: None,
            transitions: Vec::new(),
        };
        ledger.push(Transition::Reserved)?;
        Ok(ledger)
    }

    pub fn push(&mut self, transition: Transition) -> Result<(), SupervisionError> {
        if self.quarantined && !matches!(transition, Transition::Quarantined { .. }) {
            return Err(SupervisionError::IncompleteProof);
        }
        let sequence = u64::try_from(self.transitions.len())
            .map_err(|_| SupervisionError::InvalidLedger)?
            .saturating_add(1);
        let previous_sha256 = self
            .transitions
            .last()
            .map_or([0; 32], |entry| entry.entry_sha256);
        if let Transition::IdentityBound { identity } = &transition {
            validate_identity(identity)?;
            if self
                .identity
                .as_ref()
                .is_some_and(|current| current != identity)
            {
                return Err(SupervisionError::InvalidLedger);
            }
            self.identity = Some(identity.clone());
        }
        if matches!(transition, Transition::CleanupOnly) {
            self.cleanup_only = true;
        }
        if matches!(transition, Transition::Quarantined { .. }) {
            self.cleanup_only = true;
            self.quarantined = true;
        }
        let bytes = serde_json::to_vec(&(sequence, previous_sha256, &transition))
            .map_err(|_| SupervisionError::InvalidLedger)?;
        self.transitions.push(LedgerEntry {
            sequence,
            previous_sha256,
            transition,
            entry_sha256: hash(b"spurfire-supervisor-entry-v1\0", &bytes),
        });
        Ok(())
    }

    pub fn validate(&self) -> Result<(), SupervisionError> {
        if self.schema_version != SCHEMA_VERSION || self.transitions.is_empty() {
            return Err(SupervisionError::InvalidLedger);
        }
        let mut previous = [0; 32];
        let mut derived_identity = None;
        let mut derived_cleanup_only = false;
        let mut derived_quarantined = false;
        for (index, entry) in self.transitions.iter().enumerate() {
            let sequence = u64::try_from(index + 1).map_err(|_| SupervisionError::InvalidLedger)?;
            let bytes = serde_json::to_vec(&(sequence, previous, &entry.transition))
                .map_err(|_| SupervisionError::InvalidLedger)?;
            if entry.sequence != sequence
                || entry.previous_sha256 != previous
                || entry.entry_sha256 != hash(b"spurfire-supervisor-entry-v1\0", &bytes)
                || (index == 0 && !matches!(entry.transition, Transition::Reserved))
                || (index != 0 && matches!(entry.transition, Transition::Reserved))
                || (derived_quarantined
                    && !matches!(entry.transition, Transition::Quarantined { .. }))
            {
                return Err(SupervisionError::InvalidLedger);
            }
            match &entry.transition {
                Transition::IdentityBound { identity } => {
                    validate_identity(identity)?;
                    if derived_identity
                        .as_ref()
                        .is_some_and(|current| current != identity)
                    {
                        return Err(SupervisionError::InvalidLedger);
                    }
                    derived_identity = Some(identity.clone());
                }
                Transition::CleanupOnly => derived_cleanup_only = true,
                Transition::Quarantined { .. } => {
                    derived_cleanup_only = true;
                    derived_quarantined = true;
                }
                _ => {}
            }
            previous = entry.entry_sha256;
        }
        if self.identity != derived_identity
            || self.cleanup_only != derived_cleanup_only
            || self.quarantined != derived_quarantined
        {
            return Err(SupervisionError::InvalidLedger);
        }
        Ok(())
    }

    /// A crash recovery claim is always cleanup-only; it can never recreate,
    /// mint credentials, apply policy, or extend either immutable deadline.
    pub fn claim_recovery(&mut self, fence: &Fence) -> Result<(), SupervisionError> {
        self.check_fence(fence)?;
        if self
            .transitions
            .iter()
            .any(|entry| matches!(entry.transition, Transition::Released))
        {
            return Err(SupervisionError::IncompleteProof);
        }
        self.push(Transition::CleanupOnly)
    }

    pub fn check_fence(&self, fence: &Fence) -> Result<(), SupervisionError> {
        if &self.fence != fence {
            return Err(SupervisionError::StaleFence);
        }
        Ok(())
    }

    pub fn permits(&self, operation: Operation, now: UnixMillis) -> Result<(), SupervisionError> {
        if self.quarantined {
            return Err(SupervisionError::IncompleteProof);
        }
        let cleanup_operation = matches!(
            operation,
            Operation::Delete
                | Operation::ObserveAbsence
                | Operation::VaultErase
                | Operation::LeaseRelease
        );
        if (!cleanup_operation && now >= self.absolute_deadline)
            || (matches!(
                operation,
                Operation::Create | Operation::ApplyPolicy | Operation::FinalProviderIo
            ) && now >= self.final_io_deadline)
        {
            return Err(SupervisionError::Deadline);
        }
        if self.cleanup_only
            && !matches!(
                operation,
                Operation::Delete
                    | Operation::ObserveAbsence
                    | Operation::VaultErase
                    | Operation::LeaseRelease
            )
        {
            return Err(SupervisionError::IncompleteProof);
        }
        Ok(())
    }
}

fn validate_identity(identity: &SupervisedIdentity) -> Result<(), SupervisionError> {
    if identity.provider_stable_id.is_empty()
        || identity.provider_stable_id.len() > 128
        || !identity
            .provider_stable_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || identity.fqdn.is_empty()
        || identity.fqdn.len() > 253
        || !identity
            .fqdn
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        || identity.vault_version == 0
    {
        return Err(SupervisionError::InvalidLedger);
    }
    Ok(())
}

/// Private, atomic, file-and-directory-fsynced ledger storage.
#[derive(Debug)]
pub struct LedgerStore {
    directory: PathBuf,
    ledger_path: PathBuf,
    head_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerHead {
    sequence: u64,
    entry_sha256: [u8; 32],
}

impl LedgerStore {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create(directory: impl AsRef<Path>) -> Result<Self, SupervisionError> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let directory = directory.as_ref();
        fs::create_dir(directory).map_err(|_| SupervisionError::Persistence)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| SupervisionError::Persistence)?;
        let metadata =
            fs::symlink_metadata(directory).map_err(|_| SupervisionError::Persistence)?;
        if metadata.file_type().is_symlink()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != rustix::process::getuid().as_raw()
        {
            return Err(SupervisionError::InvalidLedger);
        }
        let ledger_path = directory.join("supervisor-ledger.json");
        let head_path = directory.join("supervisor-ledger.head");
        for path in [&ledger_path, &head_path] {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            let file = options
                .open(path)
                .map_err(|_| SupervisionError::Persistence)?;
            file.sync_all().map_err(|_| SupervisionError::Persistence)?;
        }
        sync_directory(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            ledger_path,
            head_path,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn create(_directory: impl AsRef<Path>) -> Result<Self, SupervisionError> {
        Err(SupervisionError::Unsupported)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, SupervisionError> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory = directory.as_ref();
        let dir_meta =
            fs::symlink_metadata(directory).map_err(|_| SupervisionError::Persistence)?;
        let ledger_path = directory.join("supervisor-ledger.json");
        let head_path = directory.join("supervisor-ledger.head");
        let file_meta =
            fs::symlink_metadata(&ledger_path).map_err(|_| SupervisionError::Persistence)?;
        let head_meta =
            fs::symlink_metadata(&head_path).map_err(|_| SupervisionError::Persistence)?;
        let uid = rustix::process::getuid().as_raw();
        if dir_meta.file_type().is_symlink()
            || file_meta.file_type().is_symlink()
            || head_meta.file_type().is_symlink()
            || !file_meta.is_file()
            || !head_meta.is_file()
            || dir_meta.permissions().mode() & 0o077 != 0
            || file_meta.permissions().mode() & 0o177 != 0
            || head_meta.permissions().mode() & 0o177 != 0
            || dir_meta.uid() != uid
            || file_meta.uid() != uid
            || head_meta.uid() != uid
        {
            return Err(SupervisionError::InvalidLedger);
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            ledger_path,
            head_path,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn open(_directory: impl AsRef<Path>) -> Result<Self, SupervisionError> {
        Err(SupervisionError::Unsupported)
    }

    pub fn load(&self) -> Result<SupervisorLedger, SupervisionError> {
        let mut ledger: SupervisorLedger = read_json_limited(&self.ledger_path)?;
        ledger.validate()?;
        let head: LedgerHead = read_json_limited(&self.head_path)?;
        let actual = ledger
            .transitions
            .last()
            .ok_or(SupervisionError::InvalidLedger)?;
        if head.sequence != actual.sequence || head.entry_sha256 != actual.entry_sha256 {
            ledger.push(Transition::Quarantined {
                reason: "ledger_rollback_or_incomplete_commit".to_owned(),
            })?;
            self.persist(&ledger)?;
        }
        Ok(ledger)
    }

    pub fn persist(&self, ledger: &SupervisorLedger) -> Result<(), SupervisionError> {
        ledger.validate()?;
        let entry = ledger
            .transitions
            .last()
            .ok_or(SupervisionError::InvalidLedger)?;
        let head = LedgerHead {
            sequence: entry.sequence,
            entry_sha256: entry.entry_sha256,
        };
        if let Ok(bytes) = fs::read(&self.head_path) {
            if !bytes.is_empty() {
                let current: LedgerHead =
                    serde_json::from_slice(&bytes).map_err(|_| SupervisionError::InvalidLedger)?;
                let idempotent = head == current;
                let linked_append = entry.sequence == current.sequence.saturating_add(1)
                    && entry.previous_sha256 == current.entry_sha256;
                let fail_closed_recovery =
                    matches!(entry.transition, Transition::Quarantined { .. });
                if !idempotent && !linked_append && !fail_closed_recovery {
                    return Err(SupervisionError::InvalidLedger);
                }
            }
        }
        atomic_replace_json(&self.directory, &self.ledger_path, "ledger", ledger)?;
        // The independently replaced high-water mark makes restoring only an
        // earlier valid ledger snapshot detectable. Any crash between these two
        // commits also fails closed into durable quarantine during load.
        atomic_replace_json(&self.directory, &self.head_path, "head", &head)
    }
}

fn read_json_limited<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, SupervisionError> {
    let file = File::open(path).map_err(|_| SupervisionError::Persistence)?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|_| SupervisionError::Persistence)?;
    serde_json::from_slice(&bytes).map_err(|_| SupervisionError::InvalidLedger)
}

fn atomic_replace_json<T: Serialize>(
    directory: &Path,
    destination: &Path,
    label: &str,
    value: &T,
) -> Result<(), SupervisionError> {
    let bytes = serde_json::to_vec(value).map_err(|_| SupervisionError::InvalidLedger)?;
    let mut random = [0_u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| SupervisionError::Persistence)?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = directory.join(format!(".{label}-{suffix}.new"));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| SupervisionError::Persistence)?;
        file.write_all(&bytes)
            .map_err(|_| SupervisionError::Persistence)?;
        file.sync_all().map_err(|_| SupervisionError::Persistence)?;
        fs::rename(&temporary, destination).map_err(|_| SupervisionError::Persistence)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), SupervisionError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| SupervisionError::Persistence)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), SupervisionError> {
    Err(SupervisionError::Unsupported)
}

/// Narrow authenticated broker request. No secret fields exist in this wire type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerRequest {
    pub sequence: u64,
    pub challenge_sha256: [u8; 32],
    pub fence: Fence,
    pub operation: Operation,
    pub identity: Option<SupervisedIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedFrame {
    pub payload: BrokerRequest,
    pub authenticator: [u8; 32],
}

/// OS-authenticated identity captured from the connected local socket. The
/// launcher must compare this with the fixed child it spawned before decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCredentials {
    pub uid: u32,
    pub pid: u32,
}

pub fn authenticate_peer(
    actual: PeerCredentials,
    expected: PeerCredentials,
) -> Result<(), SupervisionError> {
    if actual != expected || actual.pid == 0 {
        return Err(SupervisionError::PeerAuthentication);
    }
    Ok(())
}

/// Reads the broker session key only from a caller-owned inherited/private
/// descriptor. There is intentionally no string, argv, environment, or Debug
/// constructor. The allocation zeroizes on drop.
pub struct DescriptorSecret(Zeroizing<[u8; 32]>);

impl DescriptorSecret {
    pub fn read_from(mut descriptor: impl Read) -> Result<Self, SupervisionError> {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        descriptor
            .read_exact(bytes.as_mut())
            .map_err(|_| SupervisionError::PeerAuthentication)?;
        let mut extra = [0_u8; 1];
        if descriptor
            .read(&mut extra)
            .map_err(|_| SupervisionError::PeerAuthentication)?
            != 0
        {
            bytes.zeroize();
            return Err(SupervisionError::PeerAuthentication);
        }
        Ok(Self(bytes))
    }

    fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for DescriptorSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DescriptorSecret(<redacted>)")
    }
}

pub fn encode_authenticated_frame_with_secret(
    request: BrokerRequest,
    descriptor_secret: &DescriptorSecret,
) -> Result<Vec<u8>, SupervisionError> {
    encode_authenticated_frame(request, descriptor_secret.expose())
}

pub fn encode_authenticated_frame(
    request: BrokerRequest,
    descriptor_secret: &[u8; 32],
) -> Result<Vec<u8>, SupervisionError> {
    let payload = serde_json::to_vec(&request).map_err(|_| SupervisionError::MalformedIpc)?;
    let authenticator = broker_authenticator(descriptor_secret, &payload);
    let body = serde_json::to_vec(&AuthenticatedFrame {
        payload: request,
        authenticator,
    })
    .map_err(|_| SupervisionError::MalformedIpc)?;
    if body.len() > MAX_IPC_FRAME {
        return Err(SupervisionError::MalformedIpc);
    }
    let length = u32::try_from(body.len()).map_err(|_| SupervisionError::MalformedIpc)?;
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn decode_authenticated_frame(
    frame: &[u8],
    descriptor_secret: &[u8; 32],
) -> Result<BrokerRequest, SupervisionError> {
    let length_bytes: [u8; 4] = frame
        .get(..4)
        .ok_or(SupervisionError::MalformedIpc)?
        .try_into()
        .map_err(|_| SupervisionError::MalformedIpc)?;
    let length = usize::try_from(u32::from_be_bytes(length_bytes))
        .map_err(|_| SupervisionError::MalformedIpc)?;
    if length > MAX_IPC_FRAME || frame.len() != length.saturating_add(4) {
        return Err(SupervisionError::MalformedIpc);
    }
    let authenticated: AuthenticatedFrame =
        serde_json::from_slice(&frame[4..]).map_err(|_| SupervisionError::MalformedIpc)?;
    let payload =
        serde_json::to_vec(&authenticated.payload).map_err(|_| SupervisionError::MalformedIpc)?;
    if authenticated.authenticator != broker_authenticator(descriptor_secret, &payload) {
        return Err(SupervisionError::PeerAuthentication);
    }
    Ok(authenticated.payload)
}

fn broker_authenticator(secret: &[u8; 32], payload: &[u8]) -> [u8; 32] {
    let inner = hash(
        b"spurfire-broker-auth-inner-v1\0",
        &[secret.as_slice(), payload].concat(),
    );
    hash(
        b"spurfire-broker-auth-outer-v1\0",
        &[secret.as_slice(), inner.as_slice()].concat(),
    )
}

fn hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(bytes);
    digest.finalize().into()
}

/// Broker-controlled observation result. Both booleans must be true; a cached
/// or partially paginated response can never establish absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsenceObservation {
    pub provider_stable_id: String,
    pub present: bool,
    pub fully_paginated: bool,
    pub uncached: bool,
}

/// Credential-owning boundary used by the deterministic fake harness and by a
/// future separately reviewed process adapter. Secrets cannot cross this trait.
/// Broker-local fence guard. A broker constructs this from the fsynced ledger,
/// not from worker IPC, and applies it to every requested mutation.
#[derive(Clone, Debug)]
pub struct BrokerFenceGuard {
    fence: Fence,
    policy_profile_digest: [u8; 32],
    challenge_sha256: [u8; 32],
    identity: Option<SupervisedIdentity>,
    absolute_deadline: UnixMillis,
    final_io_deadline: UnixMillis,
    cleanup_only: bool,
    quarantined: bool,
    last_sequence: u64,
}

impl BrokerFenceGuard {
    /// Builds broker-local replay state from the validated durable ledger and
    /// the private launcher challenge. The challenge is never accepted from IPC.
    pub fn from_ledger(ledger: &SupervisorLedger, challenge_sha256: [u8; 32]) -> Self {
        Self {
            fence: ledger.fence.clone(),
            policy_profile_digest: ledger.policy_profile_digest,
            challenge_sha256,
            identity: ledger.identity.clone(),
            absolute_deadline: ledger.absolute_deadline,
            final_io_deadline: ledger.final_io_deadline,
            cleanup_only: ledger.cleanup_only,
            quarantined: ledger.quarantined,
            last_sequence: 0,
        }
    }

    pub fn authorize(
        &mut self,
        request: &BrokerRequest,
        expected_policy_profile_digest: &[u8; 32],
        now: UnixMillis,
    ) -> Result<(), SupervisionError> {
        if self.quarantined {
            return Err(SupervisionError::IncompleteProof);
        }
        if request.fence != self.fence {
            return Err(SupervisionError::StaleFence);
        }
        if request.sequence != self.last_sequence.saturating_add(1)
            || request.challenge_sha256 != self.challenge_sha256
            || self.challenge_sha256 == [0; 32]
        {
            return Err(SupervisionError::PeerAuthentication);
        }
        if request.identity.as_ref() != self.identity.as_ref() || self.identity.is_none() {
            return Err(SupervisionError::IncompleteProof);
        }
        if expected_policy_profile_digest != &self.policy_profile_digest {
            return Err(SupervisionError::IncompleteProof);
        }
        let cleanup_operation = matches!(
            request.operation,
            Operation::Delete
                | Operation::ObserveAbsence
                | Operation::VaultErase
                | Operation::LeaseRelease
        );
        if (!cleanup_operation && now >= self.absolute_deadline)
            || (matches!(
                request.operation,
                Operation::Create | Operation::ApplyPolicy | Operation::FinalProviderIo
            ) && now >= self.final_io_deadline)
            || (self.cleanup_only && !cleanup_operation)
        {
            return Err(if now >= self.final_io_deadline && !cleanup_operation {
                SupervisionError::Deadline
            } else {
                SupervisionError::IncompleteProof
            });
        }
        self.last_sequence = request.sequence;
        Ok(())
    }
}

pub trait CredentialBroker {
    fn delete_exact(&mut self, fence: &Fence, identity: &SupervisedIdentity) -> OperationOutcome;
    fn observe_exact(
        &mut self,
        fence: &Fence,
        identity: &SupervisedIdentity,
    ) -> Result<AbsenceObservation, SupervisionError>;
    fn erase_vault_cas(
        &mut self,
        fence: &Fence,
        identity: &SupervisedIdentity,
    ) -> Result<u64, SupervisionError>;
    fn release_lease(&mut self, fence: &Fence) -> Result<(), SupervisionError>;
}

trait MonotonicClock {
    type Mark;

    fn mark(&self) -> Self::Mark;
    fn wait_until(&self, mark: &Self::Mark, minimum: Duration);
    fn elapsed(&self, mark: &Self::Mark) -> Duration;
}

struct SystemMonotonicClock;

impl MonotonicClock for SystemMonotonicClock {
    type Mark = Instant;

    fn mark(&self) -> Self::Mark {
        Instant::now()
    }

    fn wait_until(&self, mark: &Self::Mark, minimum: Duration) {
        if let Some(remaining) = minimum.checked_sub(mark.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    fn elapsed(&self, mark: &Self::Mark) -> Duration {
        mark.elapsed()
    }
}

/// Cleanup-only execution. The supervisor owns the monotonic interval between
/// the two completed provider observations; callers cannot assert elapsed time.
pub fn run_cleanup<B: CredentialBroker>(
    store: &LedgerStore,
    ledger: &mut SupervisorLedger,
    broker: &mut B,
    now: UnixMillis,
) -> Result<(), SupervisionError> {
    run_cleanup_with_clock(store, ledger, broker, now, &SystemMonotonicClock)
}

fn run_cleanup_with_clock<B: CredentialBroker, C: MonotonicClock>(
    store: &LedgerStore,
    ledger: &mut SupervisorLedger,
    broker: &mut B,
    now: UnixMillis,
    clock: &C,
) -> Result<(), SupervisionError> {
    let fence = ledger.fence.clone();
    ledger.permits(Operation::Delete, now)?;
    durable_transition(store, ledger, Transition::CleanupOnly)?;
    durable_transition(
        store,
        ledger,
        Transition::Intent {
            operation: Operation::Delete,
        },
    )?;
    let delete = broker.delete_exact(
        &fence,
        ledger
            .identity
            .as_ref()
            .ok_or(SupervisionError::IncompleteProof)?,
    );
    durable_transition(
        store,
        ledger,
        Transition::Outcome {
            operation: Operation::Delete,
            outcome: delete,
        },
    )?;
    if delete == OperationOutcome::Failed {
        return quarantine(store, ledger, "delete_failed");
    }

    let identity = ledger
        .identity
        .clone()
        .ok_or(SupervisionError::IncompleteProof)?;
    let mut first_observation_completed = None;
    for ordinal in 1..=2 {
        if ordinal == 2 {
            let mark = first_observation_completed
                .as_ref()
                .ok_or(SupervisionError::IncompleteProof)?;
            clock.wait_until(mark, MIN_ABSENCE_SEPARATION);
            if clock.elapsed(mark) < MIN_ABSENCE_SEPARATION {
                return quarantine(store, ledger, "absence_interval_too_short");
            }
        }
        durable_transition(
            store,
            ledger,
            Transition::Intent {
                operation: Operation::ObserveAbsence,
            },
        )?;
        let observation = match broker.observe_exact(&fence, &identity) {
            Ok(observation) => observation,
            Err(_) => {
                durable_transition(
                    store,
                    ledger,
                    Transition::Outcome {
                        operation: Operation::ObserveAbsence,
                        outcome: OperationOutcome::Unknown,
                    },
                )?;
                return quarantine(store, ledger, "absence_observation_unknown");
            }
        };
        durable_transition(
            store,
            ledger,
            Transition::Outcome {
                operation: Operation::ObserveAbsence,
                outcome: OperationOutcome::Succeeded,
            },
        )?;
        if observation.present
            || !observation.fully_paginated
            || !observation.uncached
            || observation.provider_stable_id != identity.provider_stable_id
        {
            return quarantine(store, ledger, "absence_not_proven");
        }
        durable_transition(
            store,
            ledger,
            Transition::AbsenceObserved {
                ordinal,
                provider_stable_id: observation.provider_stable_id,
                fully_paginated: true,
                uncached: true,
            },
        )?;
        if ordinal == 1 {
            first_observation_completed = Some(clock.mark());
        }
    }

    durable_transition(
        store,
        ledger,
        Transition::Intent {
            operation: Operation::VaultErase,
        },
    )?;
    let erased_version = match broker.erase_vault_cas(&fence, &identity) {
        Ok(version) => version,
        Err(_) => {
            durable_transition(
                store,
                ledger,
                Transition::Outcome {
                    operation: Operation::VaultErase,
                    outcome: OperationOutcome::Unknown,
                },
            )?;
            return quarantine(store, ledger, "vault_erasure_unknown");
        }
    };
    durable_transition(
        store,
        ledger,
        Transition::Outcome {
            operation: Operation::VaultErase,
            outcome: OperationOutcome::Succeeded,
        },
    )?;
    if erased_version != identity.vault_version {
        return quarantine(store, ledger, "vault_version_mismatch");
    }
    durable_transition(
        store,
        ledger,
        Transition::VaultTombstoneReadback {
            version: erased_version,
        },
    )?;
    durable_transition(
        store,
        ledger,
        Transition::Intent {
            operation: Operation::LeaseRelease,
        },
    )?;
    if broker.release_lease(&fence).is_err() {
        durable_transition(
            store,
            ledger,
            Transition::Outcome {
                operation: Operation::LeaseRelease,
                outcome: OperationOutcome::Unknown,
            },
        )?;
        return quarantine(store, ledger, "lease_release_unknown");
    }
    durable_transition(
        store,
        ledger,
        Transition::Outcome {
            operation: Operation::LeaseRelease,
            outcome: OperationOutcome::Succeeded,
        },
    )?;
    durable_transition(store, ledger, Transition::Released)
}

fn durable_transition(
    store: &LedgerStore,
    ledger: &mut SupervisorLedger,
    transition: Transition,
) -> Result<(), SupervisionError> {
    let before = ledger.clone();
    ledger.push(transition)?;
    if store.persist(ledger).is_err() {
        *ledger = before;
        return Err(SupervisionError::Persistence);
    }
    Ok(())
}

fn quarantine(
    store: &LedgerStore,
    ledger: &mut SupervisorLedger,
    reason: &str,
) -> Result<(), SupervisionError> {
    durable_transition(
        store,
        ledger,
        Transition::Quarantined {
            reason: reason.to_owned(),
        },
    )?;
    Err(SupervisionError::IncompleteProof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> SupervisorLedger {
        let lobby_id = LobbyId::parse("00000000-0000-4000-8000-000000000099").unwrap();
        let mut ledger = SupervisorLedger::new(
            Fence {
                run_id: "run-0123456789abcdef".into(),
                lobby_id,
                generation: 1,
                epoch: 7,
                token_sha256: [9; 32],
            },
            [1; 32],
            [2; 32],
            [3; 32],
            [4; 32],
            UnixMillis::new(20_000),
            UnixMillis::new(10_000),
        )
        .unwrap();
        ledger
            .push(Transition::IdentityBound {
                identity: SupervisedIdentity {
                    provider_stable_id: "TtStable99".into(),
                    fqdn: "tail99.ts.net".into(),
                    vault_version: 4,
                },
            })
            .unwrap();
        ledger
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn store(ledger: &SupervisorLedger) -> (tempfile::TempDir, LedgerStore) {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("private");
        let store = LedgerStore::create(&path).unwrap();
        store.persist(ledger).unwrap();
        (parent, store)
    }

    #[test]
    fn frame_rejects_malformed_oversized_truncated_and_wrong_descriptor_secret() {
        let request = BrokerRequest {
            sequence: 1,
            challenge_sha256: [7; 32],
            fence: fixture().fence,
            operation: Operation::Delete,
            identity: None,
        };
        let frame = encode_authenticated_frame(request.clone(), &[8; 32]).unwrap();
        assert_eq!(
            decode_authenticated_frame(&frame, &[8; 32]).unwrap(),
            request
        );
        assert!(decode_authenticated_frame(&frame[..frame.len() - 1], &[8; 32]).is_err());
        assert!(decode_authenticated_frame(&frame, &[6; 32]).is_err());
        let mut oversized = frame;
        oversized[..4].copy_from_slice(&((MAX_IPC_FRAME + 1) as u32).to_be_bytes());
        assert!(decode_authenticated_frame(&oversized, &[8; 32]).is_err());
        let secret = DescriptorSecret::read_from(std::io::Cursor::new([8; 32])).unwrap();
        assert_eq!(format!("{secret:?}"), "DescriptorSecret(<redacted>)");
        assert!(DescriptorSecret::read_from(std::io::Cursor::new([8; 33])).is_err());
        assert!(authenticate_peer(
            PeerCredentials { uid: 501, pid: 7 },
            PeerCredentials { uid: 501, pid: 7 }
        )
        .is_ok());
        assert!(authenticate_peer(
            PeerCredentials { uid: 502, pid: 7 },
            PeerCredentials { uid: 501, pid: 7 }
        )
        .is_err());
    }

    #[test]
    fn stale_fence_deadlines_policy_and_recovery_fail_closed() {
        let mut ledger = fixture();
        let mut stale = ledger.fence.clone();
        stale.epoch += 1;
        assert!(matches!(
            ledger.check_fence(&stale),
            Err(SupervisionError::StaleFence)
        ));
        assert!(matches!(
            ledger.permits(Operation::Create, UnixMillis::new(10_000)),
            Err(SupervisionError::Deadline)
        ));
        ledger.claim_recovery(&ledger.fence.clone()).unwrap();
        assert!(ledger
            .permits(Operation::Create, UnixMillis::new(1))
            .is_err());
        assert!(ledger
            .permits(Operation::Delete, UnixMillis::new(1))
            .is_ok());
        let mut guard = BrokerFenceGuard::from_ledger(&ledger, [7; 32]);
        let request = BrokerRequest {
            sequence: 1,
            challenge_sha256: [7; 32],
            fence: ledger.fence.clone(),
            operation: Operation::Create,
            identity: ledger.identity.clone(),
        };
        assert!(guard
            .authorize(&request, &[4; 32], UnixMillis::new(1))
            .is_err());
        let cleanup = BrokerRequest {
            operation: Operation::Delete,
            ..request
        };
        assert!(guard
            .authorize(&cleanup, &[5; 32], UnixMillis::new(1))
            .is_err());
        assert!(guard
            .authorize(&cleanup, &[4; 32], UnixMillis::new(1))
            .is_ok());
    }

    #[test]
    fn broker_guard_binds_sequence_challenge_identity_deadline_and_quarantine() {
        let ledger = fixture();
        let mut guard = BrokerFenceGuard::from_ledger(&ledger, [7; 32]);
        let request = BrokerRequest {
            sequence: 1,
            challenge_sha256: [7; 32],
            fence: ledger.fence.clone(),
            operation: Operation::Create,
            identity: ledger.identity.clone(),
        };
        assert!(guard
            .authorize(&request, &[4; 32], UnixMillis::new(1))
            .is_ok());
        assert!(guard
            .authorize(&request, &[4; 32], UnixMillis::new(1))
            .is_err());

        let mut wrong_challenge = request.clone();
        wrong_challenge.sequence = 2;
        wrong_challenge.challenge_sha256 = [8; 32];
        assert!(guard
            .authorize(&wrong_challenge, &[4; 32], UnixMillis::new(1))
            .is_err());
        let mut missing_identity = request.clone();
        missing_identity.sequence = 2;
        missing_identity.identity = None;
        assert!(guard
            .authorize(&missing_identity, &[4; 32], UnixMillis::new(1))
            .is_err());
        let mut expired = request.clone();
        expired.sequence = 2;
        assert!(matches!(
            guard.authorize(&expired, &[4; 32], UnixMillis::new(10_000)),
            Err(SupervisionError::Deadline)
        ));

        let mut quarantined = ledger;
        quarantined
            .push(Transition::Quarantined {
                reason: "test".into(),
            })
            .unwrap();
        let mut guard = BrokerFenceGuard::from_ledger(&quarantined, [7; 32]);
        assert!(guard
            .authorize(&request, &[4; 32], UnixMillis::new(1))
            .is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ledger_is_private_fsynced_hash_chained_and_rejects_partial_write() {
        use std::os::unix::fs::PermissionsExt;
        let ledger = fixture();
        let (_parent, store) = store(&ledger);
        assert_eq!(store.load().unwrap(), ledger);
        assert_eq!(
            fs::metadata(&store.directory).unwrap().permissions().mode() & 0o077,
            0
        );
        fs::write(&store.ledger_path, b"{").unwrap();
        assert!(matches!(store.load(), Err(SupervisionError::InvalidLedger)));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn restoring_prior_valid_ledger_snapshot_durably_quarantines() {
        let mut ledger = fixture();
        let (_parent, store) = store(&ledger);
        let prior = fs::read(&store.ledger_path).unwrap();
        ledger.push(Transition::CleanupOnly).unwrap();
        store.persist(&ledger).unwrap();

        fs::write(&store.ledger_path, prior).unwrap();
        let recovered = store.load().unwrap();
        assert!(recovered.cleanup_only);
        assert!(recovered.quarantined);
        assert!(recovered
            .permits(Operation::Create, UnixMillis::new(1))
            .is_err());
        assert!(store.load().unwrap().quarantined);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn symlink_ledger_is_rejected() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().unwrap();
        let private = parent.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(
            &private,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let target = parent.path().join("target");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, private.join("supervisor-ledger.json")).unwrap();
        assert!(LedgerStore::open(private).is_err());
    }

    struct FakeBroker {
        delete: OperationOutcome,
        observations: Vec<AbsenceObservation>,
        vault_result: Result<u64, SupervisionError>,
        release_calls: usize,
        calls: Vec<Operation>,
    }

    impl CredentialBroker for FakeBroker {
        fn delete_exact(
            &mut self,
            _fence: &Fence,
            _identity: &SupervisedIdentity,
        ) -> OperationOutcome {
            self.calls.push(Operation::Delete);
            self.delete
        }
        fn observe_exact(
            &mut self,
            _fence: &Fence,
            _identity: &SupervisedIdentity,
        ) -> Result<AbsenceObservation, SupervisionError> {
            self.calls.push(Operation::ObserveAbsence);
            if self.observations.is_empty() {
                return Err(SupervisionError::Provider);
            }
            Ok(self.observations.remove(0))
        }
        fn erase_vault_cas(
            &mut self,
            _fence: &Fence,
            _identity: &SupervisedIdentity,
        ) -> Result<u64, SupervisionError> {
            self.calls.push(Operation::VaultErase);
            self.vault_result
                .as_ref()
                .copied()
                .map_err(|_| SupervisionError::Provider)
        }
        fn release_lease(&mut self, _fence: &Fence) -> Result<(), SupervisionError> {
            self.calls.push(Operation::LeaseRelease);
            self.release_calls += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeClock {
        elapsed: std::cell::Cell<Duration>,
        advances_when_waited: bool,
    }

    impl FakeClock {
        fn advancing() -> Self {
            Self {
                elapsed: std::cell::Cell::new(Duration::ZERO),
                advances_when_waited: true,
            }
        }
    }

    impl MonotonicClock for FakeClock {
        type Mark = Duration;

        fn mark(&self) -> Self::Mark {
            self.elapsed.get()
        }

        fn wait_until(&self, mark: &Self::Mark, minimum: Duration) {
            if self.advances_when_waited {
                self.elapsed.set(*mark + minimum);
            }
        }

        fn elapsed(&self, mark: &Self::Mark) -> Duration {
            self.elapsed.get().saturating_sub(*mark)
        }
    }

    fn observation() -> AbsenceObservation {
        AbsenceObservation {
            provider_stable_id: "TtStable99".into(),
            present: false,
            fully_paginated: true,
            uncached: true,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_orders_delete_two_fresh_pages_vault_readback_then_release() {
        let mut ledger = fixture();
        let (_parent, store) = store(&ledger);
        let mut broker = FakeBroker {
            delete: OperationOutcome::Unknown,
            observations: vec![observation(), observation()],
            vault_result: Ok(4),
            release_calls: 0,
            calls: vec![],
        };
        run_cleanup_with_clock(
            &store,
            &mut ledger,
            &mut broker,
            UnixMillis::new(1),
            &FakeClock::advancing(),
        )
        .unwrap();
        assert_eq!(
            broker.calls,
            [
                Operation::Delete,
                Operation::ObserveAbsence,
                Operation::ObserveAbsence,
                Operation::VaultErase,
                Operation::LeaseRelease
            ]
        );
        assert_eq!(broker.release_calls, 1);
        assert!(matches!(
            ledger.transitions.last().unwrap().transition,
            Transition::Released
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn every_cleanup_fault_retains_lease_or_quarantines() {
        for fault in 0..5 {
            let mut ledger = fixture();
            let (_parent, store) = store(&ledger);
            let mut first = observation();
            let mut second = observation();
            let delete = if fault == 0 {
                OperationOutcome::Failed
            } else {
                OperationOutcome::Succeeded
            };
            if fault == 1 {
                first.present = true;
            }
            if fault == 2 {
                second.fully_paginated = false;
            }
            let vault_result = if fault == 3 { Ok(99) } else { Ok(4) };
            let clock = if fault == 4 {
                FakeClock::default()
            } else {
                FakeClock::advancing()
            };
            let mut broker = FakeBroker {
                delete,
                observations: vec![first, second],
                vault_result,
                release_calls: 0,
                calls: vec![],
            };
            assert!(run_cleanup_with_clock(
                &store,
                &mut ledger,
                &mut broker,
                UnixMillis::new(1),
                &clock
            )
            .is_err());
            assert_eq!(broker.release_calls, 0);
            assert!(ledger.quarantined);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_activation_creates_nothing() {
        let path = std::env::temp_dir().join("spurfire-supervision-must-not-exist");
        let _ = fs::remove_dir_all(&path);
        assert!(matches!(
            LedgerStore::create(&path),
            Err(SupervisionError::Unsupported)
        ));
        assert!(!path.exists());
    }
}
