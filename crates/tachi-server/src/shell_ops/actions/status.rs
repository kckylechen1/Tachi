use super::*;

pub(in crate::shell_ops) async fn handle_status_action(
    params: TachiShellParams,
) -> Result<String, String> {
    let runs_root = shell_runs_root();
    if let Some(flow_id) = params.flow_id.as_deref() {
        validate_flow_id(flow_id)?;
        let run_dir = runs_root.join(flow_id);
        if !run_dir.exists() {
            return serde_json::to_string(&json!({
                "flow_id": flow_id,
                "found": false,
                "message": "no run directory exists for this flow_id",
            }))
            .map_err(|e| format!("serialize: {e}"));
        }
        let status = read_status_async(&run_dir).await;
        return serde_json::to_string(&json!({
            "flow_id": flow_id,
            "found": true,
            "run_dir": run_dir.to_string_lossy(),
            "status": status,
        }))
        .map_err(|e| format!("serialize: {e}"));
    }
    // List recent flows
    let limit = params.limit.unwrap_or(20);
    let mut flows = Vec::new();
    match tokio::fs::read_dir(&runs_root).await {
        Ok(mut read_dir) => {
            while let Some(entry) = read_dir
                .next_entry()
                .await
                .map_err(|e| format!("read shell runs root {}: {e}", runs_root.display()))?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("flow_") {
                    continue;
                }
                let status = read_status_async(&entry.path()).await;
                flows.push(json!({
                    "flow_id": name,
                    "stage": status.get("stage"),
                    "state": status.get("state"),
                    "updated_at": status.get("updated_at"),
                }));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(format!(
                "read shell runs root {}: {err}",
                runs_root.display()
            ))
        }
    }
    flows.sort_by(|a, b| {
        let ta = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    flows.truncate(limit);
    serde_json::to_string(&json!({
        "flows": flows,
        "count": flows.len(),
        "runs_root": runs_root.to_string_lossy(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
