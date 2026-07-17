//! Business logic for the `tachi_search` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_search`].

use crate::agent_markdown;
use crate::memory_search_ops::{handle_search_memory_with_access, search_memory_rows_with_access};
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

fn metadata_projection_kind(row: &Value) -> Option<&str> {
    row.get("metadata")
        .and_then(|metadata| metadata.get("projection_kind"))
        .and_then(Value::as_str)
}

fn is_continuity_projection_row(row: &Value) -> bool {
    if metadata_projection_kind(row).is_some() {
        return true;
    }

    row.get("path").and_then(Value::as_str).is_some_and(|path| {
        path == "/lorebook"
            || path.starts_with("/lorebook/")
            || path == "/user/patterns"
            || path.starts_with("/user/patterns/")
            || path == "/user/affect"
            || path.starts_with("/user/affect/")
            || path == "/timeline"
            || path.starts_with("/timeline/")
            || path == "/outcomes"
            || path.starts_with("/outcomes/")
            || path == "/project-cycle"
            || path.starts_with("/project-cycle/")
            || path == "/domain-profile"
            || path.starts_with("/domain-profile/")
            || path == "/evidence-gates"
            || path.starts_with("/evidence-gates/")
    })
}

fn is_pattern_row(row: &Value) -> bool {
    matches!(
        metadata_projection_kind(row),
        Some("pattern") | Some("bonding")
    ) || row
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path == "/user/patterns" || path.starts_with("/user/patterns/"))
}

fn parse_memory_rows(raw: String, top_k: usize) -> Value {
    let Ok(mut rows) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return Value::Array(vec![]);
    };
    rows.retain(|row| !is_wiki_row(row) && !is_continuity_projection_row(row));
    rows.truncate(top_k);
    Value::Array(rows)
}

fn parse_pattern_rows(mut rows: Vec<Value>, top_k: usize) -> Vec<Value> {
    rows.retain(is_pattern_row);
    rows.truncate(top_k);
    rows
}

fn strip_metadata(mut rows: Vec<Value>) -> Vec<Value> {
    for row in &mut rows {
        crate::continuity_ops::attach_pattern_ref_to_row(row);
        if let Some(object) = row.as_object_mut() {
            object.remove("metadata");
        }
    }
    rows
}

pub(crate) async fn handle_tachi_search(
    server: &MemoryServer,
    params: TachiSearchParams,
) -> Result<String, String> {
    if let Some(body) =
        crate::cli_client::maybe_forward_server_read(server, "tachi_search", &params).await?
    {
        return Ok(body);
    }

    let (sections, scope_remapped, scope) = collect_tachi_search_sections(server, &params).await;
    let binding =
        crate::memory_search_ops::library_binding_receipt(server, params.project.as_deref());
    let binding_md = crate::memory_search_ops::format_binding_markdown(&binding);
    let mut output = agent_markdown::format_search_sections(&params.query, &sections);
    // #898 / #900 follow-up: surface binding on standalone tachi_search markdown
    // so agents not using tachi_memory still see single_db_mode warnings.
    if let Some(idx) = output.find('\n') {
        output.insert_str(idx + 1, &format!("{binding_md}\n"));
    } else {
        output.push('\n');
        output.push_str(&binding_md);
    }
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
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let scope = params.scope.to_ascii_lowercase();
    let effective_scope = match scope.as_str() {
        "wiki" | "memory" | "patterns" | "all" | "sft" => scope.as_str(),
        _ => "all",
    };
    let scope_remapped = effective_scope != scope.as_str();
    let mut sections: Vec<(String, Value)> = Vec::new();

    if effective_scope == "memory" || effective_scope == "all" || effective_scope == "sft" {
        let mem_params = SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: top_k.saturating_mul(3).max(top_k),
            path_prefix: if effective_scope == "sft" {
                Some("/sft".to_string())
            } else {
                params.path_prefix.clone()
            },
            include_training: params.include_training || effective_scope == "sft",
            include_archived: params.include_archived,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            context_symbols: params.context_symbols.clone(),
            agent_role: params.agent_role.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context.clone(),
            error_context: params.error_context.clone(),
            enable_rerank: params.enable_rerank,
            as_of: params.as_of.clone(),
            include_metadata: false,
            // tachi#1201 k3: search_memory now defaults to markdown when
            // `format` is omitted; `parse_memory_rows` below parses the body
            // as JSON (and silently degrades to empty on failure), so this
            // must opt in explicitly.
            format: Some("json".to_string()),
        };
        match handle_search_memory_with_access(server, mem_params, false, true).await {
            Ok(raw) => sections.push(("Memory".to_string(), parse_memory_rows(raw, top_k))),
            Err(e) => sections.push(("Memory".to_string(), Value::String(format!("Error: {e}")))),
        }
    }

    if effective_scope == "patterns" {
        let pattern_params = SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: top_k.saturating_mul(3).max(top_k),
            path_prefix: Some(
                params
                    .path_prefix
                    .clone()
                    .unwrap_or_else(|| "/user/patterns".to_string()),
            ),
            include_training: params.include_training,
            include_archived: params.include_archived,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            context_symbols: params.context_symbols.clone(),
            agent_role: params.agent_role.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context.clone(),
            error_context: params.error_context.clone(),
            enable_rerank: params.enable_rerank,
            as_of: params.as_of.clone(),
            include_metadata: true,
            // Goes through search_memory_rows_with_access (Vec<Value>, no
            // string round trip) below, so format is compile-only here.
            format: None,
        };
        match search_memory_rows_with_access(server, pattern_params, false, true).await {
            Ok(rows) => {
                let rows = parse_pattern_rows(rows, top_k);
                let _feedback = crate::continuity_ops::emit_pattern_seen_events(
                    server,
                    params.project.as_deref(),
                    Some(&params.query),
                    &rows,
                    Some("tachi_search"),
                );
                sections.push(("Patterns".to_string(), Value::Array(strip_metadata(rows))));
            }
            Err(e) => sections.push(("Patterns".to_string(), Value::String(format!("Error: {e}")))),
        }
    }

    if effective_scope == "wiki" || effective_scope == "all" {
        let wiki_path_prefix = match (&params.path_prefix, &params.category) {
            (Some(prefix), _) => prefix.clone(),
            (None, Some(category)) if !category.trim().is_empty() => {
                format!("/wiki/{}", category.trim().trim_start_matches('/'))
            }
            _ => "/wiki".to_string(),
        };
        let wiki_params = SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some(wiki_path_prefix),
            include_training: params.include_training,
            include_archived: params.include_archived,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            context_symbols: params.context_symbols.clone(),
            agent_role: params.agent_role.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context.clone(),
            error_context: params.error_context.clone(),
            enable_rerank: false,
            as_of: params.as_of.clone(),
            include_metadata: false,
            // Goes through search_memory_rows_with_access (Vec<Value>, no
            // string round trip) below, so format is compile-only here.
            format: None,
        };
        match search_memory_rows_with_access(server, wiki_params, false, true).await {
            Ok(mut rows) => {
                // #1072 fix-round (#1215 BUG 4): this "Wiki" section feeds
                // BOTH `tachi_search` and `tachi_memory(action='ask')`'s LLM
                // synthesis (`handle_memory_ask` -> `collect_tachi_search_sections`)
                // — the generic-search bypass the cross-vendor review named
                // explicitly ("generic search... bypass the new gate").
                // Serving an unreviewed draft here means `ask` can answer a
                // question by citing unreviewed model output as truth. Gate
                // to the default (active-only) scope; a lookup failure drops
                // the wiki section rather than serving it unfiltered.
                match crate::wiki_ops::apply_wiki_lifecycle_gate(
                    server,
                    params.project.as_deref(),
                    &mut rows,
                    None,
                ) {
                    Ok(()) => sections.push(("Wiki".to_string(), Value::Array(rows))),
                    Err(e) => sections.push((
                        "Wiki".to_string(),
                        Value::String(format!("Error: wiki lifecycle gate failed: {e}")),
                    )),
                }
            }
            Err(e) => sections.push(("Wiki".to_string(), Value::String(format!("Error: {e}")))),
        }
    }

    (sections, scope_remapped, scope)
}
