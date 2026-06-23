use super::*;

pub(crate) fn mark_task_close_loop(flow_id: &str, raw_result: &str) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let close_loop_path = run_dir.join("close_loop.json");
    let payload = serde_json::from_str::<Value>(raw_result).unwrap_or_else(|_| {
        json!({
            "action": "close_loop",
            "raw_result": raw_result,
        })
    });
    write_json_atomic(&close_loop_path, &payload)?;
    let path_string = close_loop_path.to_string_lossy().to_string();
    merge_flow_status(
        &run_dir,
        json!({
            "state": "closed_loop",
            "closed_at": Utc::now().to_rfc3339(),
            "close_loop_path": path_string,
            "artifacts": { "close_loop": path_string },
        }),
    )?;
    Ok(())
}
