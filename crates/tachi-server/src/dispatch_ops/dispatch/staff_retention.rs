//! Release Staff write worktrees once their result is safe elsewhere.
//!
//! Write-capable Staff lanes keep their managed worktree and branch after the
//! worker exits so the result can be accepted. A retained tree is released
//! when its dispatch is terminal and its branch holds no commit that exists
//! only locally (everything is on a remote, including a merged base). Removal
//! goes through the certified worktree cleanup, which also refuses dirty
//! trees and live holders. Anything else is kept and reported; no retention
//! period applies.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::dispatch_runs_root;

#[derive(Debug, Default)]
pub(crate) struct RetainedStaffReport {
    pub(crate) released: Vec<Value>,
    pub(crate) kept: Vec<Value>,
}

impl RetainedStaffReport {
    pub(crate) fn to_json(&self) -> Value {
        json!({ "released": self.released, "kept": self.kept })
    }
}

/// Decide, and with `execute` perform, the release of each retained Staff
/// worktree among `leases` whose path lies under one of `roots` (all when
/// empty). `reclaim` retires one lease by env_id after its worktree is gone;
/// callers own the database handle.
pub(crate) fn release_retained_staff_worktrees(
    leases: Vec<memcore::ExecEnvLease>,
    roots: &[PathBuf],
    execute: bool,
    mut reclaim: impl FnMut(&str) -> Result<(), String>,
) -> RetainedStaffReport {
    let runs = dispatch_runs_root();
    let roots: Vec<PathBuf> = roots
        .iter()
        .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
        .collect();
    let mut report = RetainedStaffReport::default();
    for lease in leases {
        let Some(dispatch_id) = lease.dispatch_id.clone() else {
            continue;
        };
        if lease.state != memcore::ExecEnvState::Active || !Path::new(&lease.path).is_dir() {
            continue;
        }
        if !roots.is_empty()
            && !roots
                .iter()
                .any(|root| Path::new(&lease.path).starts_with(root))
        {
            continue;
        }
        let entry = |reason: String| {
            json!({
                "env_id": lease.env_id, "dispatch_id": dispatch_id,
                "path": lease.path, "branch": lease.branch, "reason": reason,
            })
        };
        if let Err(reason) = dispatch_is_terminal(&runs, &dispatch_id) {
            report.kept.push(entry(reason));
            continue;
        }
        match tachi_clean::wt_clean::dirty_entries_excluding_marker(Path::new(&lease.path)) {
            Ok(dirty) if dirty.is_empty() => {}
            Ok(_) => {
                report.kept.push(entry("uncommitted changes".into()));
                continue;
            }
            Err(error) => {
                report
                    .kept
                    .push(entry(format!("git probe failed: {error}")));
                continue;
            }
        }
        // Removal deletes the branch HEAD is on (or, when detached, a branch
        // from separate metadata). Release only a tree on its lease branch so
        // the branch probed below is the branch removal deletes.
        match git(
            Path::new(&lease.path),
            &["symbolic-ref", "-q", "--short", "HEAD"],
        ) {
            Ok(branch) if branch.trim() == lease.branch => {}
            _ => {
                report.kept.push(entry("not on the lease branch".into()));
                continue;
            }
        }
        match local_only_commits(Path::new(&lease.path)) {
            Ok(0) => {}
            Ok(count) => {
                report
                    .kept
                    .push(entry(format!("{count} commit(s) exist only locally")));
                continue;
            }
            Err(error) => {
                report
                    .kept
                    .push(entry(format!("git probe failed: {error}")));
                continue;
            }
        }
        if !execute {
            report.released.push(entry("would release".into()));
            continue;
        }
        match release(&lease, &mut reclaim) {
            Ok(()) => report.released.push(entry("released".into())),
            Err(error) => report.kept.push(entry(error)),
        }
    }
    report
}

/// Daemon entry: release every eligible retained Staff worktree.
pub(crate) fn release_retained_staff_worktrees_for_server(
    server: &crate::MemoryServer,
) -> Result<RetainedStaffReport, String> {
    let leases = server.with_global_store_read(|store| {
        memcore::list_exec_envs(store.connection(), Some(memcore::ExecEnvState::Active))
            .map_err(|error| error.to_string())
    })?;
    Ok(release_retained_staff_worktrees(
        leases,
        &[],
        true,
        |env_id| {
            server
                .reclaim_exec_env(
                    &memcore::ExecEnvSelector::EnvId(env_id.to_string()),
                    Some(RELEASE_REASON),
                )
                .map(|_| ())
        },
    ))
}

pub(crate) const RELEASE_REASON: &str = "staff_result_released";

fn dispatch_is_terminal(runs: &Path, dispatch_id: &str) -> Result<(), String> {
    let raw = std::fs::read(runs.join(dispatch_id).join("status.json"))
        .map_err(|_| "dispatch status unavailable".to_string())?;
    let status: Value =
        serde_json::from_slice(&raw).map_err(|_| "dispatch status unreadable".to_string())?;
    match status["state"].as_str() {
        Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED") => Ok(()),
        Some(state) => Err(format!("dispatch not terminal ({state})")),
        None => Err("dispatch status has no state".into()),
    }
}

/// Commits reachable from the worktree HEAD that no remote-tracking ref has.
fn local_only_commits(worktree: &Path) -> Result<usize, String> {
    let out = git(
        worktree,
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    )?;
    out.trim()
        .parse()
        .map_err(|_| format!("unexpected rev-list output: {out}"))
}

fn release(
    lease: &memcore::ExecEnvLease,
    reclaim: &mut impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    tachi_clean::wt_clean::run_wt_remove_keep_remote(tachi_clean::wt_clean::WtRemoveOptions {
        path: PathBuf::from(&lease.path),
        force: true,
        output: tachi_clean::wt_clean::OutputFormat::Json,
    })
    .map_err(|error| format!("worktree removal refused: {error}"))?;
    // The certified removal also deletes the local branch; the remote branch
    // holds the result and is kept.
    reclaim(&lease.env_id).map_err(|error| format!("lease reclaim failed: {error}"))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(all(test, unix))]
mod tests;
