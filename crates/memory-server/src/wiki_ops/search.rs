use super::*;

// ─── Wiki Search ────────────────────────────────────────────────────────────

/// Wiki category prefixes for quick lookup. Resolves short names to full paths.
fn resolve_wiki_category(category: &str) -> String {
    let trimmed = category.trim().trim_start_matches('/');
    // Already a full wiki path
    if trimmed.starts_with("wiki/") || trimmed.starts_with("wiki\\") {
        return format!("/{}", trimmed);
    }
    // Short alias → full path
    match trimmed.to_ascii_lowercase().as_str() {
        "quant" | "trading" => "/wiki/quant".to_string(),
        "quant/strategy" | "strategy" => "/wiki/quant/strategy".to_string(),
        "quant/stock-analysis" | "stock-analysis" | "stock" => {
            "/wiki/quant/stock-analysis".to_string()
        }
        "quant/portfolio" | "portfolio" => "/wiki/quant/portfolio".to_string(),
        "quant/market-analysis" | "market-analysis" | "market" => {
            "/wiki/quant/market-analysis".to_string()
        }
        "quant/data-pipeline" | "data-pipeline" | "data" => "/wiki/quant/data-pipeline".to_string(),
        "quant/autoresearch" | "autoresearch" => "/wiki/quant/autoresearch".to_string(),
        "engineering" | "eng" | "code" | "coding" => "/wiki/engineering".to_string(),
        "engineering/architecture" | "architecture" | "arch" => {
            "/wiki/engineering/architecture".to_string()
        }
        "engineering/devops" | "devops" => "/wiki/engineering/devops".to_string(),
        "engineering/debugging" | "debugging" | "debug" => {
            "/wiki/engineering/debugging".to_string()
        }
        "engineering/code-review" | "code-review" | "review" => {
            "/wiki/engineering/code-review".to_string()
        }
        "agent" => "/wiki/agent".to_string(),
        "agent/tachi" | "tachi" => "/wiki/agent/tachi".to_string(),
        "agent/openclaw" | "openclaw" => "/wiki/agent/openclaw".to_string(),
        "agent/evolution" | "evolution" => "/wiki/agent/evolution".to_string(),
        "product" => "/wiki/product".to_string(),
        "product/hyperion" | "hyperion" => "/wiki/product/hyperion".to_string(),
        "product/crimson-alphard" | "crimson-alphard" | "crimson" => {
            "/wiki/product/crimson-alphard".to_string()
        }
        "misc" => "/wiki/misc".to_string(),
        other => format!("/wiki/{}", other),
    }
}

/// All known wiki top-level categories for browse stats.
const WIKI_CATEGORIES: &[&str] = &[
    "/wiki/quant/strategy",
    "/wiki/quant/stock-analysis",
    "/wiki/quant/portfolio",
    "/wiki/quant/market-analysis",
    "/wiki/quant/data-pipeline",
    "/wiki/quant/autoresearch",
    "/wiki/engineering/architecture",
    "/wiki/engineering/devops",
    "/wiki/engineering/debugging",
    "/wiki/engineering/code-review",
    "/wiki/agent/tachi",
    "/wiki/agent/openclaw",
    "/wiki/agent/evolution",
    "/wiki/product/hyperion",
    "/wiki/product/crimson-alphard",
    "/wiki/misc",
];

pub(crate) async fn handle_wiki_search(
    server: &MemoryServer,
    params: WikiSearchParams,
) -> Result<String, String> {
    let value = collect_wiki_search_value(server, params).await?;
    if value
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "skipped")
    {
        return Ok("## Wiki search\n\n_Skipped: empty query._".to_string());
    }
    let empty_results = Value::Array(Vec::new());
    Ok(crate::agent_markdown::format_wiki_search(
        value.get("query").and_then(Value::as_str).unwrap_or(""),
        value.get("count").and_then(Value::as_u64).unwrap_or(0) as usize,
        value.get("results").unwrap_or(&empty_results),
    ))
}

pub(crate) async fn collect_wiki_search_value(
    server: &MemoryServer,
    params: WikiSearchParams,
) -> Result<Value, String> {
    if params.query.trim().is_empty() {
        return Ok(json!({
            "status": "skipped",
            "query": params.query,
            "reason": "empty_query",
            "count": 0,
            "results": [],
        }));
    }

    let query = params.query.clone();
    let path_prefix = params
        .category
        .as_deref()
        .map(resolve_wiki_category)
        .or_else(|| params.path_prefix.clone())
        .or_else(|| Some("/wiki".to_string()));
    let mut rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: params.top_k.max(1).min(50),
            path_prefix: path_prefix.clone(),
            include_training: false,
            include_archived: params.include_archived,
            candidates_per_channel: params.top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: params.weights.or(Some(HybridWeightsParam {
                semantic: 0.48,
                fts: 0.30,
                symbolic: 0.20,
                decay: 0.02,
                use_rrf: true,
            })),
            context_symbols: Vec::new(),
            agent_role: params.agent_role,
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context,
            error_context: params.error_context,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    filter_user_facing_wiki_rows(&mut rows);
    let unfiltered_count = rows.len();
    rows.retain(wiki_row_has_direct_match_signal);

    append_wiki_log(
        server,
        "search",
        &format!("{} | {} result(s)", query, rows.len()),
    );

    Ok(json!({
        "status": "completed",
        "query": query,
        "path_prefix": path_prefix,
        "project": params.project,
        "domain": params.domain,
        "unfiltered_count": unfiltered_count,
        "count": rows.len(),
        "results": rows,
    }))
}

pub(super) fn wiki_row_has_direct_match_signal(row: &Value) -> bool {
    let score = row.get("score").and_then(Value::as_object);
    let fts = score
        .and_then(|score| score.get("fts"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let symbolic = score
        .and_then(|score| score.get("symbolic"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    fts > 0.0 || symbolic > 0.0
}

pub(crate) fn handle_wiki_browse(
    server: &MemoryServer,
    params: WikiBrowseParams,
) -> Result<String, String> {
    let value = collect_wiki_browse_value(server, params)?;

    if value
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "category")
    {
        let path = value.get("path").and_then(Value::as_str).unwrap_or("/wiki");
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        return Ok(crate::agent_markdown::format_wiki_browse_category(
            path, entries,
        ));
    }

    let categories = value
        .get("categories")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some((
                        row.get("path")?.as_str()?.to_string(),
                        row.get("count")?.as_u64()? as usize,
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(crate::agent_markdown::format_wiki_browse_stats(
        value.get("total").and_then(Value::as_u64).unwrap_or(0) as usize,
        &categories,
    ))
}

pub(crate) fn collect_wiki_browse_value(
    server: &MemoryServer,
    params: WikiBrowseParams,
) -> Result<Value, String> {
    let project_name = params.project;

    match params.category.as_deref() {
        None | Some("") => {
            let mut categories = Vec::new();
            let mut total = 0usize;
            let all_entries =
                list_related_candidates(server, &project_name, 5000).unwrap_or_default();

            for &cat_path in WIKI_CATEGORIES {
                let cat_prefix = format!("{cat_path}/");
                let count = all_entries
                    .iter()
                    .filter(|entry| entry.path == cat_path || entry.path.starts_with(&cat_prefix))
                    .count();
                if count > 0 {
                    categories.push(json!({
                        "path": cat_path,
                        "count": count,
                    }));
                    total += count;
                }
            }

            append_wiki_log(
                server,
                "browse",
                &format!("stats | {} categor(ies), {} total", categories.len(), total),
            );

            Ok(json!({
                "status": "completed",
                "kind": "stats",
                "project": project_name,
                "total": total,
                "categories": categories,
            }))
        }
        Some(category) => {
            let resolved_path = resolve_wiki_category(category);
            let limit = params.limit.max(1).min(500);

            let (entries, _) = list_wiki_entries(server, &project_name, 5000)?;
            let resolved_prefix = format!("{resolved_path}/");
            let slim_entries: Vec<Value> = entries
                .into_iter()
                .filter(|entry| {
                    entry.path == resolved_path || entry.path.starts_with(&resolved_prefix)
                })
                .take(limit)
                .map(|entry| {
                    json!({
                        "path": entry.path,
                        "summary": entry.summary,
                        "importance": entry.importance,
                    })
                })
                .collect();

            append_wiki_log(
                server,
                "browse",
                &format!("{} | {} entry(s)", resolved_path, slim_entries.len()),
            );

            Ok(json!({
                "status": "completed",
                "kind": "category",
                "project": project_name,
                "path": resolved_path,
                "count": slim_entries.len(),
                "entries": slim_entries,
            }))
        }
    }
}

// ─── Wiki Read ──────────────────────────────────────────────────────────────

pub(crate) fn handle_wiki_read(
    server: &MemoryServer,
    path: &str,
    project: &str,
) -> Result<String, String> {
    let value = collect_wiki_read_value(server, path, project)?;
    if value
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "found")
    {
        return Ok(crate::agent_markdown::format_wiki_read(&value["entry"]));
    }
    if value
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "ambiguous")
    {
        let resolved = value
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_else(|| path.trim());
        let count = value
            .get("candidate_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Ok(format!(
            "## Wiki read\n\n_Ambiguous path `{resolved}` matched {count} entries._\n\nUse `tachi_wiki(action=\"search\")` or read by a more specific path."
        ));
    }
    let resolved = value
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_else(|| path.trim());
    Ok(format!(
        "## Wiki read\n\n_No entry found at `{resolved}`._\n\nUse `tachi_wiki(action=\"search\")` or `tachi_wiki(action=\"browse\")` to find entries."
    ))
}

pub(crate) fn collect_wiki_read_value(
    server: &MemoryServer,
    path: &str,
    project: &str,
) -> Result<Value, String> {
    let resolved = if path.trim().starts_with('/') {
        let trimmed = path.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(
                "Wiki path cannot be root '/' — specify a concrete path like /wiki/my-topic"
                    .to_string(),
            );
        }
        trimmed.to_string()
    } else {
        resolve_wiki_category(path)
    };

    let (entries, _) = list_wiki_entries(server, project, 5000)?;
    let exact_matches = entries
        .iter()
        .filter(|entry| entry.path == resolved)
        .collect::<Vec<_>>();
    if exact_matches.len() > 1 {
        return Ok(json!({
            "status": "ambiguous",
            "project": project,
            "path": resolved,
            "entry": null,
            "candidate_count": exact_matches.len(),
            "candidates": exact_matches
                .into_iter()
                .map(wiki_read_candidate)
                .collect::<Vec<_>>(),
            "next_action": "Use tachi_wiki(action=\"search\") or read by a more specific path.",
        }));
    }
    let entry = exact_matches.into_iter().next().or_else(|| {
        let prefix = format!("{resolved}/");
        entries.iter().find(|e| e.path.starts_with(&prefix))
    });

    match entry {
        Some(entry) => {
            append_wiki_log(server, "read", &resolved);
            Ok(json!({
                "status": "found",
                "project": project,
                "path": resolved,
                "entry": {
                    "path": entry.path,
                    "text": entry.text,
                    "summary": entry.summary,
                    "importance": entry.importance,
                    "keywords": entry.keywords,
                    "entities": entry.entities,
                    "topic": entry.topic,
                    "timestamp": entry.timestamp,
                }
            }))
        }
        None => Ok(json!({
            "status": "not_found",
            "project": project,
            "path": resolved,
            "entry": null,
            "next_action": "Use tachi_wiki(action=\"search\") or tachi_wiki(action=\"browse\") to find entries.",
        })),
    }
}

fn wiki_read_candidate(entry: &memory_core::MemoryEntry) -> Value {
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "importance": entry.importance,
        "keywords": entry.keywords,
        "entities": entry.entities,
        "topic": entry.topic,
        "timestamp": entry.timestamp,
    })
}
