use super::flow::{flow_dispatch_ids, merge_run_task};
use super::paths::runs_dir_for_server;
use super::runs::{collect_run_task_by_id, collect_run_tasks_from_dir};
use super::status::{is_terminal_state, mark_abandoned_kanban_task, state_matches_filter};
use crate::tool_params::{SearchMemoryParams, TachiBoardParams};
use crate::MemoryServer;
use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

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
            context_symbols: Vec::new(),
            agent_role: None,
            project: params.project.clone(),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
            format: None,
        },
        false,
    )
    .await?;

    let state_filter = params
        .state_filter
        .clone()
        .unwrap_or_else(|| "all".to_string());
    let state_filter_name = state_filter.as_str();

    // Build compact board view
    let mut tasks: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let meta = row.get("metadata").cloned().unwrap_or(json!({}));
            let updated_at = meta
                .get("updated_at")
                .cloned()
                .or_else(|| row.get("timestamp").cloned());
            json!({
                "dispatch_id": meta.get("dispatch_id"),
                "agent": meta.get("agent"),
                "state": meta.get("a2a_state"),
                "eval_id": meta.get("eval_ledger_id"),
                "summary": row.get("summary"),
                "updated_at": updated_at,
                "timeout_secs": meta.get("timeout_secs"),
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
        let run_state_filter = state_filter.clone();
        tokio::task::spawn_blocking(move || {
            collect_run_tasks_from_dir(runs_dir, &run_state_filter, run_scan_limit)
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
    let now = Utc::now();
    for task in &mut tasks {
        mark_abandoned_kanban_task(task, now);
    }
    if state_filter_name != "all" {
        tasks.retain(|task| {
            task.get("state")
                .and_then(|v| v.as_str())
                .is_some_and(|state| state_matches_filter(state_filter_name, state))
        });
    }

    // tachi#1173 item 3: default board rows omit the heavy per-row session
    // payload (identity_receipt/acpx/acpx_events) unless the caller asks for
    // the full shape via verbose=true. Kanban-only rows never carried these
    // fields; run-ledger rows do (see `runs::collect_run_tasks_from_dir` /
    // `collect_run_task_from_dir`) -- stripped here rather than at the
    // shared collectors, which `wait`/`status` still call for full fidelity.
    let verbose = params.verbose.unwrap_or(false);
    if !verbose {
        for task in &mut tasks {
            if let Some(obj) = task.as_object_mut() {
                obj.remove("identity_receipt");
                obj.remove("acpx");
                obj.remove("acpx_events");
            }
        }
    }

    // tachi#1173 item 3: fold terminal (completed/failed/canceled) rows into
    // a per-state count row on the default (unfiltered, non-verbose) view so
    // a long-lived board doesn't drown active work under historical noise.
    // An explicit non-"all" state_filter is itself an ask to see that state
    // expanded, so folding never applies there regardless of `verbose`. A
    // flow_id filter is the same kind of explicit, scoped ask -- a flow
    // board is expected to show every dispatch it recorded (including
    // completed ones), so folding is skipped there too (see
    // `tachi_task_board_filters_to_flow_dispatch_ids`, which asserts a
    // completed flow dispatch is still an individual row).
    let fold_terminal = !verbose && state_filter_name == "all" && flow_filter.is_none();
    let mut folded_counts: BTreeMap<String, usize> = BTreeMap::new();
    if fold_terminal {
        let mut kept = Vec::with_capacity(tasks.len());
        for task in tasks {
            let state = task
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if is_terminal_state(&state) {
                *folded_counts.entry(state).or_insert(0) += 1;
            } else {
                kept.push(task);
            }
        }
        tasks = kept;
    }

    // tachi#1173 item 7: attach a bounded, ANSI-free failure_tail to any
    // visible individual row for a terminal-failed dispatch, so a caller can
    // autopsy the failure without a separate file read. This only reaches
    // failed rows that survived the fold step above (i.e. an explicit
    // state_filter or verbose=true asked to see them expanded) -- a folded
    // count row has no single run to read a tail from.
    for task in &mut tasks {
        if task.get("state").and_then(|v| v.as_str()) != Some("TASK_STATE_FAILED") {
            continue;
        }
        let Some(run_dir) = task
            .get("run_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
        else {
            continue;
        };
        if let Some(tail) = super::read_failure_tail(&run_dir) {
            if let Some(obj) = task.as_object_mut() {
                obj.insert("failure_tail".to_string(), json!(tail));
            }
        }
    }

    tasks.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    tasks.truncate(limit);

    let folded_total: usize = folded_counts.values().sum();
    if folded_total > 0 {
        for (state, count) in &folded_counts {
            tasks.push(json!({
                "source": "folded",
                "folded": true,
                "state": state,
                "count": count,
            }));
        }
    }

    serde_json::to_string(&json!({
        "board": "kanban",
        "flow_id": flow_filter,
        "filter": state_filter_name,
        "count": tasks.len(),
        "kanban_count": kanban_count,
        "run_count": run_count,
        "folded_total": folded_total,
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}
