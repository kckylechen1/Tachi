//! #1454 F1/G2: server-owned run-receipt store.
//!
//! Merge authority comes from THIS store, never from the flow ledger. The
//! ledger (`<runs-root>/<flow_id>/verification.json`) can live inside a
//! repository (`.tachi/runs` fallback, flow_artifacts.rs), so anything in it
//! — including a forged `source: "server_run:<kind>"` string — is a DISPLAY
//! artifact only. Receipts live OUTSIDE any repository/worktree, under
//! `<tachi-home>/verify-receipts/<flow_id>/<kind>.json`, where `tachi-home`
//! is the server's global data root family: the same root the global memory
//! DB lives under, resolved once at server construction
//! (`MemoryServer::tachi_home_dir()` ← `path_utils::resolve_tachi_home()`,
//! init.rs). A caller manipulating a repo can never reach this directory by
//! the repo-relative default.
//!
//! The write path ([`write_run_receipt`]) is reachable ONLY from the run
//! executor (run.rs). The `#[cfg(test)]` helper ([`seed_run_receipt_for_test`])
//! is the prescribed test seam: discriminators that used to assert authority
//! from ledger JSON fixtures now seed this store instead.
//!
//! Receipt shape (all fields server-observed):
//! `flow_id`, `kind`, `head_sha` (the head the run evaluated — equals
//! `source_head`), `exit_code`, `log_path`, `duration_ms`, `ran_at`, `status`,
//! `reason` (tree_mutation_during_run / timed_out / failed), `timed_out`,
//! `kill_abandoned`, `tool_version` (G4, informational), plus the G2
//! detached-copy binding fields: `source_head` (observed claim HEAD),
//! `executed_in_detached_copy` (must be `true` for the gate to bind the
//! receipt), `copy_head_before`/`copy_head_after`, `copy_clean_before`/
//! `copy_clean_after` (the copy is checked out AT `source_head`, so the
//! before-side is `source_head` + clean by construction of `git worktree
//! add` from a commit; the after-side is observed post-run).

use super::storage::{read_json, write_json};
use super::*;
use crate::task_lifecycle::validate_flow_id;
use std::path::{Path, PathBuf};

fn kind_is_safe(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Root of the server-owned receipt store for a given Tachi home.
pub(super) fn verify_receipts_root(home: &Path) -> PathBuf {
    home.join("verify-receipts")
}

/// Per-flow per-kind receipt path. Flow ids are validated by the same rule
/// the ledger uses (`validate_flow_id`); kinds are validated against a safe
/// charset so neither can escape the store root.
pub(super) fn receipt_path_for(home: &Path, flow_id: &str, kind: &str) -> Result<PathBuf, String> {
    validate_flow_id(flow_id)?;
    if !kind_is_safe(kind) {
        return Err(format!("invalid verification receipt kind '{kind}'"));
    }
    Ok(verify_receipts_root(home)
        .join(flow_id)
        .join(format!("{kind}.json")))
}

/// Executor-only write path (#1454 F1): the ONLY production code that writes
/// a receipt. `kind` must match `receipt["kind"]`; the path is derived from
/// `kind` (validated), never from caller input.
pub(super) fn write_run_receipt(
    home: &Path,
    flow_id: &str,
    kind: &str,
    receipt: &Value,
) -> Result<(), String> {
    if receipt.get("kind").and_then(Value::as_str) != Some(kind) {
        return Err(format!(
            "receipt kind mismatch: path kind '{kind}' vs receipt kind {:?}",
            receipt.get("kind").and_then(Value::as_str)
        ));
    }
    let path = receipt_path_for(home, flow_id, kind)?;
    write_json(&path, receipt)
}

/// Read every receipt for a flow (all kinds), sorted by `ran_at` ascending.
pub(super) fn read_run_receipts(home: &Path, flow_id: &str) -> Result<Vec<Value>, String> {
    let dir = verify_receipts_root(home).join(flow_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut receipts = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(receipt) = read_json(&path)? {
            receipts.push(receipt);
        }
    }
    receipts.sort_by(|a, b| {
        let a_ran = a.get("ran_at").and_then(Value::as_str).unwrap_or("");
        let b_ran = b.get("ran_at").and_then(Value::as_str).unwrap_or("");
        a_ran.cmp(b_ran)
    });
    Ok(receipts)
}

/// Best server-known head for a flow: the `head_sha` of the most recent
/// receipt (`ran_at` max). Used by display consumers (status/board/
/// cycle_status/pr_handoff) when no GitHub head is available — the F6
/// "receipt-store head for the flow" branch. Never a caller field.
pub(crate) fn best_receipt_head(home: &Path, flow_id: &str) -> Option<String> {
    let receipts = read_run_receipts(home, flow_id).ok()?;
    receipts
        .iter()
        .max_by(|a, b| {
            let a_ran = a.get("ran_at").and_then(Value::as_str).unwrap_or("");
            let b_ran = b.get("ran_at").and_then(Value::as_str).unwrap_or("");
            a_ran.cmp(b_ran)
        })
        .and_then(|receipt| {
            receipt
                .get("head_sha")
                .and_then(Value::as_str)
                .filter(|sha| !sha.is_empty())
                .map(str::to_string)
        })
}

/// #1454 F1 test seam: seed a receipt into the server-owned store exactly as
/// the executor would write it. Discriminators that previously forged ledger
/// JSON fixtures to mint authority now prove the boundary by seeding THIS
/// store — and by showing the same JSON in the flow ledger is NOT accepted.
#[cfg(test)]
pub(crate) fn seed_run_receipt_for_test(
    home: &Path,
    flow_id: &str,
    kind: &str,
    receipt: &Value,
) -> Result<(), String> {
    write_run_receipt(home, flow_id, kind, receipt)
}
