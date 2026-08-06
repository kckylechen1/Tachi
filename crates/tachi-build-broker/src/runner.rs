//! The executor seat's hands: checkout, run, wipe (#894 S2c item 3).
//!
//! Behind a trait for two reasons. One, the broker's *decisions* (serialize,
//! pick a target, quarantine on interrupt) are the part worth testing, and they
//! must be testable without a Rust toolchain in the loop. Two, the real
//! implementation is three subprocesses and an `rm -rf`, which is exactly the
//! kind of code that should be a leaf, not a dependency of the policy.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ticket::{BuildTicket, SourceIdentity};

/// How a build ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildOutcome {
    /// Exit 0.
    Success,
    /// Non-zero exit — a real compile/test failure. The target dir is in a
    /// well-defined state (cargo finished writing) and keeps its generation.
    Failed,
    /// Killed by a signal / never reached an exit status. The target dir was
    /// being written to when the process died: its contents are NOT trustworthy
    /// and it gets quarantined (#894 S2c item 4).
    Interrupted,
}

/// What one build run produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRun {
    pub outcome: BuildOutcome,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// The seat's hands.
pub trait BuildRunner {
    /// Put the seat's fixed checkout on the ticket's source commit.
    ///
    /// MUST refuse a dirty checkout: the seat's tree is a shared machine
    /// resource, and checking out over someone's uncommitted work is the
    /// clobber this whole design exists to avoid.
    fn prepare_checkout(&self, checkout: &Path, source: &SourceIdentity) -> Result<(), String>;

    /// Run the ticket's command in `checkout` with `CARGO_TARGET_DIR=target_dir`.
    fn run(
        &self,
        ticket: &BuildTicket,
        checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String>;

    /// Wipe a target dir. Returns the bytes freed. Must be idempotent (wiping an
    /// already-empty/absent dir frees 0 and is not an error).
    fn clear_target(&self, target_dir: &Path) -> Result<i64, String>;
}

/// The real seat: git + a subprocess + `remove_dir_all`.
pub struct ProcessBuildRunner;

impl BuildRunner for ProcessBuildRunner {
    fn prepare_checkout(&self, checkout: &Path, source: &SourceIdentity) -> Result<(), String> {
        let dirty = git_stdout(checkout, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(format!(
                "executor seat checkout '{}' is dirty; refusing to check out {} over it — the \
                 seat holds ONE fixed clean checkout and never clobbers work in it (#894 S2c)",
                checkout.display(),
                source.head_sha
            ));
        }
        // `--detach <sha>`: the seat never sits on a branch, so it can never
        // "just pull" or drift. The sha was validated as a hex object id by
        // `BuildTicket::new`, so it cannot be a flag or a ref name.
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["checkout", "--detach", &source.head_sha])
            .status()
            .map_err(|e| format!("git checkout: {e}"))?;
        if !status.success() {
            return Err(format!(
                "git checkout --detach {} in '{}' failed ({status})",
                source.head_sha,
                checkout.display()
            ));
        }
        // Verify we landed where the ticket said. A silent mismatch here would
        // build the wrong tree and attribute it to this ticket.
        let head = git_stdout(checkout, &["rev-parse", "HEAD"])?;
        let head = head.trim();
        if !head.starts_with(&source.head_sha) && !source.head_sha.starts_with(head) {
            return Err(format!(
                "executor seat HEAD is {head} after checking out {}: refusing to build a tree \
                 that is not the ticket's source (#894 S2c)",
                source.head_sha
            ));
        }
        Ok(())
    }

    fn run(
        &self,
        ticket: &BuildTicket,
        checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String> {
        std::fs::create_dir_all(target_dir)
            .map_err(|e| format!("create target dir {}: {e}", target_dir.display()))?;
        let output = std::process::Command::new(&ticket.command.program)
            .args(&ticket.command.args)
            .current_dir(checkout)
            .env("CARGO_TARGET_DIR", target_dir)
            .output()
            .map_err(|e| format!("spawn {}: {e}", ticket.command.program))?;

        let outcome = match output.status.code() {
            Some(0) => BuildOutcome::Success,
            Some(_) => BuildOutcome::Failed,
            // No exit code = killed by a signal. The compiler died with its
            // hands in the target dir.
            None => BuildOutcome::Interrupted,
        };
        Ok(BuildRun {
            outcome,
            exit_code: output.status.code(),
            stdout_tail: tail(&String::from_utf8_lossy(&output.stdout)),
            stderr_tail: tail(&String::from_utf8_lossy(&output.stderr)),
        })
    }

    fn clear_target(&self, target_dir: &Path) -> Result<i64, String> {
        if !target_dir.exists() {
            return Ok(0);
        }
        let bytes = dir_size(target_dir);
        std::fs::remove_dir_all(target_dir)
            .map_err(|e| format!("clear target dir {}: {e}", target_dir.display()))?;
        std::fs::create_dir_all(target_dir)
            .map_err(|e| format!("recreate target dir {}: {e}", target_dir.display()))?;
        Ok(bytes)
    }
}

fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} in '{}' failed: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Last 4 KiB — enough to see the error, small enough to keep in a KV row.
fn tail(raw: &str) -> String {
    const MAX: usize = 4096;
    if raw.len() <= MAX {
        return raw.to_string();
    }
    let start = raw.len() - MAX;
    // Do not slice mid-codepoint.
    let start = (start..raw.len())
        .find(|i| raw.is_char_boundary(*i))
        .unwrap_or(raw.len());
    raw[start..].to_string()
}

fn dir_size(path: &Path) -> i64 {
    let mut total: i64 = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(meta.len() as i64);
        }
    }
    total
}
