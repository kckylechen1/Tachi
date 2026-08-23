use super::*;
use crate::gh_safe_merge::{CheckRun, MockGhClient};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn ready_pr() -> PrState {
    PrState {
        number: 42,
        state: PrLifecycleState::Open,
        mergeable: Mergeable::Mergeable,
        review_decision: Some(ReviewDecision::Approved),
        checks: ChecksState::Success,
        is_draft: false,
        head_sha: "deadbeef".to_string(),
        head_ref: Some("feat/source-branch".to_string()),
        linked_issue_refs: Vec::new(),
        closing_issue_labels: Vec::new(),
        head_consistent: None,
    }
}

fn already_merged_pr() -> PrState {
    PrState {
        state: PrLifecycleState::Merged,
        ..ready_pr()
    }
}

fn blocked_draft_pr() -> PrState {
    PrState {
        is_draft: true,
        ..ready_pr()
    }
}

fn pending_pr() -> PrState {
    PrState {
        mergeable: Mergeable::Unknown,
        ..ready_pr()
    }
}

fn skipped_checks_pr() -> PrState {
    PrState {
        checks: ChecksState::Skipped,
        ..ready_pr()
    }
}

/// #1454 F1: the gate reads the server-owned receipt store under
/// `path_utils::tachi_home()`. The safe-merge tests therefore pin BOTH the
/// run root (ledger display) and the Tachi home (receipt authority) to temp
/// dirs, then seed receipts through the store test helper — the ledger JSON
/// alone no longer mints authority.
fn write_verification(root: &std::path::Path, flow: &str, status: &str, head_sha: &str) {
    let run_dir = root.join(flow);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow,
            "head_sha": head_sha,
            "overall": status,
            "updated_at": "2026-06-08T00:00:00Z",
            "items": [
                {
                    "id": "gitleaks",
                    "kind": "gitleaks",
                    "status": status,
                    "head_sha": head_sha,
                    "required": true,
                    "source": "server_run:fmt",
                    "summary": "verification fixture"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Seed a receipt for one canonical kind into the receipt store under
/// `tachi_home` (the server-owned authority store, F1/G2/G4).
fn seed_receipt(
    tachi_home: &std::path::Path,
    flow: &str,
    kind: &str,
    status: &str,
    exit_code: i64,
    head_sha: &str,
    reason: Option<&str>,
) {
    let receipt = json!({
        "flow_id": flow,
        "kind": kind,
        "head_sha": head_sha,
        "status": status,
        "reason": reason,
        "exit_code": exit_code,
        "log_path": "/tmp/safe-merge-seed.log",
        "duration_ms": 1,
        "ran_at": "2026-08-18T00:00:00Z",
        "timed_out": false,
        "kill_abandoned": false,
        "source_head": head_sha,
        "executed_in_detached_copy": true,
        "copy_head_before": head_sha,
        "copy_head_after": head_sha,
        "copy_clean_before": true,
        "copy_clean_after": true,
        "tool_version": "seed-tool-1.0",
    });
    crate::verify_ops::seed_run_receipt_for_test(tachi_home, flow, kind, &receipt)
        .expect("seed receipt");
}

/// Seed a passed receipt for every canonical kind EXCEPT `skip`.
fn seed_full_passed_set(
    tachi_home: &std::path::Path,
    flow: &str,
    head_sha: &str,
    skip: Option<&str>,
) {
    for kind in crate::verify_ops::MERGE_REQUIRED_RUN_KINDS {
        if skip == Some(*kind) {
            continue;
        }
        seed_receipt(tachi_home, flow, kind, "passed", 0, head_sha, None);
    }
}

/// Pin TACHI_HOME + TACHI_RUN_ROOT to temp dirs for the duration of a
/// safe-merge verification test; restores both on drop. The global test lock
/// is held by the caller.
fn with_verify_env() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Option<std::ffi::OsString>,
    Option<std::ffi::OsString>,
) {
    let home = tempfile::tempdir().expect("temp tachi home");
    let runs = tempfile::tempdir().expect("temp run root");
    let original_home = std::env::var_os("TACHI_HOME");
    let original_runs = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_HOME", home.path());
    std::env::set_var("TACHI_RUN_ROOT", runs.path());
    (home, runs, original_home, original_runs)
}

fn restore_env(
    original_home: Option<std::ffi::OsString>,
    original_runs: Option<std::ffi::OsString>,
) {
    match original_home {
        Some(v) => std::env::set_var("TACHI_HOME", v),
        None => std::env::remove_var("TACHI_HOME"),
    }
    match original_runs {
        Some(v) => std::env::set_var("TACHI_RUN_ROOT", v),
        None => std::env::remove_var("TACHI_RUN_ROOT"),
    }
}

mod auth_env;
mod check_state_artifact;
mod flow_events;
mod merge_gate;
mod parsers;
mod reclamation;
mod review_digest;
mod strict_verification;
