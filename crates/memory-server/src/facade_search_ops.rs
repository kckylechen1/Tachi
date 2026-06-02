//! Business logic for the `tachi_search` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_search`].

use crate::agent_markdown;
use crate::memory_search_ops::{handle_search_memory, search_memory_rows};
use crate::tool_params::*;
use crate::MemoryServer;
use serde_json::Value;

fn is_wiki_row(row: &Value) -> bool {
    row.get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path == "/wiki" || path.starts_with("/wiki/"))
        || row
            .get("metadata")
            .and_then(|metadata| metadata.get("wiki"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || row
            .get("domain")
            .and_then(Value::as_str)
            .is_some_and(|domain| domain.eq_ignore_ascii_case("wiki"))
}

fn parse_memory_rows(raw: String, top_k: usize) -> Value {
    let Ok(mut rows) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return Value::Array(vec![]);
    };
    rows.retain(|row| !is_wiki_row(row));
    rows.truncate(top_k);
    Value::Array(rows)
}

pub(crate) async fn handle_tachi_search(
    server: &MemoryServer,
    params: TachiSearchParams,
) -> Result<String, String> {
    let (sections, scope_remapped, scope) = collect_tachi_search_sections(server, &params).await;
    let mut output = agent_markdown::format_search_sections(&params.query, &sections);
    if scope_remapped {
        output = format!(
            "> **Note**: scope='{}' was interpreted as 'all'. Use `project` to target a named library under `~/.tachi/projects/<name>/memory.db`.\n\n{output}",
            scope
        );
    }

    Ok(output)
}

pub(crate) async fn collect_tachi_search_sections(
    server: &MemoryServer,
    params: &TachiSearchParams,
) -> (Vec<(String, Value)>, bool, String) {
    let scope = params.scope.to_ascii_lowercase();
    let effective_scope = match scope.as_str() {
        "wiki" | "memory" | "all" => scope.as_str(),
        _ => "all",
    };
    let scope_remapped = effective_scope != scope.as_str();
    let mut sections: Vec<(String, Value)> = Vec::new();

    if effective_scope == "memory" || effective_scope == "all" {
        let mem_params = SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: params.top_k.saturating_mul(3).max(params.top_k),
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
            include_metadata: false,
        };
        match handle_search_memory(server, mem_params, false).await {
            Ok(raw) => sections.push(("Memory".to_string(), parse_memory_rows(raw, params.top_k))),
            Err(e) => sections.push(("Memory".to_string(), Value::String(format!("Error: {e}")))),
        }
    }

    if effective_scope == "wiki" || effective_scope == "all" {
        let wiki_params = SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: params.top_k,
            path_prefix: Some(
                params
                    .path_prefix
                    .clone()
                    .unwrap_or_else(|| "/wiki".to_string()),
            ),
            include_archived: params.include_archived,
            candidates_per_channel: params.top_k.max(20),
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
            include_metadata: false,
        };
        match search_memory_rows(server, wiki_params, false).await {
            Ok(rows) => sections.push(("Wiki".to_string(), Value::Array(rows))),
            Err(e) => sections.push(("Wiki".to_string(), Value::String(format!("Error: {e}")))),
        }
    }

    (sections, scope_remapped, scope)
}
