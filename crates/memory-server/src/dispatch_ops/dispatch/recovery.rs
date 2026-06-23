use super::*;

pub(super) fn dispatch_status_needs_recovery(status: &serde_json::Value) -> bool {
    if status.get("exit_code").is_some() {
        return false;
    }
    match status.get("state").and_then(serde_json::Value::as_str) {
        Some("TASK_STATE_WORKING" | "TASK_STATE_PENDING" | "TASK_STATE_RUNNING") => true,
        Some(_) => false,
        None => true,
    }
}

/// Mark orphaned in-flight dispatch runs as failed after daemon restart.
pub(crate) fn recover_orphaned_dispatch_runs() -> Vec<String> {
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
        recovered.push(dispatch_id);
    }
    recovered
}
