use serde_json::{json, Value};

const BOARD_FLOW_STATUS_MAX_BYTES: usize = 1024 * 1024;

pub(super) struct FlowDispatchIds {
    pub(super) ids: Vec<String>,
    pub(super) truncated: bool,
}

pub(super) fn flow_dispatch_ids(
    flow_id: &str,
    limit: usize,
) -> Result<Option<FlowDispatchIds>, String> {
    if limit == 0 {
        return Ok(Some(FlowDispatchIds {
            ids: Vec::new(),
            truncated: false,
        }));
    }
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id)?;
    let status_path = run_dir.join("status.json");
    let runs_root = run_dir.parent().ok_or_else(|| {
        format!(
            "flow run directory {} has no containment root",
            run_dir.display()
        )
    })?;
    let Some(status_raw) = crate::dispatch_ops::read_text_file_within(
        runs_root,
        &status_path,
        BOARD_FLOW_STATUS_MAX_BYTES,
    )?
    else {
        return Ok(None);
    };
    let status: Value = serde_json::from_str(&status_raw).map_err(|error| {
        format!(
            "flow status artifact {} is not valid JSON: {error}",
            status_path.display()
        )
    })?;
    let dispatch_ids = status
        .get("dispatch_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "flow status {} has no dispatch_ids array",
                status_path.display()
            )
        })?;
    for value in dispatch_ids {
        let id = value.as_str().ok_or_else(|| {
            format!(
                "flow status {} has a non-string dispatch id",
                status_path.display()
            )
        })?;
        if !crate::dispatch_ops::is_valid_dispatch_id(id) {
            return Err(format!(
                "flow status {} has an invalid dispatch id",
                status_path.display()
            ));
        }
    }
    let newest_start = dispatch_ids.len().saturating_sub(limit);
    let ids = dispatch_ids
        .iter()
        .skip(newest_start)
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    Ok(Some(FlowDispatchIds {
        ids,
        truncated: dispatch_ids.len() > limit,
    }))
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
