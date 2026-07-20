//! Protected Unix execution-plane boundary.
//!
//! This module is deliberately not configured by environment variables. It
//! resolves fixed sibling artifacts, verifies them through already-open
//! no-follow descriptors, clears ambient environment/FDs, and gives provider
//! custody only to the broker role. The ordinary server does not import it.

use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    process::{Child, Command, Stdio},
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectedRole {
    Worker,
    Broker,
}

impl ProtectedRole {
    const fn file_name(self) -> &'static str {
        match self {
            Self::Worker => "spurfire-alpha-worker",
            Self::Broker => "spurfire-provider-broker",
        }
    }
}

#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("protected execution is unsupported on this platform")]
    Unsupported,
    #[error("protected artifact or custody path is unsafe")]
    UnsafePath,
    #[error("protected artifact digest does not match receipt")]
    DigestMismatch,
    #[error("protected child launch failed")]
    Spawn,
    #[error("worker inherited forbidden provider custody")]
    AmbientCredential,
}

/// Independently reject ambient provider/vault custody at worker startup.
pub fn reject_worker_credential_environment() -> Result<(), LauncherError> {
    validate_worker_environment(std::env::vars_os().map(|(name, _)| name))
}

fn validate_worker_environment<I, S>(names: I) -> Result<(), LauncherError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    if names.into_iter().any(|name| {
        let name = name.as_ref().to_string_lossy();
        matches!(
            name.as_ref(),
            "TS_CLIENT_ID" | "TS_CLIENT_SECRET" | "TS_AUTHKEY"
        ) || name.contains("VAULT_KEY")
            || name.contains("OAUTH")
    }) {
        Err(LauncherError::AmbientCredential)
    } else {
        Ok(())
    }
}

/// Verified open artifact. The path cannot be substituted after verification;
/// Linux execution uses `/proc/self/fd/<fd>` for the same open inode.
pub struct VerifiedArtifact {
    file: File,
    role: ProtectedRole,
}

impl std::fmt::Debug for VerifiedArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedArtifact")
            .field("role", &self.role)
            .field("path", &"<fixed-sibling>")
            .finish()
    }
}

#[cfg(unix)]
pub fn open_fixed_sibling(
    role: ProtectedRole,
    expected_sha256: [u8; 32],
) -> Result<VerifiedArtifact, LauncherError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let current = std::env::current_exe().map_err(|_| LauncherError::UnsafePath)?;
    let parent = current.parent().ok_or(LauncherError::UnsafePath)?;
    let parent_meta = std::fs::symlink_metadata(parent).map_err(|_| LauncherError::UnsafePath)?;
    let uid = rustix::process::getuid().as_raw();
    if parent_meta.file_type().is_symlink()
        || parent_meta.uid() != uid
        || parent_meta.permissions().mode() & 0o022 != 0
    {
        return Err(LauncherError::UnsafePath);
    }
    let path = parent.join(role.file_name());
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| LauncherError::UnsafePath)?;
    let meta = file.metadata().map_err(|_| LauncherError::UnsafePath)?;
    if !meta.is_file() || meta.uid() != uid || meta.permissions().mode() & 0o022 != 0 {
        return Err(LauncherError::UnsafePath);
    }
    let mut reader = &file;
    let mut hasher = Sha256::new();
    io::copy(&mut reader, &mut hasher).map_err(|_| LauncherError::UnsafePath)?;
    if <[u8; 32]>::from(hasher.finalize()) != expected_sha256 {
        return Err(LauncherError::DigestMismatch);
    }
    Ok(VerifiedArtifact { file, role })
}

#[cfg(not(unix))]
pub fn open_fixed_sibling(
    _role: ProtectedRole,
    _expected_sha256: [u8; 32],
) -> Result<VerifiedArtifact, LauncherError> {
    Err(LauncherError::Unsupported)
}

/// Opens owner-only broker custody without following the final component.
/// The returned descriptor, never its path or contents, is passed to the broker.
#[cfg(unix)]
pub fn open_broker_custody(path: &Path) -> Result<File, LauncherError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let parent = path.parent().ok_or(LauncherError::UnsafePath)?;
    let uid = rustix::process::getuid().as_raw();
    let parent_meta = std::fs::symlink_metadata(parent).map_err(|_| LauncherError::UnsafePath)?;
    if parent_meta.file_type().is_symlink()
        || parent_meta.uid() != uid
        || parent_meta.permissions().mode() & 0o077 != 0
    {
        return Err(LauncherError::UnsafePath);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| LauncherError::UnsafePath)?;
    let meta = file.metadata().map_err(|_| LauncherError::UnsafePath)?;
    if !meta.is_file() || meta.uid() != uid || meta.permissions().mode() & 0o777 != 0o400 {
        return Err(LauncherError::UnsafePath);
    }
    Ok(file)
}

#[cfg(not(unix))]
pub fn open_broker_custody(_path: &Path) -> Result<File, LauncherError> {
    Err(LauncherError::Unsupported)
}

/// Unlink custody only after the authenticated broker confirms it holds the
/// same already-open inode. The descriptor remains readable by that broker.
#[cfg(unix)]
pub fn unlink_confirmed_custody(path: &Path, open_file: &File) -> Result<(), LauncherError> {
    use std::os::unix::fs::MetadataExt;
    let path_meta = std::fs::symlink_metadata(path).map_err(|_| LauncherError::UnsafePath)?;
    let open_meta = open_file
        .metadata()
        .map_err(|_| LauncherError::UnsafePath)?;
    if path_meta.dev() != open_meta.dev() || path_meta.ino() != open_meta.ino() {
        return Err(LauncherError::UnsafePath);
    }
    std::fs::remove_file(path).map_err(|_| LauncherError::UnsafePath)?;
    File::open(path.parent().ok_or(LauncherError::UnsafePath)?)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| LauncherError::UnsafePath)
}

#[cfg(not(unix))]
pub fn unlink_confirmed_custody(_path: &Path, _open_file: &File) -> Result<(), LauncherError> {
    Err(LauncherError::Unsupported)
}

/// Spawn a fixed verified helper with an empty environment and a closed FD
/// universe. `descriptors` are duplicated to exact target numbers; the worker
/// caller must not include credential or vault custody descriptors.
#[cfg(target_os = "linux")]
pub fn spawn_protected(
    artifact: &VerifiedArtifact,
    descriptors: Vec<(File, i32)>,
) -> Result<Child, LauncherError> {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let executable = format!("/proc/self/fd/{}", artifact.file.as_raw_fd());
    let mappings: Vec<(i32, i32)> = descriptors
        .iter()
        .map(|(file, target)| (file.as_raw_fd(), *target))
        .collect();
    let retained: Vec<i32> = mappings.iter().map(|(_, target)| *target).collect();
    let mut command = Command::new(executable);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe syscalls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            for (source, target) in &mappings {
                if libc::dup2(*source, *target) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            for fd in 3..1024 {
                if !retained.contains(&fd) {
                    libc::close(fd);
                }
            }
            Ok(())
        });
    }
    command.spawn().map_err(|_| LauncherError::Spawn)
}

#[cfg(not(target_os = "linux"))]
pub fn spawn_protected(
    _artifact: &VerifiedArtifact,
    _descriptors: Vec<(File, i32)>,
) -> Result<Child, LauncherError> {
    Err(LauncherError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roles_are_fixed_and_not_caller_selected() {
        assert_eq!(ProtectedRole::Worker.file_name(), "spurfire-alpha-worker");
        assert_eq!(
            ProtectedRole::Broker.file_name(),
            "spurfire-provider-broker"
        );
    }

    #[test]
    fn worker_rejects_ambient_provider_and_vault_names() {
        assert!(validate_worker_environment(["PATH", "HOME"]).is_ok());
        for name in [
            "TS_CLIENT_ID",
            "TS_CLIENT_SECRET",
            "SPURFIRE_VAULT_KEY",
            "CHILD_OAUTH_FILE",
        ] {
            assert!(matches!(
                validate_worker_environment([name]),
                Err(LauncherError::AmbientCredential)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn custody_rejects_symlink_and_unsafe_modes_and_unlinks_same_inode() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let credential = root.path().join("credential");
        std::fs::write(&credential, b"not-a-real-secret").unwrap();
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(open_broker_custody(&credential).is_err());
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o400)).unwrap();
        let file = open_broker_custody(&credential).unwrap();
        let linked = root.path().join("linked");
        symlink(&credential, &linked).unwrap();
        assert!(open_broker_custody(&linked).is_err());
        unlink_confirmed_custody(&credential, &file).unwrap();
        assert!(!credential.exists());
    }
}
