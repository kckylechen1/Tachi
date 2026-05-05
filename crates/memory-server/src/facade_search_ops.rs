//! Business logic for the `tachi_search` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_search`].

use crate::copilot_ops::handle_tachi_wiki_search;
use crate::memory_search_ops::handle_search_memory;
use crate::tool_params::*;
use crate::MemoryServer;

pub(crate) async fn handle_tachi_search(
    server: &MemoryServer,
    params: TachiSearchParams,
) -> Result<String, String> {
    let scope = params.scope.to_ascii_lowercase();
    let mut parts = Vec::new();

    // Normalize scope: "wiki", "memory", "all" are the valid subsystem selectors.
    // "project", "global", "user" are DB-target hints that callers sometimes pass
    // by analogy with save_memory's scope parameter. Treat them as "all".
    let effective_scope = match scope.as_str() {
        "wiki" | "memory" | "all" => scope.as_str(),
        _ => "all",
    };
    let scope_remapped = effective_scope != scope.as_str();

    if effective_scope == "wiki" || effective_scope == "all" {
        let wiki_params = WikiSearchParams {
            query: params.query.clone(),
            path_prefix: params.path_prefix.clone(),
            category: params.category.clone(),
            top_k: params.top_k,
            include_archived: params.include_archived,
            agent_role: None,
            project: params.project.clone(),
            domain: params.domain.clone(),
            weights: None,
        };
        let wiki_result = handle_tachi_wiki_search(server, wiki_params).await;
        match wiki_result {
            Ok(r) => parts.push(format!("## Wiki results\n{}", r)),
            Err(e) => parts.push(format!("## Wiki results\nError: {e}")),
        }
    }

    if effective_scope == "memory" || effective_scope == "all" {
        let mem_params = SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: params.top_k,
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
            enable_rerank: params.enable_rerank,
        };
        let mem_result = handle_search_memory(server, mem_params).await;
        match mem_result {
            Ok(r) => parts.push(format!("## Memory results\n{}", r)),
            Err(e) => parts.push(format!("## Memory results\nError: {e}")),
        }
    }

    let mut output = parts.join("\n\n");
    if scope_remapped {
        output = format!(
            "> **Note**: scope='{}' was interpreted as 'all' (search both wiki and memory). \
             The `scope` parameter selects *which subsystems* to search (wiki/memory/all), \
             not which DB. Use the `project` parameter to target a specific project DB.\n\n{}",
            scope, output
        );
    }

    Ok(output)
}
