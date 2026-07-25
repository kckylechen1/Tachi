use super::flow::{flow_dispatch_ids, merge_run_task};
use super::paths::runs_dir_for_server;
use super::runs::{
    collect_run_task_by_id, collect_run_tasks_from_dir, RunTaskScan,
    BOARD_RUN_DIRECTORY_CANDIDATE_HARD_MAX,
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
const BOARD_KANBAN_FETCH_PER_RETURNED_ROW: usize = 5;
const BOARD_KANBAN_FETCH_MINIMUM: usize = 50;
pub(super) const BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX: usize = 500;
const BOARD_KANBAN_FETCH_INSPECTION_HARD_MAX: usize = BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX + 1;

struct KanbanFetch {
    entries: Vec<memcore::MemoryEntry>,
    truncated: bool,
}

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

pub(super) fn bounded_kanban_fetch_limit(limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    limit
        .saturating_mul(BOARD_KANBAN_FETCH_PER_RETURNED_ROW)
        .max(BOARD_KANBAN_FETCH_MINIMUM)
        .min(BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX)
}

fn recent_kanban_entries(
    server: &MemoryServer,
    project: Option<&str>,
    candidate_limit: usize,
) -> Result<KanbanFetch, String> {
    let inspection_limit = candidate_limit
        .saturating_add(1)
        .min(BOARD_KANBAN_FETCH_INSPECTION_HARD_MAX);
    let load = |store: &mut memcore::MemoryStore| {
        store
            .list_by_path_recent("/kanban/tasks", inspection_limit, false)
            .map_err(|error| format!("list recent kanban rows: {error}"))
    };

    if let Some(project_name) = project {
        let mut entries = server.with_named_project_store_read(project_name, load)?;
        let truncated = entries.len() > candidate_limit;
        entries.truncate(candidate_limit);
        return Ok(KanbanFetch { entries, truncated });
    }

    let mut entries = server.with_global_store_read(load)?;
    let mut truncated = entries.len() > candidate_limit;
    if server.has_project_db() {
        let mut project_entries = server.with_project_store_read(load)?;
        truncated |= project_entries.len() > candidate_limit;
        entries.append(&mut project_entries);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    truncated |= entries.len() > candidate_limit;
    entries.truncate(candidate_limit);
    Ok(KanbanFetch { entries, truncated })
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
            "kanban_fetch_limit": 0,
            "kanban_fetch_truncated": false,
            "flow_fetch_truncated": false,
            "run_count": 0,
            "run_scan_limit": 0,
            "run_scan_inspected": 0,
            "run_scan_truncated": false,
            "run_scan_invalid_entries": 0,
            "run_fallback_incomplete": false,
            "limit_incomplete": false,
            "incomplete": false,
            "incomplete_reasons": [],
            "warning": null,
            "folded_total": 0,
            "tasks": [],
        }))
        .map_err(|error| format!("serialize board: {error}"));
    }

    // The maintained Kanban ledger, rather than filesystem enumeration, is
    // the authoritative recency index for board rows.
    let kanban_fetch_limit = bounded_kanban_fetch_limit(limit);
    let kanban_fetch =
        recent_kanban_entries(server, params.project.as_deref(), kanban_fetch_limit)?;
    let kanban_fetch_truncated = kanban_fetch.truncated;
    let mut tasks: Vec<serde_json::Value> = kanban_fetch
        .entries
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
    let mut flow_fetch_truncated = false;
    if let Some(flow_id) = flow_filter.as_deref() {
        let flow_lookup = flow_dispatch_ids(flow_id, kanban_fetch_limit)?;
        flow_fetch_truncated = flow_lookup.as_ref().is_some_and(|lookup| lookup.truncated);
        let flow_ids = flow_lookup.map(|lookup| lookup.ids).unwrap_or_default();
        let flow_id_set: std::collections::HashSet<String> = flow_ids.iter().cloned().collect();
        tasks.retain(|task| {
            task.get("dispatch_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| flow_id_set.contains(id))
        });
        for dispatch_id in &flow_ids {
            if seen.contains(dispatch_id) {
                if let Some(run_task) = collect_run_task_by_id(&runs_dir, dispatch_id)? {
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
            if let Some(task) = collect_run_task_by_id(&runs_dir, dispatch_id)? {
                flow_run_count += 1;
                seen.insert(dispatch_id.clone());
                tasks.push(task);
            }
        }
    }
    let run_scan_limit = bounded_run_scan_limit(limit);
    let mut run_scan = if flow_filter.is_some() {
        RunTaskScan::default()
    } else {
        let run_state_filter = state_filter.clone();
        tokio::task::spawn_blocking(move || {
            collect_run_tasks_from_dir(runs_dir, &run_state_filter, run_scan_limit)
        })
        .await
        .unwrap_or_else(|error| {
            RunTaskScan::failed(format!("board run fallback worker failed: {error}"))
        })
    };
    let run_count = if flow_filter.is_some() {
        flow_run_count
    } else {
        run_scan.tasks.len()
    };
    for task in run_scan.tasks.drain(..) {
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
            match obj
                .get("run_dir")
                .and_then(|value| value.as_str())
                .map(PathBuf::from)
            {
                Some(run_dir) => super::read_failure_tail(&run_dir)?,
                None => None,
            }
        } else {
            None
        };
        obj.insert("failure_tail".to_string(), json!(tail));
    }

    let run_fallback_incomplete = run_scan.incomplete();
    let mut incomplete_reasons = Vec::new();
    if kanban_fetch_truncated {
        incomplete_reasons.push("kanban_fetch_truncated");
    }
    if flow_fetch_truncated {
        incomplete_reasons.push("flow_fetch_truncated");
    }
    if run_scan.truncated {
        incomplete_reasons.push("run_fallback_scan_truncated");
    }
    if run_scan.invalid_entries > 0 {
        incomplete_reasons.push("run_fallback_invalid_entries");
    }
    if run_scan.error.is_some() {
        incomplete_reasons.push("run_fallback_error");
    }
    let incomplete = !incomplete_reasons.is_empty();
    let limit_incomplete = incomplete && tasks.len() < limit;
    let warning = if run_fallback_incomplete {
        Some(
            "board response is incomplete: filesystem fallback is a bounded sample and directory order is not a recency index",
        )
    } else if kanban_fetch_truncated || flow_fetch_truncated {
        Some("board response is incomplete: indexed board fetch reached its hard cap")
    } else {
        None
    };

    serde_json::to_string(&json!({
        "board": "kanban",
        "flow_id": flow_filter,
        "filter": state_filter_name,
        "limit": limit,
        "count": tasks.len(),
        "kanban_count": kanban_count,
        "kanban_fetch_limit": kanban_fetch_limit,
        "kanban_fetch_truncated": kanban_fetch_truncated,
        "flow_fetch_truncated": flow_fetch_truncated,
        "run_count": run_count,
        "run_scan_limit": run_scan_limit,
        "run_scan_inspected": run_scan.inspected_entries,
        "run_scan_truncated": run_scan.truncated,
        "run_scan_invalid_entries": run_scan.invalid_entries,
        "run_fallback_incomplete": run_fallback_incomplete,
        "limit_incomplete": limit_incomplete,
        "incomplete": incomplete,
        "incomplete_reasons": incomplete_reasons,
        "warning": warning,
        "folded_total": folded_total,
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}
