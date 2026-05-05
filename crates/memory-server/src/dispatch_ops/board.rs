use super::*;

// ─── Task Board (Kanban) handler ──────────────────────────────────────────────

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
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.7),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: params.project.clone(),
            domain: None,
            enable_rerank: false,
        },
    )
    .await?;

    // Filter by state if requested
    let state_filter = params.state_filter.as_deref().unwrap_or("all");
    let filtered: Vec<&serde_json::Value> = if state_filter == "all" {
        rows.iter().collect()
    } else {
        let target_state = match state_filter {
            "working" => "TASK_STATE_WORKING",
            "completed" => "TASK_STATE_COMPLETED",
            "failed" => "TASK_STATE_FAILED",
            "pending" => "TASK_STATE_PENDING",
            "input_required" => "TASK_STATE_INPUT_REQUIRED",
            "canceled" => "TASK_STATE_CANCELED",
            other => other, // allow raw A2A state
        };
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
    let tasks: Vec<serde_json::Value> = filtered
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
            })
        })
        .collect();

    serde_json::to_string(&json!({
        "board": "kanban",
        "filter": state_filter,
        "count": tasks.len(),
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}
