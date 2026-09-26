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

    row.get("path")
        .and_then(Value::as_str)
        .is_some_and(memcore::is_continuity_projection_path)
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

fn parse_memory_rows(raw: String, top_k: usize, path_prefix: Option<&str>) -> Value {
    let Ok(mut rows) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return Value::Array(vec![]);
    };
    rows.retain(|row| {
        let projection_is_explicitly_scoped =
            row.get("path").and_then(Value::as_str).is_some_and(|path| {
                memcore::path_prefix_opts_into_continuity_projection(path, path_prefix)
            });
        !is_wiki_row(row) && (!is_continuity_projection_row(row) || projection_is_explicitly_scoped)
    });
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
    Ok(format_tachi_search_output(
        server,
        &params,
        sections,
        scope_remapped,
        &scope,
    ))
}

pub(crate) async fn handle_tachi_search_with_resources(
    server: &MemoryServer,
    params: TachiSearchParams,
    bound_project: Option<&str>,
) -> Result<(String, Vec<rmcp::model::Resource>), String> {
    let (sections, scope_remapped, scope, links) =
        collect_tachi_search_sections_with_resources(server, &params, bound_project).await;
    Ok((
        format_tachi_search_output(server, &params, sections, scope_remapped, &scope),
        links,
    ))
}

fn format_tachi_search_output(
    server: &MemoryServer,
    params: &TachiSearchParams,
    sections: Vec<(String, Value)>,
    scope_remapped: bool,
    scope: &str,
) -> String {
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
            "> **Note**: scope='{}' was interpreted as 'all'. Use `project` to target a named library under `~/.tachi/projects/<name>/tachi-memory.db`.\n\n{output}",
            scope
        );
    }

    output
}

pub(crate) async fn collect_tachi_search_sections(
    server: &MemoryServer,
    params: &TachiSearchParams,
) -> (Vec<(String, Value)>, bool, String) {
    let (sections, scope_remapped, scope, _) =
        collect_tachi_search_sections_inner(server, params, None).await;
    (sections, scope_remapped, scope)
}

pub(crate) async fn collect_tachi_search_sections_with_resources(
    server: &MemoryServer,
    params: &TachiSearchParams,
    bound_project: Option<&str>,
) -> (
    Vec<(String, Value)>,
    bool,
    String,
    Vec<rmcp::model::Resource>,
) {
    let resource_project = bound_project
        .filter(|_| {
            crate::memory_resources::resource_issuance_allowed(server, params, bound_project)
        })
        .map(str::to_string);
    collect_tachi_search_sections_inner(server, params, resource_project).await
}

async fn collect_tachi_search_sections_inner(
    server: &MemoryServer,
    params: &TachiSearchParams,
    resource_project: Option<String>,
) -> (
    Vec<(String, Value)>,
    bool,
    String,
    Vec<rmcp::model::Resource>,
) {
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let scope = params.scope.to_ascii_lowercase();
    let effective_scope = match scope.as_str() {
        "wiki" | "memory" | "patterns" | "all" | "sft" => scope.as_str(),
        _ => "all",
    };
    let scope_remapped = effective_scope != scope.as_str();
    let mut sections: Vec<(String, Value)> = Vec::new();
    let mut resource_links = Vec::new();

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
        let memory_path_prefix = mem_params.path_prefix.clone();
        if let Some(project_name) = resource_project.as_deref() {
            match crate::memory_search_ops::handle_search_memory_with_resources(
                server,
                mem_params,
                project_name,
                true,
            )
            .await
            {
                Ok((raw, links)) => {
                    let rows = parse_memory_rows(raw, top_k, memory_path_prefix.as_deref());
                    let visible_ids: std::collections::HashSet<&str> = rows
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|row| row.get("id").and_then(Value::as_str))
                        .collect();
                    resource_links.extend(links.into_iter().filter(|link| {
                        crate::memory_resources::parse_resource_uri(&link.uri)
                            .is_some_and(|reference| visible_ids.contains(reference.id.as_str()))
                    }));
                    sections.push(("Memory".to_string(), rows));
                }
                Err(e) => {
                    sections.push(("Memory".to_string(), Value::String(format!("Error: {e}"))))
                }
            }
        } else {
            match handle_search_memory_with_access(server, mem_params, false, true).await {
                Ok(raw) => sections.push((
                    "Memory".to_string(),
                    parse_memory_rows(raw, top_k, memory_path_prefix.as_deref()),
                )),
                Err(e) => {
                    sections.push(("Memory".to_string(), Value::String(format!("Error: {e}"))))
                }
            }
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
        let plan = WikiReadPlan::from_project(params.project.as_deref());
        match plan {
            Ok(plan) => match crate::wiki_ops::search_wiki_rows_for_plan(
                server,
                wiki_params,
                &plan,
                None,
                true,
            )
            .await
            {
                Ok(result) => sections.push(("Wiki".to_string(), Value::Array(result.rows))),
                Err(e) => sections.push(("Wiki".to_string(), Value::String(format!("Error: {e}")))),
            },
            Err(e) => sections.push(("Wiki".to_string(), Value::String(format!("Error: {e}")))),
        }
    }

    (sections, scope_remapped, scope, resource_links)
}
