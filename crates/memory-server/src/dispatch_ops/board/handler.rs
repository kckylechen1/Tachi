use super::flow::{flow_dispatch_ids, merge_run_task};
use super::paths::runs_dir_for_server;
use super::runs::{collect_run_task_by_id, collect_run_tasks_from_dir};
use super::status::map_filter_state;
use crate::tool_params::{SearchMemoryParams, TachiBoardParams};
use crate::MemoryServer;
use serde_json::json;

pub(crate) async fn handle_tachi_board(
    server: &MemoryServer,
    params: TachiBoardParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(20);
    let flow_filter = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);

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

    let state_filter = params.state_filter.as_deref().unwrap_or("all");

    // Build compact board view
    let mut tasks: Vec<serde_json::Value> = rows
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
    let runs_dir = runs_dir_for_server(server);
    let mut flow_run_count = 0usize;
    if let Some(flow_id) = flow_filter.as_deref() {
        let flow_ids = flow_dispatch_ids(flow_id)?;
        let flow_id_set: std::collections::HashSet<String> = flow_ids.iter().cloned().collect();
        tasks.retain(|task| {
            task.get("dispatch_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| flow_id_set.contains(id))
        });
        for dispatch_id in &flow_ids {
            if seen.contains(dispatch_id) {
                if let Some(run_task) = collect_run_task_by_id(&runs_dir, dispatch_id) {
                    flow_run_count += 1;
                    if let Some(existing) = tasks.iter_mut().find(|candidate| {
                        candidate.get("dispatch_id").and_then(|v| v.as_str())
                            == Some(dispatch_id.as_str())
                    }) {
                        merge_run_task(existing, &run_task, true);
                    }
                }
                continue;
            }
            if let Some(task) = collect_run_task_by_id(&runs_dir, dispatch_id) {
                flow_run_count += 1;
                seen.insert(dispatch_id.clone());
                tasks.push(task);
            }
        }
    }
    let run_scan_limit = limit.saturating_mul(5).max(50);
    let run_tasks = if flow_filter.is_some() {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(move || {
            collect_run_tasks_from_dir(runs_dir, "all", run_scan_limit)
        })
        .await
        .unwrap_or_default()
    };
    let run_count = if flow_filter.is_some() {
        flow_run_count
    } else {
        run_tasks.len()
    };
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
                    merge_run_task(existing, &task, false);
                }
                continue;
            }
            seen.insert(id.to_string());
        }
        tasks.push(task);
    }
    if state_filter != "all" {
        let target_state = map_filter_state(state_filter);
        tasks.retain(|task| task.get("state").and_then(|v| v.as_str()) == Some(target_state));
    }
    tasks.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    tasks.truncate(limit);

    serde_json::to_string(&json!({
        "board": "kanban",
        "flow_id": flow_filter,
        "filter": state_filter,
        "count": tasks.len(),
        "kanban_count": kanban_count,
        "run_count": run_count,
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}
