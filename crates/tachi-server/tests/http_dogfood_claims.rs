#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn packaged_claims_http_dogfood() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            "claims",
        ])
        .output()
        .expect("run the HTTP dogfood scenario against the packaged candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == "11 passed, 0 failed"),
        "HTTP dogfood did not complete every claims check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}

#[test]
fn packaged_health_cohort_http_dogfood() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            "health-cohort",
        ])
        .output()
        .expect("run the HTTP dogfood scenario against the packaged candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == "6 passed, 0 failed"),
        "HTTP dogfood did not complete every health-cohort check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}

#[test]
fn packaged_queue_evidence_http_dogfood() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            "queue-evidence",
        ])
        .output()
        .expect("run the HTTP dogfood scenario against the packaged candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == "5 passed, 0 failed"),
        "HTTP dogfood did not complete every queue-evidence check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}
