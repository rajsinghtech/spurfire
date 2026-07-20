use std::process::Command;

#[test]
fn supervisor_never_executes_substituted_helpers_or_inherits_secret_canaries() {
    let binary = env!("CARGO_BIN_EXE_spurfire-rehearsal-supervisor");
    let output = Command::new(binary)
        .args([
            "--service",
            "/bin/true",
            "--recovery",
            "/bin/true",
            "--custody-fd",
            "3",
            "--receipt-fd",
            "4",
        ])
        .env("TS_CLIENT_SECRET", "ambient-secret-canary")
        .output()
        .expect("supervisor must execute");

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ambient-secret-canary"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("ambient-secret-canary"));
}

#[test]
fn disabled_supervisor_cannot_report_clean() {
    let output = Command::new(env!("CARGO_BIN_EXE_spurfire-rehearsal-supervisor"))
        .output()
        .expect("supervisor must execute");
    assert_eq!(output.status.code(), Some(78));
}
