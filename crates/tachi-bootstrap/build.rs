//! Build script — compile-time build provenance stamping (tachi-bootstrap twin).
//!
//! This is the twin of `crates/tachi-server/build.rs`. The `tachi-bootstrap`
//! crate renders the `--version` CLI output, so it needs the same `GIT_SHA`
//! stamp at compile time to print the sha alongside the version. Keep the two
//! scripts in sync.
//!
//! Hand-rolled (`std::process::Command`), NOT `vergen`, to avoid a new
//! dependency. A missing VCS (tarball build) MUST NOT fail the build — every
//! value falls back to the literal `"unknown"`.

fn main() {
    // ── GIT_SHA ──────────────────────────────────────────────────────────
    let git_sha = capture("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={git_sha}");

    // ── BUILD_TIME ───────────────────────────────────────────────────────
    let build_time =
        capture("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_TIME={build_time}");

    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}

/// Run `cmd` with `args`, return the trimmed stdout on success (exit 0 +
/// non-empty utf8), else `None`. Never panics.
fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(cmd).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
