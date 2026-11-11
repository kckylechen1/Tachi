#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn packaged_staff_submission_recovery_http_dogfood() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            "submission",
        ])
        .output()
        .expect("run canonical Staff recovery over HTTP against the candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == "13 passed, 0 failed"),
        "Staff submission recovery did not complete every check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}
