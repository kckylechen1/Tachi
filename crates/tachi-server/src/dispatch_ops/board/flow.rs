use super::runs::read_bounded_json_file;
use serde_json::{json, Value};
use std::io::ErrorKind;

pub(super) fn flow_dispatch_ids(flow_id: &str, limit: usize) -> Result<Vec<String>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id)?;
    let status_path = run_dir.join("status.json");
    match std::fs::symlink_metadata(&status_path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "inspect flow status {}: {error}",
                status_path.display()
            ));
        }
    }
    let status = read_bounded_json_file(&status_path)
        .map_err(|error| format!("read flow status {}: {error}", status_path.display()))?;
    let dispatch_ids = status
        .get("dispatch_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let newest_start = dispatch_ids.len().saturating_sub(limit);
    Ok(dispatch_ids
        .into_iter()
        .skip(newest_start)
        .map(str::to_string)
        .collect())
}

pub(super) fn merge_run_task(
    existing: &mut serde_json::Value,
    run_task: &serde_json::Value,
    authoritative_state: bool,
) {
    if let Some(obj) = existing.as_object_mut() {
        for key in [
            "run_dir",
            "result_written",
            "closure_kind",
            "exit_code",
            "stale",
            "stale_reason",
            "state_source",
            "harness_transport",
            "harness_server_url",
            "harness_server_status",
            "execution_backend",
            "acpx",
            "acpx_events",
        ] {
            if obj.get(key).is_none() {
                obj.insert(
                    key.to_string(),
                    run_task
                        .get(key)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
            }
        }
        if authoritative_state || run_task.get("stale").and_then(|v| v.as_bool()) == Some(true) {
            obj.insert(
                "state".to_string(),
                run_task
                    .get("state")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "updated_at".to_string(),
                run_task
                    .get("updated_at")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "closure_kind".to_string(),
                run_task
                    .get("closure_kind")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        obj.insert("source".to_string(), json!("kanban+run"));
    }
}
