use serde_json::{json, Value};

pub(super) fn flow_dispatch_ids(flow_id: &str) -> Result<Vec<String>, String> {
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id)?;
    let status_path = run_dir.join("status.json");
    let Some(status) = crate::task_lifecycle::read_json_file(&status_path)? else {
        return Ok(Vec::new());
    };
    Ok(status
        .get("dispatch_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
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
