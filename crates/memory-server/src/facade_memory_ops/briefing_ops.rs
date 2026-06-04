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
use crate::memory_search_ops::{handle_search_memory, search_memory_rows};
use crate::memory_search_ops::{named_project_db_exists, resolve_workspace_named_project};
use crate::tool_params::*;
use crate::MemoryServer;
use serde_json::json;

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
        .or_else(|| resolve_workspace_named_project().filter(|name| named_project_db_exists(name)));
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

    let mem_params = SearchMemoryParams {
        query: query.clone(),
        query_vec: None,
        top_k: top_k.saturating_mul(2).max(top_k),
        path_prefix: params.path_prefix.clone(),
        include_archived: params.include_archived,
        candidates_per_channel: 20,
        mmr_threshold: Some(0.85),
        graph_expand_hops: 1,
        graph_relation_filter: None,
        weights: None,
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
            include_archived: params.include_archived,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
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

    let memories = slim_memory_rows(parse_evidence_array(memories_result?));
    let wiki = if include_wiki {
        let mut rows = wiki_result?;
        crate::wiki_ops::filter_user_facing_wiki_rows(&mut rows);
        slim_memory_rows(serde_json::Value::Array(rows))
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
                state_filter: Some("all".to_string()),
                limit: Some(top_k.min(kanban_cap)),
                project: named_project.clone(),
            },
        ),
        async { crate::status_ops::list_recent_checkpoint_entries(server, checkpoint_cap) },
        async {
            if compact {
                Ok(json!({"orphans":0,"stale_nodes":0,"duplicates":0}))
            } else {
                crate::wiki_ops::wiki_hygiene_counts(server).await
            }
        },
    );
    let warnings: Vec<String> = warnings_res;
    let board = slim_kanban(parse_json_or_empty(board_res?));
    let checkpoints = json!(checkpoints_res);
    let wiki_counts: serde_json::Value = wiki_counts_res?;
    let health_summary = if compact {
        json!({"health_score": 95, "warnings": [], "wiki": wiki_counts, "compact": true})
    } else {
        json!({
            "health_score": if warnings.is_empty() { 95 } else { 85 },
            "warnings": warnings.iter().take(6).cloned().collect::<Vec<_>>(),
            "wiki": wiki_counts,
        })
    };

    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "completed",
            "query": query,
            "project": named_project,
            "memories": memories,
            "wiki": wiki,
            "cross_project": cross_project,
            "health": health_summary,
            "kanban": board,
            "recent_checkpoints": checkpoints,
            "compact": compact,
            "limits": {
                "memories": memory_cap,
                "wiki": wiki_cap,
                "kanban": kanban_cap,
                "checkpoints": checkpoint_cap,
                "cross_project": cross_project_cap,
            },
        }));
    }

    Ok(agent_markdown::format_briefing(
        &query,
        named_project.as_deref(),
        &memories,
        &wiki,
        &cross_project,
        &health_summary,
        &board,
        &checkpoints,
        compact,
    ))
}
