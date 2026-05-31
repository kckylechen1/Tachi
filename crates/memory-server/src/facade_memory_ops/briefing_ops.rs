//! Briefing handler for `tachi_memory(action="briefing")`.

use super::evidence_format::{
    json_string, parse_evidence_array, parse_json_or_empty, slim_kanban, slim_memory_rows,
    wants_json,
};
use crate::agent_markdown;
use crate::memory_search_ops::{handle_search_memory, search_memory_rows};
use crate::tool_params::*;
use crate::MemoryServer;
use serde_json::json;

pub(crate) async fn handle_memory_briefing(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let query = params
        .query
        .clone()
        .or_else(|| params.topic.clone())
        .or_else(|| params.title.clone())
        .unwrap_or_else(|| "current task recent decisions blockers next steps".to_string());
    let top_k = params.top_k.max(1).min(12);
    let include_wiki = !matches!(
        params
            .scope
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("memory")
    );

    // Memory + wiki searches run concurrently to reduce briefing latency.
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
        project: params.project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        enable_rerank: params.enable_rerank,
        as_of: params.as_of.clone(),
    };

    let wiki_params = if include_wiki {
        Some(SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: top_k.min(5),
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
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context.clone(),
            error_context: params.error_context.clone(),
            enable_rerank: false,
            as_of: params.as_of.clone(),
        })
    } else {
        None
    };

    let (memories_result, wiki_result) =
        tokio::join!(handle_search_memory(server, mem_params), async {
            if let Some(wp) = wiki_params {
                search_memory_rows(server, wp).await
            } else {
                Ok(vec![])
            }
        });

    let memories = slim_memory_rows(parse_evidence_array(memories_result?));
    let wiki = if include_wiki {
        slim_memory_rows(serde_json::Value::Array(wiki_result?))
    } else {
        json!([])
    };

    let (warnings_res, board_res, checkpoints_res, wiki_counts_res) = tokio::join!(
        crate::status_ops::collect_agent_warning_lines(server),
        crate::dispatch_ops::handle_tachi_board(
            server,
            TachiBoardParams {
                state_filter: Some("all".to_string()),
                limit: Some(top_k.min(5)),
                project: params.project.clone(),
            },
        ),
        async { crate::status_ops::list_recent_checkpoint_entries(server, 3) },
        crate::wiki_ops::wiki_hygiene_counts(server),
    );
    let warnings = warnings_res;
    let board = slim_kanban(parse_json_or_empty(board_res?));
    let checkpoints = json!(checkpoints_res);
    let wiki_counts = wiki_counts_res?;
    let health_summary = json!({
        "health_score": if warnings.is_empty() { 95 } else { 85 },
        "warnings": warnings.iter().take(6).cloned().collect::<Vec<_>>(),
        "wiki": wiki_counts,
    });

    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "completed",
            "query": query,
            "memories": memories,
            "wiki": wiki,
            "health": health_summary,
            "kanban": board,
            "recent_checkpoints": checkpoints,
        }));
    }

    Ok(agent_markdown::format_briefing(
        &query,
        &memories,
        &wiki,
        &health_summary,
        &board,
        &checkpoints,
    ))
}
