//! Fail-closed placeholder for the local rehearsal supervisor.
//!
//! The former implementation accepted caller-selected helper executables and
//! treated their exit status as cleanup proof. Until a pinned worker, durable
//! deadline/lease, and supervisor-verifiable cleanup receipt exist, activation
//! is deliberately unavailable. In particular, this binary never spawns a
//! child and therefore never delegates inherited descriptors or ambient secrets.

use clap::Parser;

const DISABLED_EXIT_CODE: i32 = 78;

#[derive(Debug, Parser)]
#[command(
    name = "spurfire-rehearsal-supervisor",
    about = "Fail-closed placeholder; real rehearsal activation is unavailable"
)]
struct Args {}

fn main() {
    let _ = Args::parse();
    eprintln!(
        "local rehearsal activation is disabled pending durable attestation and cleanup recovery"
    );
    std::process::exit(DISABLED_EXIT_CODE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_supervisor_has_no_helper_or_descriptor_arguments() {
        assert!(Args::try_parse_from(["supervisor"]).is_ok());
        for unsafe_argument in [
            "--service",
            "--recovery",
            "--custody-fd",
            "--receipt-fd",
            "--deadline-secs",
        ] {
            assert!(Args::try_parse_from(["supervisor", unsafe_argument, "/bin/true"]).is_err());
        }
        assert_ne!(DISABLED_EXIT_CODE, 0);
    }
}
