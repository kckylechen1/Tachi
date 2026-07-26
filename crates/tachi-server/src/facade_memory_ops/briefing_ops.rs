//! Briefing handler for `tachi_memory(action="briefing")`.
//!
//! Memories/wiki are **workspace-scoped** (`project_only` search).
//!
//! #1099: the "Cross-project (global handoffs)" section that used to live
//! here (`crate::handoff_ops::list_pending_handoffs_for_briefing`) is
//! retired along with `handoff_ops`'s write path — see `handoff_ops.rs`'s
//! module doc for the caller-sweep evidence and legacy-row data policy.
//! Cross-session coordination now goes through the `stickies` section
//! (`sticky_ops`, #964) below.

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
            let carries_incomplete_evidence = map
                .get("incomplete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || map
                    .get("warning")
                    .and_then(Value::as_str)
                    .is_some_and(|warning| !warning.is_empty())
                || map
                    .get("incomplete_reasons")
                    .and_then(Value::as_array)
                    .is_some_and(|reasons| !reasons.is_empty());
            !carries_incomplete_evidence
                && (map.is_empty()
                    || map
                        .get("count")
                        .and_then(Value::as_u64)
                        .is_some_and(|count| count == 0)
                    || map
                        .get("tasks")
                        .and_then(Value::as_array)
                        .is_some_and(|tasks| tasks.is_empty()))
        }
        _ => false,
    }
}

fn insert_non_empty_compact_section(map: &mut Map<String, Value>, key: &str, value: Value) {
    if !compact_section_is_empty(&value) {
        map.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod compact_section_tests {
    use super::*;

    #[test]
    fn incomplete_empty_kanban_is_not_omitted_from_compact_briefing() {
        let board = json!({
            "count": 0,
            "tasks": [],
            "incomplete": true,
            "warning": "bounded fallback is incomplete",
            "incomplete_reasons": ["run_fallback_scan_truncated"],
        });

        assert!(
            !compact_section_is_empty(&board),
            "incomplete board evidence must survive compact briefing shaping"
        );
    }
}

async fn compact_health_summary(server: &MemoryServer, wiki_counts: Value) -> Value {
    // tachi#1201 k3: search_memory/tachi_status now default to markdown when
    // `format` is omitted; this internal consumer parses the body as JSON,
    // so it must opt in explicitly to keep this call's shape unchanged.
    let status = crate::status_ops::handle_tachi_status_agent(server, Some("json"))
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
        list_available_named_projects(&server.tachi_home_dir())
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
        // tachi#1201 k3: search_memory now defaults to markdown when `format`
        // is omitted; this internal consumer parses the body as JSON via
        // `handle_search_memory` below, so it must opt in explicitly.
        format: Some("json".to_string()),
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
            // Goes through search_memory_rows (Vec<Value>, no string
            // round trip) below, so format is compile-only here.
            format: None,
        })
    } else {
        None
    };

    let sticky_cap = if compact { 3 } else { 5 };
    let (memories_result, wiki_result, sticky_result) = tokio::join!(
        handle_search_memory(server, mem_params, true),
        async {
            if let Some(wp) = wiki_params {
                search_memory_rows(server, wp, true).await
            } else {
                Ok(vec![])
            }
        },
        // #964: unread stickies for the caller. A caller with no seat
        // identity (agent_id absent) is treated as leader (frozen semantics
        // #3/#4) — worker seats only see stickies explicitly addressed to
        // their seat name. Inclusion here IS the read: each row returned is
        // atomically claimed (read-once) as a side effect.
        //
        // CP2: identity is resolved server-side (params.agent_id ->
        // agent_profile -> TACHI_AGENT_SEAT env -> leader), NOT trusted from
        // params.agent_id alone — an unauthenticated/param-less worker
        // briefing call must not be silently treated as the leader and
        // consume broadcast (`to`-absent) stickies meant for the real
        // leader. See sticky_ops::identity::resolve_caller_agent_id.
        async {
            let resolved_agent_id =
                crate::sticky_ops::resolve_caller_agent_id(server, params.agent_id.as_deref());
            crate::sticky_ops::claim_unread_stickies_for_briefing(
                server,
                resolved_agent_id.as_deref(),
                sticky_cap,
            )
        },
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
        // #1072 fix-round (#1215 BUG 4): `tachi_memory(briefing)` used to
        // serve raw `/wiki` search rows — including unreviewed
        // `pending_review` drafts — straight into the briefing response with
        // no lifecycle filtering, another reasoning-context bypass the
        // cross-vendor review named explicitly. Briefing has no explicit
        // lifecycle-scope param and should not grow one (it is a read
        // surface, not an authoring one) — always gate to the default
        // (active-only) scope.
        crate::wiki_ops::apply_wiki_lifecycle_gate(
            server,
            params.project.as_deref(),
            &mut rows,
            None,
        )?;
        let wiki_rows = Value::Array(rows);
        slim_memory_rows(if compact {
            apply_compact_relevance_floor(wiki_rows)
        } else {
            wiki_rows
        })
    } else {
        json!([])
    };
    let stickies = json!(sticky_result?);

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
                verbose: None,
            },
        ),
        async {
            if let Some(project_name) = named_project.as_deref() {
                crate::status_ops::list_recent_checkpoint_entries_for_project(
                    server,
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
        let app_home = server.tachi_home_dir();
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

    // Issue freshness (#1000): zombie (fixed-but-open) + stale-candidate
    // queues, projected from already-scanned review-candidate rows
    // (state_kv). This reads only — population happens via
    // `tachi_gh(action='issue_freshness_scan')` + candidate save, kept out
    // of the briefing hot path.
    let issue_freshness = crate::gh_ops::briefing_freshness_queues(server, 5);

    // Presence 工位表 (#1001): read-only, failure-safe projection of live
    // session claims + advisory collision warnings against any explicit
    // issue_ref this briefing call was scoped to. Never fails briefing.
    // Single call point (Scope item 3) — see `claims_ops::presence_briefing_section`.
    let presence_section =
        crate::claims_ops::presence_briefing_section(server, params.issue_ref.as_deref());
    let presence_board = presence_section["board"].clone();
    let presence_warnings: Vec<String> = presence_section["warnings"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

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
            insert_non_empty_compact_section(&mut response, "stickies", stickies.clone());
            insert_non_empty_compact_section(&mut response, "memories", memories);
            insert_non_empty_compact_section(&mut response, "wiki", wiki);
            insert_non_empty_compact_section(&mut response, "verification", json!(verification));
            insert_non_empty_compact_section(&mut response, "kanban", board);
            insert_non_empty_compact_section(&mut response, "open_loops", json!(open_loops));
            if issue_freshness["zombies"]["count"].as_u64().unwrap_or(0) > 0
                || issue_freshness["stale_candidates"]["count"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
            {
                response.insert("issue_freshness".to_string(), issue_freshness.clone());
            }
            if presence_board["count"].as_u64().unwrap_or(0) > 0 || !presence_warnings.is_empty() {
                response.insert(
                    "presence".to_string(),
                    json!({ "board": presence_board.clone(), "warnings": presence_warnings.clone() }),
                );
            }
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
            "stickies": stickies,
            "memories": memories,
            "wiki": wiki,
            "health": health_summary,
            "verification": verification,
            "kanban": board,
            "open_loops": open_loops,
            "issue_freshness": issue_freshness,
            "presence": {
                "board": presence_board,
                "warnings": presence_warnings,
            },
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
                "verification": verification_cap,
            },
        }));
    }

    let presence_for_markdown = json!({
        "board": presence_board,
        "warnings": presence_warnings,
    });
    let mut markdown = agent_markdown::format_briefing(
        &query,
        named_project.as_deref(),
        &stickies,
        &memories,
        &wiki,
        &health_summary,
        &verification,
        &board,
        &checkpoints,
        &open_loops,
        &component_governance,
        &issue_freshness,
        &presence_for_markdown,
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
