use std::path::Path;
use std::process::Command;

/// Runs named HTTP dogfood scenarios (comma-separated) against the packaged candidate. The
/// harness itself refuses silent skips (no bare returns, at least one check,
/// required checks abort with a recorded FAIL); success means exit 0 plus the
/// scenario's own completion line.
pub fn assert_dogfood_scenario(scenario: &str) {
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
    let completed = scenario.split(',').all(|name| {
        let marker = format!("SCENARIO {name} completed");
        stdout.lines().any(|line| line == marker)
    });
    assert!(
        output.status.success() && completed,
        "HTTP dogfood scenario {scenario} did not complete cleanly: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    println!("{stdout}");
}
