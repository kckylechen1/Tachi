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
    let plan = WikiReadPlan::from_project(params.project.as_deref())?;
    if let WikiReadPlan::NamedOnly(StoreRef::NamedProject { project }) = &plan {
        if !crate::memory_search_ops::named_project_db_exists(server, project) {
            return Err(format!("Wiki project '{project}' not found"));
        }
    }
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
            format: None,
        },
        matches!(plan, WikiReadPlan::NamedOnly(_)),
    )
    .await?;
    filter_user_facing_wiki_rows(&mut rows);
    let unfiltered_count = rows.len();
    rows.retain(wiki_row_has_direct_match_signal);
    apply_wiki_lifecycle_gate_for_plan(
        server,
        &plan,
        &mut rows,
        params.lifecycle.as_deref(),
    )?;

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
        "stores": stores_for_wiki_plan(server, &plan),
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

/// #1072 RED case 1 fix: derives a wiki entry's browse-facet "category" from
/// its actual stored path (the parent directory) instead of matching it
/// against a hard-coded category list. A fixture under an unlisted prefix
/// (e.g. `/wiki/newteam/entry`) now produces its own `/wiki/newteam` facet
/// instead of being silently dropped from `browse` stats.
fn wiki_entry_category_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((parent, _leaf)) if !parent.is_empty() => parent.to_string(),
        _ => trimmed.to_string(),
    }
}

pub(crate) fn collect_wiki_browse_value(
    server: &MemoryServer,
    params: WikiBrowseParams,
) -> Result<Value, String> {
    let project_name = params.project;
    let plan = WikiReadPlan::from_project(project_name.as_deref())?;
    let requested_lifecycle = params.lifecycle.as_deref();

    match params.category.as_deref() {
        None | Some("") => {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut total = 0usize;
            let all_entries = list_wiki_entries_for_plan(server, &plan, "/wiki", 5000)?;

            for stored in &all_entries {
                let entry = &stored.entry;
                if !wiki_entry_matches_lifecycle_scope(entry, requested_lifecycle)? {
                    continue;
                }
                let category = wiki_entry_category_path(&entry.path);
                *counts.entry(category).or_insert(0) += 1;
                total += 1;
            }
            let categories: Vec<Value> = counts
                .into_iter()
                .map(|(path, count)| json!({ "path": path, "count": count }))
                .collect();

            append_wiki_log(
                server,
                "browse",
                &format!("stats | {} categor(ies), {} total", categories.len(), total),
            );

            Ok(json!({
                "status": "completed",
                "kind": "stats",
                "project": project_name,
                "stores": stores_for_wiki_plan(server, &plan),
                "total": total,
                "categories": categories,
            }))
        }
        Some(category) => {
            let resolved_path = resolve_wiki_category(category);
            let limit = params.limit.max(1).min(500);

            let entries = list_wiki_entries_for_plan(server, &plan, "/wiki", 5000)?;
            let resolved_prefix = format!("{resolved_path}/");
            let mut slim_entries: Vec<Value> = Vec::new();
            for stored in entries {
                let entry = stored.entry;
                if !(entry.path == resolved_path || entry.path.starts_with(&resolved_prefix)) {
                    continue;
                }
                if !wiki_entry_matches_lifecycle_scope(&entry, requested_lifecycle)? {
                    continue;
                }
                if slim_entries.len() >= limit {
                    break;
                }
                let lifecycle = derive_wiki_lifecycle(&entry.metadata, &entry.path);
                let authority = derive_wiki_authority(&entry.metadata);
                // #1072 fix-round (#1215 BUG 6): browse-category provenance
                // was overclaimed — the PR description said read/search
                // "expose ... revision, source refs, ... review receipt" but
                // this branch (unlike `collect_wiki_search_value`'s
                // `attach_wiki_provenance`) dropped id/revision/references/
                // review_receipt entirely. Bring it to parity.
                let review_receipt = derive_wiki_review_receipt(&entry.metadata)
                    .and_then(|receipt| serde_json::to_value(receipt).ok())
                    .unwrap_or(Value::Null);
                slim_entries.push(json!({
                    "id": entry.id,
                    "path": entry.path,
                    "summary": entry.summary,
                    "importance": entry.importance,
                    "revision": entry.revision,
                    "lifecycle": lifecycle.as_str(),
                    "authority": authority.as_str(),
                    "references": preferred_wiki_references(&entry.metadata),
                    "review_receipt": review_receipt,
                    "store": stored.store,
                }));
            }

            append_wiki_log(
                server,
                "browse",
                &format!("{} | {} entry(s)", resolved_path, slim_entries.len()),
            );

            Ok(json!({
                "status": "completed",
                "kind": "category",
                "project": project_name,
                "stores": stores_for_wiki_plan(server, &plan),
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
    let plan = legacy_wiki_read_plan(project);
    handle_wiki_read_for_plan(server, path, &plan)
}

pub(crate) fn handle_wiki_read_for_plan(
    server: &MemoryServer,
    path: &str,
    plan: &WikiReadPlan,
) -> Result<String, String> {
    let value = collect_wiki_read_value_for_plan(server, path, plan)?;
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
    let plan = legacy_wiki_read_plan(project);
    collect_wiki_read_value_for_plan(server, path, &plan)
}

fn legacy_wiki_read_plan(project: &str) -> WikiReadPlan {
    if project == LOGICAL_SHARED_WIKI_PROJECT {
        WikiReadPlan::Federated
    } else {
        WikiReadPlan::NamedOnly(StoreRef::named(project))
    }
}

pub(crate) fn collect_wiki_read_value_for_plan(
    server: &MemoryServer,
    path: &str,
    plan: &WikiReadPlan,
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

    let entries = list_wiki_entries_for_plan(server, plan, "/wiki", 5000)?;
    let exact_matches = entries
        .iter()
        .filter(|entry| entry.entry.path == resolved)
        .collect::<Vec<_>>();
    if exact_matches.len() > 1 {
        return Ok(json!({
            "status": "ambiguous",
            "stores": stores_for_wiki_plan(server, plan),
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
        entries
            .iter()
            .find(|entry| entry.entry.path.starts_with(&prefix))
    });

    match entry {
        Some(stored) => {
            let entry = &stored.entry;
            append_wiki_log(server, "read", &resolved);
            // #1072 RED case 3: expose id/revision/authority/lifecycle/
            // source refs/typed evidence refs/review receipt on read, not
            // just search — a direct read of a `pending_review` path must
            // still show the caller it is not reviewed truth, even though
            // reading by an exact known path (unlike default search) is not
            // itself gated.
            let lifecycle = derive_wiki_lifecycle(&entry.metadata, &entry.path);
            let authority = derive_wiki_authority(&entry.metadata);
            let review_receipt = derive_wiki_review_receipt(&entry.metadata)
                .and_then(|receipt| serde_json::to_value(receipt).ok())
                .unwrap_or(Value::Null);
            Ok(json!({
                "status": "found",
                "stores": stores_for_wiki_plan(server, plan),
                "path": resolved,
                "entry": {
                    "id": entry.id,
                    "path": entry.path,
                    "text": entry.text,
                    "summary": entry.summary,
                    "importance": entry.importance,
                    "keywords": entry.keywords,
                    "entities": entry.entities,
                    "topic": entry.topic,
                    "timestamp": entry.timestamp,
                    "revision": entry.revision,
                    "authority": authority.as_str(),
                    "lifecycle": lifecycle.as_str(),
                    "source_refs": entry.metadata.get("source_refs").cloned().unwrap_or_else(|| json!([])),
                    "evidence_refs_v1": entry.metadata.get("evidence_refs_v1").cloned().unwrap_or_else(|| json!([])),
                    "references": preferred_wiki_references(&entry.metadata),
                    "review_receipt": review_receipt,
                    "store": stored.store,
                }
            }))
        }
        None => Ok(json!({
            "status": "not_found",
            "stores": stores_for_wiki_plan(server, plan),
            "path": resolved,
            "entry": null,
            "next_action": "Use tachi_wiki(action=\"search\") or tachi_wiki(action=\"browse\") to find entries.",
        })),
    }
}

fn wiki_read_candidate(entry: &StoredWikiEntry) -> Value {
    let stored = &entry.entry;
    json!({
        "id": stored.id,
        "path": stored.path,
        "summary": stored.summary,
        "importance": stored.importance,
        "keywords": stored.keywords,
        "entities": stored.entities,
        "topic": stored.topic,
        "timestamp": stored.timestamp,
        "store": entry.store,
    })
}
