use super::flow::{flow_dispatch_ids, merge_run_task};
use super::paths::runs_dir_for_server;
use super::runs::{
    collect_run_task_by_id, collect_run_tasks_from_dir, BOARD_RUN_DIRECTORY_CANDIDATE_HARD_MAX,
};
use super::status::{
    is_terminal_state_with_closure_kind, mark_abandoned_kanban_task,
    state_matches_filter_with_closure_kind,
};
use crate::tool_params::TachiBoardParams;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

const DEFAULT_BOARD_LIMIT: usize = 20;
pub(super) const BOARD_RETURN_LIMIT_HARD_MAX: usize = 100;
const BOARD_RUN_SCAN_PER_RETURNED_ROW: usize = 5;
const BOARD_RUN_SCAN_MINIMUM: usize = 50;

pub(super) fn bounded_board_limit(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_BOARD_LIMIT)
        .min(BOARD_RETURN_LIMIT_HARD_MAX)
}

pub(super) fn bounded_run_scan_limit(limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    limit
        .saturating_mul(BOARD_RUN_SCAN_PER_RETURNED_ROW)
        .max(BOARD_RUN_SCAN_MINIMUM)
        .min(BOARD_RUN_DIRECTORY_CANDIDATE_HARD_MAX)
}

fn recent_kanban_entries(
    server: &MemoryServer,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<memcore::MemoryEntry>, String> {
    let load = |store: &mut memcore::MemoryStore| {
        store
            .list_by_path_recent("/kanban/tasks", limit, false)
            .map_err(|error| format!("list recent kanban rows: {error}"))
    };

    if let Some(project_name) = project {
        return server.with_named_project_store_read(project_name, load);
    }

    let mut entries = server.with_global_store_read(load)?;
    if server.has_project_db() {
        entries.append(&mut server.with_project_store_read(load)?);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries)
}

pub(crate) async fn handle_tachi_board(
    server: &MemoryServer,
    params: TachiBoardParams,
) -> Result<String, String> {
    let limit = bounded_board_limit(params.limit);
    let flow_filter = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);

    let state_filter_explicit = params
        .state_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    let state_filter = params
        .state_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "all".to_string());
    let state_filter_name = state_filter.as_str();

    if limit == 0 {
        return serde_json::to_string(&json!({
            "board": "kanban",
            "flow_id": flow_filter,
            "filter": state_filter_name,
            "limit": limit,
            "count": 0,
            "kanban_count": 0,
            "run_count": 0,
            "folded_total": 0,
            "tasks": [],
        }))
        .map_err(|error| format!("serialize board: {error}"));
    }

    // The maintained Kanban ledger, rather than filesystem enumeration, is
    // the authoritative recency index for board rows.
    let rows = recent_kanban_entries(server, params.project.as_deref(), limit)?;
    let mut tasks: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            let meta = row.metadata;
            let updated_at = meta
                .get("updated_at")
                .cloned()
                .unwrap_or_else(|| json!(row.timestamp));
            json!({
                "dispatch_id": meta.get("dispatch_id"),
                "agent": meta.get("agent"),
                "state": meta.get("a2a_state"),
                "closure_kind": meta.get("closure_kind"),
                "eval_id": meta.get("eval_ledger_id"),
                "summary": row.summary,
                "updated_at": updated_at,
                "timeout_secs": meta.get("timeout_secs"),
                "source": "kanban",
            })
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    tasks.retain(|task| {
        task.get("dispatch_id")
            .and_then(|value| value.as_str())
            .map(|id| seen.insert(id.to_string()))
            .unwrap_or(true)
    });
    let kanban_count = tasks.len();
    let runs_dir = runs_dir_for_server(server);
    let mut flow_run_count = 0usize;
    if let Some(flow_id) = flow_filter.as_deref() {
        let flow_ids = flow_dispatch_ids(flow_id, limit)?;
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
    let run_scan_limit = bounded_run_scan_limit(limit);
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
                .is_some_and(|state| {
                    state_matches_filter_with_closure_kind(
                        state_filter_name,
                        state,
                        task.get("closure_kind").and_then(|v| v.as_str()),
                    )
                })
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
    // a per-state count row on the default (state_filter omitted, non-verbose)
    // view so a long-lived board doesn't drown active work under historical
    // noise. Any explicitly-passed state_filter -- including the literal
    // value "all" -- is itself an ask to see that state (or every state)
    // expanded, so folding never applies there regardless of `verbose`;
    // folding is purely a default-view convenience, not a filter behavior a
    // caller can accidentally trigger by asking for "all" on purpose. A
    // flow_id filter is the same kind of explicit, scoped ask -- a flow
    // board is expected to show every dispatch it recorded (including
    // completed ones), so folding is skipped there too (see
    // `tachi_task_board_filters_to_flow_dispatch_ids`, which asserts a
    // completed flow dispatch is still an individual row).
    let fold_terminal = !verbose && !state_filter_explicit && flow_filter.is_none();
    let mut folded_counts: BTreeMap<String, usize> = BTreeMap::new();
    if fold_terminal {
        let mut kept = Vec::with_capacity(tasks.len());
        for task in tasks {
            let state = task
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if is_terminal_state_with_closure_kind(
                &state,
                task.get("closure_kind").and_then(|v| v.as_str()),
            ) {
                *folded_counts.entry(state).or_insert(0) += 1;
            } else {
                kept.push(task);
            }
        }
        tasks = kept;
    }

    tasks.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });

    // Reserve room for folded summaries inside the effective limit. A tiny
    // caller limit can represent only the first few state summaries, but it
    // must never grow the returned task array beyond that explicit bound.
    let folded_total: usize = folded_counts.values().sum();
    let folded_rows: Vec<_> = folded_counts.iter().take(limit).collect();
    let individual_limit = limit.saturating_sub(folded_rows.len());
    tasks.truncate(individual_limit);
    if folded_total > 0 {
        for (state, count) in folded_rows {
            tasks.push(json!({
                "source": "folded",
                "folded": true,
                "state": state,
                "count": count,
            }));
        }
    }

    // Attach the failure tail only after the return limit has been enforced.
    // This keeps per-row filesystem reads bounded by the visible task count.
    for task in &mut tasks {
        let Some(obj) = task.as_object_mut() else {
            continue;
        };
        if obj.get("folded").and_then(|value| value.as_bool()) == Some(true) {
            continue;
        }
        let is_failed =
            obj.get("state").and_then(|value| value.as_str()) == Some("TASK_STATE_FAILED");
        let tail = if is_failed {
            obj.get("run_dir")
                .and_then(|value| value.as_str())
                .map(PathBuf::from)
                .and_then(|run_dir| super::read_failure_tail(&run_dir))
        } else {
            None
        };
        obj.insert("failure_tail".to_string(), json!(tail));
    }

    serde_json::to_string(&json!({
        "board": "kanban",
        "flow_id": flow_filter,
        "filter": state_filter_name,
        "limit": limit,
        "count": tasks.len(),
        "kanban_count": kanban_count,
        "run_count": run_count,
        "folded_total": folded_total,
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}
