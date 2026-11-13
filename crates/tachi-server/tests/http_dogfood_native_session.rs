#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn packaged_native_session_supervision_http_dogfood() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dogfood_e2e.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--tachi",
            env!("CARGO_BIN_EXE_tachi-server"),
            "--only",
            "native-session",
        ])
        .output()
        .expect("run native Session conformance over HTTP against the candidate");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == "47 passed, 0 failed"),
        "Native Session conformance did not complete every check: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr
    );
    println!("{stdout}");
}
