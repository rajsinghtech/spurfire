//! Credential-blind bounded owner for one local rehearsal service child.
//!
//! Provider credentials remain on an inherited private descriptor opened by an
//! owner-only wrapper. This process never reads, copies, logs, or serializes it.

use std::{path::PathBuf, process::Stdio, time::Duration};

use clap::Parser;
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use zeroize::Zeroizing;

const MIN_DEADLINE_SECS: u64 = 30;
const MAX_DEADLINE_SECS: u64 = 15 * 60;
const CLEANUP_RESERVE_SECS: u64 = 20;

#[derive(Debug, Parser)]
#[command(
    name = "spurfire-rehearsal-supervisor",
    about = "Bounded credential-blind owner for one private rehearsal child"
)]
struct Args {
    /// Attested local rehearsal service executable.
    #[arg(long)]
    service: PathBuf,
    /// Recovery-only executable; it owns exact generation-bound cleanup.
    #[arg(long)]
    recovery: PathBuf,
    /// Hard run deadline, including cleanup reserve.
    #[arg(long)]
    deadline_secs: u64,
    /// Inherited private descriptor containing owner/runtime custody.
    #[arg(long)]
    custody_fd: u32,
    /// Inherited private descriptor used for challenge/receipt exchange.
    #[arg(long)]
    receipt_fd: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitDisposition {
    Clean,
    Quarantined,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    validate_args(&args)?;
    let run_nonce = run_nonce().map_err(|_| "failed to create supervisor nonce")?;
    let remediation = remediation_hash(&run_nonce);

    let run_budget = Duration::from_secs(args.deadline_secs - CLEANUP_RESERVE_SECS);
    let cleanup_budget = Duration::from_secs(CLEANUP_RESERVE_SECS);
    let mut service = spawn_owned(&args.service, &args, false)?;
    let service_result = tokio::time::timeout(run_budget, service.wait()).await;
    let service_clean = matches!(service_result, Ok(Ok(status)) if status.success());
    if !service_clean {
        terminate_bounded(&mut service).await;
    }

    // Recovery receives no caller-selected provider identity. It must load the
    // exact held lobby/generation/stable identity from the fenced durable state,
    // retain/quarantine on every ambiguity, and release only after its internal
    // two-observation + vault-CAS proof succeeds.
    let mut recovery = spawn_owned(&args.recovery, &args, true)?;
    let recovered = matches!(
        tokio::time::timeout(cleanup_budget, recovery.wait()).await,
        Ok(Ok(status)) if status.success()
    );
    if !recovered {
        terminate_bounded(&mut recovery).await;
        emit_quarantine(&remediation);
        std::process::exit(75);
    }

    match if service_clean {
        ExitDisposition::Clean
    } else {
        // A service crash is acceptable only after recovery proved and released
        // the exact lease. No secret or provider identity is emitted.
        ExitDisposition::Quarantined
    } {
        ExitDisposition::Clean => Ok(()),
        ExitDisposition::Quarantined => {
            eprintln!("rehearsal recovered after child failure; reference={remediation}");
            Ok(())
        }
    }
}

fn validate_args(args: &Args) -> Result<(), &'static str> {
    if !(MIN_DEADLINE_SECS..=MAX_DEADLINE_SECS).contains(&args.deadline_secs)
        || args.deadline_secs <= CLEANUP_RESERVE_SECS
        || args.custody_fd <= 2
        || args.receipt_fd <= 2
        || args.custody_fd == args.receipt_fd
        || !args.service.is_absolute()
        || !args.recovery.is_absolute()
    {
        return Err("invalid fail-closed supervisor configuration");
    }
    Ok(())
}

fn spawn_owned(path: &PathBuf, args: &Args, recovery_only: bool) -> std::io::Result<Child> {
    let mut command = Command::new(path);
    command
        .arg(if recovery_only {
            "--recovery-only"
        } else {
            "--supervised"
        })
        .arg("--custody-fd")
        .arg(args.custody_fd.to_string())
        .arg("--receipt-fd")
        .arg(args.receipt_fd.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.spawn()
}

async fn terminate_bounded(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

fn run_nonce() -> Result<Zeroizing<[u8; 32]>, getrandom::Error> {
    let mut nonce = Zeroizing::new([0_u8; 32]);
    getrandom::getrandom(nonce.as_mut())?;
    Ok(nonce)
}

fn remediation_hash(nonce: &[u8; 32]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"spurfire-rehearsal-remediation-v1\0");
    digest.update(nonce);
    let bytes: [u8; 32] = digest.finalize().into();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn emit_quarantine(reference: &str) {
    eprintln!("rehearsal cleanup ambiguous; lease retained; reference={reference}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_deadlines_descriptors_and_relative_programs() {
        let base = Args {
            service: PathBuf::from("/bin/true"),
            recovery: PathBuf::from("/bin/true"),
            deadline_secs: 60,
            custody_fd: 3,
            receipt_fd: 4,
        };
        assert!(validate_args(&base).is_ok());
        for invalid in [
            Args {
                deadline_secs: 1,
                ..clone_args(&base)
            },
            Args {
                custody_fd: 2,
                ..clone_args(&base)
            },
            Args {
                receipt_fd: 3,
                ..clone_args(&base)
            },
            Args {
                service: PathBuf::from("relative"),
                ..clone_args(&base)
            },
        ] {
            assert!(validate_args(&invalid).is_err());
        }
    }

    #[test]
    fn remediation_reference_is_domain_separated_and_redacted() {
        let reference = remediation_hash(&[9; 32]);
        assert_eq!(reference.len(), 64);
        assert!(reference.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    fn clone_args(args: &Args) -> Args {
        Args {
            service: args.service.clone(),
            recovery: args.recovery.clone(),
            deadline_secs: args.deadline_secs,
            custody_fd: args.custody_fd,
            receipt_fd: args.receipt_fd,
        }
    }
}
