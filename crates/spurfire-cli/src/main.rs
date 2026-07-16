//! spurfire-ctl — lobby lifecycle CLI.
//!
//! Verbs (implement with clap derive):
//!   spurfire-ctl lobby create --name <n> [--players <n>] [--mode tailnet-per-lobby|shared]
//!   spurfire-ctl lobby list
//!   spurfire-ctl lobby status --name <n>
//!   spurfire-ctl lobby destroy --name <n>
//!   global: --json for machine-readable output
//!
//! Lobby metadata persists locally (e.g. ~/.local/share/spurfire/lobbies.json) so
//! `destroy` can always find what `create` made — deterministic cleanup.

fn main() {
    eprintln!("spurfire-ctl: not yet implemented — see module docs");
    std::process::exit(2);
}
