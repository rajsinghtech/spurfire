//! Default-inert launcher boundary for external rehearsal supervision.
//!
//! The durable fenced state machine, authenticated broker protocol, deadline,
//! and crash-safe cleanup proof live in `spurfire_server::supervision`. This
//! checked-in launcher intentionally exposes no activation arguments and never
//! acquires ambient credentials. Activation requires a separately protected,
//! fixed-artifact launcher integration; Windows remains unsupported.

use clap::Parser;

const DISABLED_EXIT_CODE: i32 = 78;

#[derive(Debug, Parser)]
#[command(
    name = "spurfire-rehearsal-supervisor",
    about = "Default-inert rehearsal supervision boundary"
)]
struct Args {}

fn main() {
    let _ = Args::parse();
    eprintln!("local rehearsal activation is inert; no protected launcher authority was supplied");
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
