use super::*;

/// Terminal-outcome `error_class` written for a dispatch this function
/// closes out with no live process behind it (daemon restarted mid-run).
const RECOVERED_ORPHAN_ERROR_CLASS: &str = "recovered_orphan";

pub(super) fn dispatch_status_needs_recovery(status: &serde_json::Value) -> bool {
    // `completion_recovery` is a durable handoff to a later tachi_complete
    // replay, not evidence that a dead dispatch should be converted to an
    // orphan failure. Its canonical outcome is intentionally still pending.
    if status.get("completion_recovery").is_some() {
        return false;
    }
    if status.get("exit_code").is_some() {
        return false;
    }
    match status.get("state").and_then(serde_json::Value::as_str) {
        Some("TASK_STATE_WORKING" | "TASK_STATE_PENDING" | "TASK_STATE_RUNNING") => true,
        Some(_) => false,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_completion_recovery_is_not_an_orphan_failure() {
        assert!(
            !dispatch_status_needs_recovery(&json!({
                "state": "TASK_STATE_WORKING",
                "completion_recovery": {"status": "pending_canonical_outcome"},
            })),
            "a canonical-outcome recovery marker must remain available for replay, not be failed as an orphan"
        );
    }
}

/// Mark orphaned in-flight dispatch runs as failed after daemon restart.
///
/// This is a terminal path in its own right (#774 round 2): the run never
/// reaches `tachi_complete` (there is no live process left to call it) and
/// previously the daemon-restart recovery only rewrote `status.json`,
/// leaving the canonical `dispatch_outcomes` ledger silent about it — the
/// same "terminal state with no outcome row" hole `record_terminal_failure_outcome`
/// closes for the backend/preflight/watchdog/early-exit paths. Each
/// recovered run now also gets a canonical outcome row via that same writer
/// (`error_class = "recovered_orphan"`, `reported_outcome` NULL — there was
/// no self-report to preserve).
///
/// `project` (#774 round 3): recovery has no live `TachiDispatchParams` to
/// read from — the dispatching process is gone — but `dispatch.rs`'s
/// receipt-first `status.json` seed now stamps `TachiDispatchParams::project`
/// into the on-disk blob at dispatch time (same field name, `"project"`),
/// specifically so a crash-recovered run can still resolve its terminal
/// outcome to the same named-project store a live `tachi_complete` for it
/// would have used. This function reads that field straight off the
/// pre-overwrite status blob below (same place `agent` is read) and threads
/// it through. Older run directories written before this field existed have
/// no `"project"` key, so `status.get("project")` is `None` for them and
/// they fall back to `resolve_write_scope("")` exactly as before — pure
/// backward compatibility, not a behavior change for already-written blobs.
pub(crate) fn recover_orphaned_dispatch_runs(server: &MemoryServer) -> Vec<String> {
    let root = dispatch_runs_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut recovered = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let run_dir = entry.path();
        let dispatch_id = match run_dir.file_name().and_then(|name| name.to_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        if dispatch_status_is_terminal(&dispatch_id) {
            continue;
        }
        let status_path = run_dir.join("status.json");
        let Ok(Some(status)) = crate::task_lifecycle::read_json_file(&status_path) else {
            continue;
        };
        if !dispatch_status_needs_recovery(&status) {
            continue;
        }
        let previous_state = status
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let agent = status
            .get("agent")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let project = status
            .get("project")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        append_trajectory_event(
            &run_dir.join("trajectory.jsonl"),
            json!({
                "event": "dispatch_recovered",
                "dispatch_id": dispatch_id,
                "previous_state": previous_state,
                "reason": "daemon_restart_orphan_recovery",
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );

        write_status_json(
            &run_dir,
            &dispatch_id,
            status
                .get("v2")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            status
                .get("plan_generated_at")
                .and_then(serde_json::Value::as_str),
            status
                .get("executed_at")
                .and_then(serde_json::Value::as_str),
            status
                .get("plan_review_status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("n/a"),
            Some(1),
            None,
            None,
            None,
            Some(json!({
                "state": "TASK_STATE_FAILED",
                "updated_at": Utc::now().to_rfc3339(),
                "exit_code": 1,
                "recovery_reason": "daemon_restart_orphan_recovery",
                "previous_state": previous_state,
            })),
        );
        // #774 round 2: this recovery IS the terminal path for an orphaned
        // run — no `tachi_complete` is coming — so it writes the same
        // canonical outcome row every other terminal-without-complete path
        // writes (first-writer-wins, so this is a no-op if some earlier
        // classifier already recorded the row for this dispatch_id).
        crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
            server,
            &dispatch_id,
            RECOVERED_ORPHAN_ERROR_CLASS,
            agent.as_deref(),
            project.as_deref(),
        );
        recovered.push(dispatch_id);
    }
    recovered
}
