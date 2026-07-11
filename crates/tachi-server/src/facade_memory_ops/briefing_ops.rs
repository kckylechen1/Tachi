//! Briefing handler for `tachi_memory(action="briefing")`.
//!
//! Memories/wiki are **workspace-scoped** (`project_only` search). Cross-project
//! signal lives in global **handoff** memos (like a local issue board) — not
//! generic global hybrid search.

use super::evidence_format::{
    json_string, parse_evidence_array, parse_json_or_empty, slim_kanban, slim_memory_rows,
    wants_json,
};
use crate::agent_markdown;
use crate::memory_search_ops::{
    handle_search_memory, list_available_named_projects, resolve_effective_named_project,
    search_memory_rows,
};
use crate::tool_params::*;
use crate::MemoryServer;
use serde_json::{json, Map, Value};

const BRIEFING_COMPACT_RELEVANCE_FLOOR: f64 = 0.25;

fn compact_row_relevance(row: &Value) -> Option<f64> {
    row.get("relevance")
        .and_then(Value::as_f64)
        .or_else(|| {
            row.get("score")
                .and_then(|score| score.get("final"))
                .and_then(Value::as_f64)
        })
        .or_else(|| row.get("score").and_then(Value::as_f64))
}

fn meets_compact_relevance_floor(row: &Value) -> bool {
    compact_row_relevance(row).is_none_or(|score| score >= BRIEFING_COMPACT_RELEVANCE_FLOOR)
}

fn apply_compact_relevance_floor(value: Value) -> Value {
    match value {
        Value::Array(rows) => Value::Array(
            rows.into_iter()
                .filter(meets_compact_relevance_floor)
                .collect(),
        ),
        other => other,
    }
}

fn compact_section_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(rows) => rows.is_empty(),
        Value::Object(map) => {
            map.is_empty()
                || map
                    .get("count")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count == 0)
                || map
                    .get("tasks")
                    .and_then(Value::as_array)
                    .is_some_and(|tasks| tasks.is_empty())
        }
        _ => false,
    }
}

fn insert_non_empty_compact_section(map: &mut Map<String, Value>, key: &str, value: Value) {
    if !compact_section_is_empty(&value) {
        map.insert(key.to_string(), value);
    }
}

async fn compact_health_summary(server: &MemoryServer, wiki_counts: Value) -> Value {
    let status = crate::status_ops::handle_tachi_status_agent(server)
        .await
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .unwrap_or_else(|| json!({}));
    json!({
        "health_score": status.get("health_score").cloned().unwrap_or_else(|| json!(0)),
        "warnings": status.get("warnings").cloned().unwrap_or_else(|| json!([])),
        "wiki": wiki_counts,
        "compact": true,
    })
}

fn default_briefing_query(named_project: Option<&str>) -> String {
    match named_project {
        Some(name) => format!("{name} current task recent decisions blockers next steps"),
        None => "current task recent decisions blockers next steps".to_string(),
    }
}

pub(crate) async fn handle_memory_briefing(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let named_project = params
        .project
        .clone()
        .or_else(|| resolve_effective_named_project(server, None));
    let available_projects = if named_project.is_none() {
        list_available_named_projects()
    } else {
        Vec::new()
    };
    let query = params
        .query
        .clone()
        .or_else(|| params.topic.clone())
        .or_else(|| params.title.clone())
        .unwrap_or_else(|| default_briefing_query(named_project.as_deref()));
    let compact = params.compact;
    let top_k = params.top_k.max(1).min(if compact { 6 } else { 12 });
    let include_wiki = !matches!(
        params
            .scope
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("memory")
    );
    let memory_cap = if compact { 6 } else { 12 };
    let wiki_cap = if compact { 3 } else { 5 };
    let kanban_cap = if compact { 3 } else { 5 };
    let checkpoint_cap = if compact { 2 } else { 3 };
    let cross_project_cap = if compact { 3 } else { 5 };
    let verification_cap = if compact { 3 } else { 6 };

    let mem_params = SearchMemoryParams {
        query: query.clone(),
        query_vec: None,
        top_k: top_k.saturating_mul(2).max(top_k),
        path_prefix: params.path_prefix.clone(),
        include_training: params.include_training,
        include_archived: params.include_archived,
        candidates_per_channel: 20,
        mmr_threshold: Some(0.85),
        graph_expand_hops: 1,
        graph_relation_filter: None,
        weights: None,
        context_symbols: Vec::new(),
        agent_role: None,
        project: named_project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        enable_rerank: params.enable_rerank,
        as_of: params.as_of.clone(),
        include_metadata: false,
    };

    let wiki_params = if include_wiki {
        Some(SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: top_k.min(wiki_cap),
            path_prefix: Some(
                params
                    .path_prefix
                    .clone()
                    .unwrap_or_else(|| "/wiki".to_string()),
            ),
            include_training: params.include_training,
            include_archived: params.include_archived,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            // If the caller did not explicitly target a project, keep wiki
            // search unscoped so the /wiki retrieval path can merge the
            // canonical project:wiki library with the active workspace.
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context.clone(),
            error_context: params.error_context.clone(),
            enable_rerank: false,
            as_of: params.as_of.clone(),
            include_metadata: false,
        })
    } else {
        None
    };

    let (memories_result, wiki_result, cross_project_result) = tokio::join!(
        handle_search_memory(server, mem_params, true),
        async {
            if let Some(wp) = wiki_params {
                search_memory_rows(server, wp, true).await
            } else {
                Ok(vec![])
            }
        },
        async { crate::handoff_ops::list_pending_handoffs_for_briefing(server, cross_project_cap) },
    );

    let memory_rows = parse_evidence_array(memories_result?);
    let memories = slim_memory_rows(if compact {
        apply_compact_relevance_floor(memory_rows)
    } else {
        memory_rows
    });
    let wiki = if include_wiki {
        let mut rows = wiki_result?;
        crate::wiki_ops::filter_user_facing_wiki_rows(&mut rows);
        let wiki_rows = Value::Array(rows);
        slim_memory_rows(if compact {
            apply_compact_relevance_floor(wiki_rows)
        } else {
            wiki_rows
        })
    } else {
        json!([])
    };
    let cross_project = json!(cross_project_result?);

    let (warnings_res, board_res, checkpoints_res, wiki_counts_res) = tokio::join!(
        async {
            if compact {
                Vec::<String>::new()
            } else {
                crate::status_ops::collect_agent_warning_lines(server).await
            }
        },
        crate::dispatch_ops::handle_tachi_board(
            server,
            TachiBoardParams {
                state_filter: Some("active".to_string()),
                limit: Some(top_k.min(kanban_cap)),
                project: named_project.clone(),
                flow_id: None,
            },
        ),
        async {
            if let Some(project_name) = named_project.as_deref() {
                crate::status_ops::list_recent_checkpoint_entries_for_project(
                    project_name,
                    checkpoint_cap,
                )
            } else {
                crate::status_ops::list_recent_checkpoint_entries(server, checkpoint_cap)
            }
        },
        async { crate::wiki_ops::wiki_hygiene_counts(server).await },
    );
    let warnings: Vec<String> = warnings_res;
    let board = slim_kanban(parse_json_or_empty(board_res?));
    let checkpoints = json!(checkpoints_res);
    let verification = crate::verify_ops::recent_verification_summaries(verification_cap);
    let wiki_counts: Value = wiki_counts_res?;
    let mut health_summary = if compact {
        compact_health_summary(server, wiki_counts).await
    } else {
        // Report the REAL health score — the same value tachi_status computes —
        // instead of a placeholder, so the briefing and status surfaces never
        // disagree (this used to hardcode 95/85 and diverged from the actual score).
        // collect_snapshot does blocking SQLite I/O, so run it off the async
        // executor via spawn_blocking.
        let app_home = crate::status_ops::resolve_app_home();
        let global_db = server.global_db_path_buf();
        let project_db = server.project_db_path_buf();
        let health_score = tokio::task::spawn_blocking(move || {
            crate::status_ops::collect_snapshot(&app_home, &global_db, project_db.as_deref())
                .health_score
        })
        .await
        .unwrap_or(0);
        json!({
            "health_score": health_score,
            "warnings": warnings.iter().take(6).cloned().collect::<Vec<_>>(),
            "wiki": wiki_counts,
        })
    };

    // Cross-flow closure debt: surface unclosed loops / stale specs at the
    // session-start surface the agent actually opens, not just the per-flow
    // feature briefing (which only sees the flow already in scope).
    let open_loops = crate::shell_ops::scan_open_loops(8);

    // Component governance for the active workspace (#799): registry-only,
    // never presented as memory-derived current truth.
    let component_governance = crate::component_governance_ops::component_governance_context(
        server,
        named_project.as_deref(),
        None,
    )
    .unwrap_or_else(|e| {
        json!({
            "status": "error",
            "matches": [],
            "note": format!("component governance context unavailable: {e}"),
        })
    });
    // Fold governance warnings into health so compact status remains glanceable.
    if let Some(health_warnings) = health_summary
        .get_mut("warnings")
        .and_then(Value::as_array_mut)
    {
        for line in crate::component_governance_ops::component_governance_warning_lines(
            &component_governance,
        ) {
            if health_warnings.len() >= 8 {
                break;
            }
            if !health_warnings
                .iter()
                .any(|w| w.as_str() == Some(line.as_str()))
            {
                health_warnings.push(json!(line));
            }
        }
    }

    let binding =
        crate::memory_search_ops::library_binding_receipt(server, params.project.as_deref());
    // Surface binding warnings inside health so compact agents cannot miss them.
    // Ensure `warnings` exists even if health summary shape drifts (Gemini #900).
    if let Some(health_obj) = health_summary.as_object_mut() {
        let health_warnings = health_obj
            .entry("warnings".to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut();
        if let Some(health_warnings) = health_warnings {
            if let Some(binding_warnings) = binding.get("warnings").and_then(Value::as_array) {
                for w in binding_warnings {
                    if health_warnings.len() >= 10 {
                        break;
                    }
                    if !health_warnings.contains(w) {
                        health_warnings.push(w.clone());
                    }
                }
            }
        }
    }

    if wants_json(params.format.as_deref()) {
        if compact {
            let mut response = Map::new();
            response.insert("status".to_string(), json!("completed"));
            response.insert("query".to_string(), json!(query));
            response.insert("project".to_string(), json!(named_project));
            response.insert("available_projects".to_string(), json!(available_projects));
            response.insert("binding".to_string(), binding);
            response.insert("health".to_string(), health_summary);
            insert_non_empty_compact_section(&mut response, "memories", memories);
            insert_non_empty_compact_section(&mut response, "wiki", wiki);
            insert_non_empty_compact_section(&mut response, "cross_project", cross_project);
            insert_non_empty_compact_section(&mut response, "verification", json!(verification));
            insert_non_empty_compact_section(&mut response, "kanban", board);
            insert_non_empty_compact_section(&mut response, "open_loops", json!(open_loops));
            insert_non_empty_compact_section(&mut response, "recent_checkpoints", checkpoints);
            if component_governance
                .get("matches")
                .and_then(Value::as_array)
                .is_some_and(|m| !m.is_empty())
            {
                response.insert("component_governance".to_string(), component_governance);
            }
            response.insert("compact".to_string(), json!(true));
            return json_string(&Value::Object(response));
        }
        return json_string(&json!({
            "status": "completed",
            "query": query,
            "project": named_project,
            "available_projects": available_projects,
            "binding": binding,
            "memories": memories,
            "wiki": wiki,
            "cross_project": cross_project,
            "health": health_summary,
            "verification": verification,
            "kanban": board,
            "open_loops": open_loops,
            "recent_checkpoints": checkpoints,
            "component_governance": component_governance,
            "layer_authority": {
                "docs_specs": "highest; use tachi_task(action='briefing') for feature-scoped canonical docs/specs",
                "guide_sop": "high; procedural workflow guidance",
                "wiki": "medium-high; synthesized durable knowledge",
                "memory": "low-medium; fragmented decisions/checkpoints/evidence",
                "eval_verification": "evidence; supports routing/review but does not override docs/specs",
                "kanban": "workflow state; current task board and dispatch ledger",
                "component_governance": "governance registry; stale/unknown are not memory truth"
            },
            "compact": compact,
            "limits": {
                "memories": memory_cap,
                "wiki": wiki_cap,
                "kanban": kanban_cap,
                "checkpoints": checkpoint_cap,
                "cross_project": cross_project_cap,
                "verification": verification_cap,
            },
        }));
    }

    let mut markdown = agent_markdown::format_briefing(
        &query,
        named_project.as_deref(),
        &memories,
        &wiki,
        &cross_project,
        &health_summary,
        &verification,
        &board,
        &checkpoints,
        &open_loops,
        &component_governance,
        compact,
    );
    // Insert binding receipt after the project-focus line so unscoped sessions are obvious.
    let binding_md = crate::memory_search_ops::format_binding_markdown(&binding);
    if let Some(idx) = markdown.find('\n') {
        markdown.insert_str(idx + 1, &format!("{binding_md}\n"));
    } else {
        markdown.push('\n');
        markdown.push_str(&binding_md);
    }
    Ok(markdown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_relevance_floor_drops_low_scores_and_keeps_unscored_rows() {
        let rows = json!([
            {"id": "low", "relevance": 0.24},
            {"id": "floor", "relevance": BRIEFING_COMPACT_RELEVANCE_FLOOR},
            {"id": "score-low", "score": {"final": 0.10}},
            {"id": "score-high", "score": 0.80},
            {"id": "unscored", "summary": "no relevance"}
        ]);

        let filtered = apply_compact_relevance_floor(rows);
        let ids = filtered
            .as_array()
            .expect("filtered rows")
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["floor", "score-high", "unscored"]);
    }

    #[test]
    fn compact_sections_treat_null_as_empty() {
        let mut response = Map::new();

        insert_non_empty_compact_section(&mut response, "empty_null", Value::Null);
        insert_non_empty_compact_section(&mut response, "non_empty", json!({"count": 1}));

        assert!(!response.contains_key("empty_null"));
        assert!(response.contains_key("non_empty"));
    }
}
