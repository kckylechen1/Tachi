use super::*;

// ─── Task Board (Kanban) handler ──────────────────────────────────────────────

fn tachi_home() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    }
}

fn map_filter_state(state_filter: &str) -> &str {
    match state_filter {
        "working" => "TASK_STATE_WORKING",
        "completed" => "TASK_STATE_COMPLETED",
        "failed" => "TASK_STATE_FAILED",
        "pending" => "TASK_STATE_PENDING",
        "input_required" => "TASK_STATE_INPUT_REQUIRED",
        "canceled" => "TASK_STATE_CANCELED",
        other => other,
    }
}

fn status_state(status: &serde_json::Value, result_written: bool) -> &'static str {
    if let Some(state) = status.get("state").and_then(|s| s.as_str()) {
        return match state {
            "TASK_STATE_COMPLETED" => "TASK_STATE_COMPLETED",
            "TASK_STATE_FAILED" => "TASK_STATE_FAILED",
            "TASK_STATE_PENDING" => "TASK_STATE_PENDING",
            "TASK_STATE_INPUT_REQUIRED" => "TASK_STATE_INPUT_REQUIRED",
            "TASK_STATE_CANCELED" => "TASK_STATE_CANCELED",
            _ => "TASK_STATE_WORKING",
        };
    }
    if status.get("plan_review_status").and_then(|s| s.as_str()) == Some("pending_review") {
        return "TASK_STATE_INPUT_REQUIRED";
    }
    match status.get("exit_code") {
        Some(serde_json::Value::Number(n)) if n.as_i64() == Some(0) => "TASK_STATE_COMPLETED",
        Some(serde_json::Value::Number(_)) => "TASK_STATE_FAILED",
        _ if result_written => "TASK_STATE_COMPLETED",
        _ => "TASK_STATE_WORKING",
    }
}

fn collect_run_tasks(state_filter: &str, limit: usize) -> Vec<serde_json::Value> {
    let runs_dir = tachi_home().join("runs");
    let Ok(read_dir) = std::fs::read_dir(&runs_dir) else {
        return Vec::new();
    };

    let mut entries = read_dir.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    let target_state = map_filter_state(state_filter);
    let mut runs = Vec::new();

    for entry in entries {
        if runs.len() >= limit {
            break;
        }
        let run_dir = entry.path();
        if !run_dir.is_dir() {
            continue;
        }
        let status_path = run_dir.join("status.json");
        let Ok(status_raw) = std::fs::read_to_string(&status_path) else {
            continue;
        };
        let Ok(status) = serde_json::from_str::<serde_json::Value>(&status_raw) else {
            continue;
        };
        let Some(dispatch_id) = status
            .get("dispatch_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                run_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
        else {
            continue;
        };
        let result_written = run_dir.join("result.md").exists();
        let state = status_state(&status, result_written);
        if state_filter != "all" && state != target_state {
            continue;
        }
        let updated_at = status
            .get("updated_at")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                std::fs::metadata(&status_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(chrono::DateTime::<Utc>::from)
                    .map(|dt| dt.to_rfc3339())
            });
        runs.push(json!({
            "dispatch_id": dispatch_id,
            "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
            "state": state,
            "exit_code": status.get("exit_code").cloned().unwrap_or(serde_json::Value::Null),
            "summary": status.get("task").cloned().unwrap_or(serde_json::Value::Null),
            "updated_at": updated_at,
            "run_dir": run_dir.to_string_lossy(),
            "result_written": result_written,
            "source": "run",
        }));
    }

    runs.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    runs
}

pub(crate) async fn handle_tachi_board(
    server: &MemoryServer,
    params: TachiBoardParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(20);

    let rows = crate::memory_search_ops::search_memory_rows(
        server,
        SearchMemoryParams {
            query: "kanban dispatch task".to_string(),
            query_vec: None,
            top_k: limit,
            path_prefix: Some("/kanban/tasks/".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.7),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: params.project.clone(),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        false,
    )
    .await?;

    // Filter by state if requested
    let state_filter = params.state_filter.as_deref().unwrap_or("all");
    let filtered: Vec<&serde_json::Value> = if state_filter == "all" {
        rows.iter().collect()
    } else {
        let target_state = map_filter_state(state_filter);
        rows.iter()
            .filter(|row| {
                row.get("metadata")
                    .and_then(|m| m.get("a2a_state"))
                    .and_then(|s| s.as_str())
                    == Some(target_state)
            })
            .collect()
    };

    // Build compact board view
    let mut tasks: Vec<serde_json::Value> = filtered
        .iter()
        .map(|row| {
            let meta = row.get("metadata").cloned().unwrap_or(json!({}));
            json!({
                "dispatch_id": meta.get("dispatch_id"),
                "agent": meta.get("agent"),
                "state": meta.get("a2a_state"),
                "eval_id": meta.get("eval_ledger_id"),
                "summary": row.get("summary"),
                "updated_at": meta.get("updated_at"),
                "source": "kanban",
            })
        })
        .collect();
    let kanban_count = tasks.len();
    let mut seen = std::collections::HashSet::new();
    for task in &tasks {
        if let Some(id) = task.get("dispatch_id").and_then(|v| v.as_str()) {
            seen.insert(id.to_string());
        }
    }
    let state_filter_owned = state_filter.to_string();
    let run_tasks =
        tokio::task::spawn_blocking(move || collect_run_tasks(&state_filter_owned, limit))
            .await
            .unwrap_or_default();
    let run_count = run_tasks.len();
    for task in run_tasks {
        let dispatch_id = task
            .get("dispatch_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(id) = dispatch_id.as_deref() {
            if seen.contains(id) {
                if let Some(existing) = tasks.iter_mut().find(|candidate| {
                    candidate.get("dispatch_id").and_then(|v| v.as_str()) == Some(id)
                }) {
                    if let Some(obj) = existing.as_object_mut() {
                        for key in ["run_dir", "result_written", "exit_code"] {
                            if obj.get(key).is_none() {
                                obj.insert(
                                    key.to_string(),
                                    task.get(key).cloned().unwrap_or(serde_json::Value::Null),
                                );
                            }
                        }
                        obj.insert("source".to_string(), json!("kanban+run"));
                    }
                }
                continue;
            }
            seen.insert(id.to_string());
        }
        tasks.push(task);
    }
    tasks.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    tasks.truncate(limit);

    serde_json::to_string(&json!({
        "board": "kanban",
        "filter": state_filter,
        "count": tasks.len(),
        "kanban_count": kanban_count,
        "run_count": run_count,
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}
