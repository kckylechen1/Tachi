#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn packaged_projection_retirement_http_dogfood() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            "projection-retirement",
        ])
        .output()
        .expect("run the HTTP dogfood scenario against the packaged candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success()
            && stdout.lines().any(|line| line == "14 passed, 0 failed")
            && !stdout.lines().any(|line| line.starts_with("SKIP")),
        "HTTP dogfood did not complete every projection retirement check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}
