#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn packaged_claims_http_dogfood() {
    assert_http_dogfood("claims", "11 passed, 0 failed");
}

#[test]
fn packaged_eval_receipts_http_dogfood() {
    assert_http_dogfood("eval-receipts", "12 passed, 0 failed");
}

fn assert_http_dogfood(scenario: &str, expected_summary: &str) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            scenario,
        ])
        .output()
        .expect("run the HTTP dogfood scenario against the packaged candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == expected_summary),
        "HTTP dogfood did not complete every {scenario} check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}

#[test]
fn packaged_skill_surface_status_dogfood() {
    assert_http_dogfood("skill-surface", "7 passed, 0 failed");
}
