use super::*;

// ─── Wiki Search ────────────────────────────────────────────────────────────

/// Wiki category prefixes for quick lookup. Resolves short names to full paths.
fn resolve_wiki_category(category: &str) -> String {
    let trimmed = category.trim().trim_start_matches('/').replace('\\', "/");
    // Already a full Wiki or guide path.
    if trimmed == "wiki"
        || trimmed.starts_with("wiki/")
        || trimmed.starts_with("wiki\\")
        || trimmed == "guide"
        || trimmed.starts_with("guide/")
        || trimmed.starts_with("guide\\")
    {
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

fn knowledge_artifact_root(path: &str) -> &'static str {
    if path == "/guide" || path.starts_with("/guide/") {
        "/guide"
    } else {
        "/wiki"
    }
}

/// The two knowledge-artifact roots this crate is willing to project as
/// wiki-corpus content. Shared with `lint` so its `path_prefix` clamp uses the
/// same judgment `browse`/`read` already apply (tachi#1561 L6).
pub(super) fn is_public_knowledge_artifact_path(path: &str) -> bool {
    path == "/wiki" || path.starts_with("/wiki/") || path == "/guide" || path.starts_with("/guide/")
}

fn list_public_knowledge_entries_for_plan(
    server: &MemoryServer,
    plan: &WikiReadPlan,
    limit_per_root: usize,
) -> Result<Vec<StoredWikiEntry>, String> {
    let mut entries = list_wiki_entries_for_plan(server, plan, "/wiki", limit_per_root)?;
    entries.extend(list_wiki_entries_for_plan(
        server,
        plan,
        "/guide",
        limit_per_root,
    )?);
    Ok(entries)
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
    // #1624: a resolved-empty store list (e.g. `--no-project-db` with no
    // named "wiki" project) must refuse loudly, not report a clean
    // `status:"completed", count:0` — that shape is indistinguishable from
    // "searched everywhere, found nothing" and hides the misconfiguration.
    // Resolve once here and reuse below for the response's "stores" field —
    // `zero_store_refusal` consumes this resolved list rather than
    // re-deriving emptiness itself.
    let stores = stores_for_wiki_plan(server, &plan);
    if let Some(refusal) = zero_store_refusal(&plan, &stores) {
        return Err(refusal);
    }
    let path_prefix = params
        .category
        .as_deref()
        .map(resolve_wiki_category)
        .or_else(|| params.path_prefix.clone());
    let search_result = search_wiki_rows_for_plan(
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
        &plan,
        params.lifecycle.as_deref(),
        false,
    )
    .await?;

    append_wiki_log(
        server,
        "search",
        &format!("{} | {} result(s)", query, search_result.rows.len()),
    );

    Ok(json!({
        "status": "completed",
        "query": query,
        "path_prefix": path_prefix,
        "project": params.project,
        "stores": stores,
        "domain": params.domain,
        "unfiltered_count": search_result.unfiltered_count,
        "candidate_counts": search_result.candidate_counts,
        "count": search_result.rows.len(),
        "results": search_result.rows,
    }))
}

#[derive(Debug)]
pub(crate) struct WikiSearchRowsResult {
    pub(crate) rows: Vec<Value>,
    pub(crate) unfiltered_count: usize,
    pub(crate) candidate_counts: Vec<Value>,
}

pub(crate) async fn search_wiki_rows_for_plan(
    server: &MemoryServer,
    mut params: SearchMemoryParams,
    plan: &WikiReadPlan,
    requested_lifecycle: Option<&str>,
    record_access: bool,
) -> Result<WikiSearchRowsResult, String> {
    if let WikiReadPlan::NamedOnly(StoreRef::NamedProject { project }) = plan {
        if !crate::memory_search_ops::named_project_db_exists(server, project) {
            return Err(format!("Wiki project '{project}' not found"));
        }
    }
    let final_top_k = params.top_k.max(1).min(50);
    let per_store_candidate_budget = final_top_k.max(20);
    params.top_k = per_store_candidate_budget;
    params.candidates_per_channel = params
        .candidates_per_channel
        .max(per_store_candidate_budget);
    let stores = stores_for_wiki_plan(server, plan);
    let path_prefix = params.path_prefix.clone();
    let query = params.query.clone();
    let include_metadata = params.include_metadata;
    let mut candidates =
        search_wiki_store_candidates(server, params, &stores, record_access).await?;
    candidates.retain(|candidate| {
        is_user_facing_wiki_entry(&candidate.result.entry)
            && is_public_knowledge_artifact_path(&candidate.result.entry.path)
            && path_prefix.as_deref().is_none_or(|prefix| {
                candidate.result.entry.path == prefix
                    || candidate
                        .result
                        .entry
                        .path
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
    });
    let unfiltered_count = candidates.len();
    let candidate_counts = stores
        .iter()
        .map(|store| {
            let count = candidates
                .iter()
                .filter(|candidate| &candidate.store == store)
                .count();
            json!({"store": store, "count": count})
        })
        .collect::<Vec<_>>();
    candidates.retain(wiki_candidate_has_direct_match_signal);

    let mut eligible = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if wiki_entry_matches_lifecycle_scope(&candidate.result.entry, requested_lifecycle)? {
            eligible.push(candidate);
        }
    }
    eligible.sort_by(|left, right| {
        right
            .result
            .score
            .final_score
            .partial_cmp(&left.result.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .result
                    .entry
                    .timestamp
                    .cmp(&left.result.entry.timestamp)
            })
            .then_with(|| left.result.entry.id.cmp(&right.result.entry.id))
            .then_with(|| format!("{:?}", left.store).cmp(&format!("{:?}", right.store)))
    });
    let mut seen = HashSet::new();
    eligible.retain(|candidate| {
        seen.insert((candidate.store.clone(), candidate.result.entry.id.clone()))
    });
    eligible.truncate(final_top_k);
    normalize_wiki_candidate_relevance(&mut eligible);

    let mut rows = eligible
        .iter()
        .map(|candidate| {
            let db_scope = match candidate.store {
                StoreRef::LegacyGlobal => DbScope::Global,
                StoreRef::BoundProject | StoreRef::NamedProject { .. } => DbScope::Project,
            };
            let mut row = slim_search_result(&candidate.result, db_scope, include_metadata);
            attach_wiki_provenance(&mut row, &candidate.result.entry, &candidate.store);
            if let Some(recall_quality) = &candidate.recall_quality {
                if let Some(object) = row.as_object_mut() {
                    object.insert("recall_quality".to_string(), recall_quality.clone());
                }
            }
            row
        })
        .collect::<Vec<_>>();
    annotate_wiki_exact_token_matches(&mut rows, &query);

    Ok(WikiSearchRowsResult {
        rows,
        unfiltered_count,
        candidate_counts,
    })
}

fn wiki_candidate_has_direct_match_signal(candidate: &WikiStoreSearchCandidate) -> bool {
    let rounded_fts = (candidate.result.score.fts * 1000.0).round() / 1000.0;
    let rounded_symbolic = (candidate.result.score.symbolic * 1000.0).round() / 1000.0;
    rounded_fts > 0.0 || rounded_symbolic > 0.0
}

fn normalize_wiki_candidate_relevance(candidates: &mut [WikiStoreSearchCandidate]) {
    let max_score = candidates
        .iter()
        .map(|candidate| candidate.result.score.final_score)
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);
    if max_score <= f64::EPSILON {
        return;
    }
    for candidate in candidates {
        candidate.result.score.final_score =
            (candidate.result.score.final_score / max_score).clamp(0.0, 1.0);
    }
}

#[cfg(test)]
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
            let all_entries = list_public_knowledge_entries_for_plan(server, &plan, 5000)?;

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

            let entries = list_wiki_entries_for_plan(
                server,
                &plan,
                knowledge_artifact_root(&resolved_path),
                5000,
            )?;
            let resolved_prefix = format!("{resolved_path}/");
            let store_order = stores_for_wiki_plan(server, &plan);
            let mut per_store_entries = vec![Vec::<Value>::new(); store_order.len()];
            for stored in entries {
                let entry = stored.entry;
                if !(entry.path == resolved_path || entry.path.starts_with(&resolved_prefix)) {
                    continue;
                }
                if !wiki_entry_matches_lifecycle_scope(&entry, requested_lifecycle)? {
                    continue;
                }
                let effective =
                    derive_effective_knowledge_artifact(&entry.metadata, &entry.path, &entry.scope);
                // #1072 fix-round (#1215 BUG 6): browse-category provenance
                // was overclaimed — the PR description said read/search
                // "expose ... revision, source refs, ... review receipt" but
                // this branch (unlike `collect_wiki_search_value`'s
                // `attach_wiki_provenance`) dropped id/revision/references/
                // review_receipt entirely. Bring it to parity.
                let review_receipt = derive_wiki_review_receipt(&entry.metadata)
                    .and_then(|receipt| serde_json::to_value(receipt).ok())
                    .unwrap_or(Value::Null);
                let Some(store_index) = store_order.iter().position(|store| store == &stored.store)
                else {
                    continue;
                };
                per_store_entries[store_index].push(json!({
                    "id": entry.id,
                    "path": entry.path,
                    "summary": entry.summary,
                    "importance": entry.importance,
                    "revision": entry.revision,
                    "lifecycle": effective.lifecycle.as_str(),
                    "authority": effective.authority.as_str(),
                    "effective_artifact": effective,
                    "references": preferred_wiki_references(&entry.metadata),
                    "review_receipt": review_receipt,
                    "store": stored.store,
                }));
            }
            let mut slim_entries = Vec::with_capacity(limit);
            let mut indexes = vec![0usize; per_store_entries.len()];
            while slim_entries.len() < limit {
                let mut made_progress = false;
                for (store_index, entries) in per_store_entries.iter().enumerate() {
                    if slim_entries.len() >= limit {
                        break;
                    }
                    let index = indexes[store_index];
                    if let Some(entry) = entries.get(index) {
                        slim_entries.push(entry.clone());
                        indexes[store_index] += 1;
                        made_progress = true;
                    }
                }
                if !made_progress {
                    break;
                }
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

#[cfg(test)]
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
        let candidates = value
            .get("candidates")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        return Ok(crate::agent_markdown::format_wiki_read_ambiguity(
            resolved, count, candidates,
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

#[cfg(test)]
pub(crate) fn collect_wiki_read_value(
    server: &MemoryServer,
    path: &str,
    project: &str,
) -> Result<Value, String> {
    let plan = legacy_wiki_read_plan(project);
    collect_wiki_read_value_for_plan(server, path, &plan)
}

#[cfg(test)]
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
    let normalized = path.trim().replace('\\', "/");
    let resolved = if normalized.starts_with('/') {
        let trimmed = normalized.trim_end_matches('/');
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

    let entries =
        list_wiki_entries_for_plan(server, plan, knowledge_artifact_root(&resolved), 5000)?;
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
            let effective =
                derive_effective_knowledge_artifact(&entry.metadata, &entry.path, &entry.scope);
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
                    "authority": effective.authority.as_str(),
                    "lifecycle": effective.lifecycle.as_str(),
                    "effective_artifact": effective,
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
    let effective =
        derive_effective_knowledge_artifact(&stored.metadata, &stored.path, &stored.scope);
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
        "effective_artifact": effective,
    })
}
