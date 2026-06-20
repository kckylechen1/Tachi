use super::auto_link::is_training_seed;
use super::*;
use crate::memory_search_ops::search_helpers::{
    apply_guide_context_boosts, dedup_search_results, infer_search_project,
    named_project_db_exists, normalize_json_relevance, normalize_search_relevance, search_score,
};
use std::collections::HashSet;

fn is_training_path(path: &str) -> bool {
    path == "/sft" || path.starts_with("/sft/")
}

fn is_eval_path(path: &str) -> bool {
    path == "/eval" || path.starts_with("/eval/")
}

fn is_eval_entry(entry: &memory_core::MemoryEntry) -> bool {
    memory_core::is_eval_entry(entry) || is_eval_path(&entry.path)
}

fn training_recall_opted_in(params: &SearchMemoryParams) -> bool {
    params.include_training
        || params
            .path_prefix
            .as_deref()
            .is_some_and(|prefix| is_training_path(prefix.trim_end_matches('/')))
}

fn find_similar_training_opted_in(params: &FindSimilarMemoryParams) -> bool {
    params.include_training
        || params
            .path_prefix
            .as_deref()
            .is_some_and(|prefix| is_training_path(prefix.trim_end_matches('/')))
}

fn eval_recall_opted_in(params: &SearchMemoryParams) -> bool {
    params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| is_eval_path(prefix.trim_end_matches('/')))
}

fn query_explicitly_requests_foreign_sigil_domain(query: &str, domain: Option<&str>) -> bool {
    if domain.is_some() {
        return true;
    }
    let config = super::routing_config::RoutingConfig::get();
    let q = query.to_lowercase();
    // Whole-word ASCII terms (so common coding queries like "change"/"channel"
    // → "chan", "quantity" → "quant" don't trip the filter) plus CJK/numeric
    // substrings. Both term lists come from RoutingConfig, not hardcoded here.
    let matches_word = q
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| config.foreign_domain_word_terms.iter().any(|t| t == word));
    matches_word
        || config
            .foreign_domain_substring_terms
            .iter()
            .any(|term| q.contains(term))
}

fn is_foreign_sigil_memory(entry: &memory_core::MemoryEntry) -> bool {
    let config = super::routing_config::RoutingConfig::get();
    let domain = entry.domain.as_deref().unwrap_or("");
    let path = entry.path.to_ascii_lowercase();
    config
        .foreign_domains
        .iter()
        .any(|d| d.eq_ignore_ascii_case(domain))
        || config
            .foreign_path_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
}

fn project_scope_allows_memory(
    project_name: &str,
    params: &SearchMemoryParams,
    entry: &memory_core::MemoryEntry,
) -> bool {
    if !project_name.eq_ignore_ascii_case("sigil") {
        return true;
    }
    if query_explicitly_requests_foreign_sigil_domain(&params.query, params.domain.as_deref()) {
        return true;
    }
    !is_foreign_sigil_memory(entry)
}

fn project_filter_name(params: &SearchMemoryParams, project_only: bool) -> Option<String> {
    params.project.clone().or_else(|| {
        if project_only {
            crate::memory_search_ops::search_helpers::resolve_workspace_named_project()
        } else {
            infer_search_project(&params.query, params.domain.as_deref())
        }
    })
}

fn search_store(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    record_access: bool,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let mut opts = params.to_search_options(store.vec_available);
    opts.record_access = record_access;
    store
        .search(&params.query, Some(opts))
        .map_err(|e| e.to_string())
}

fn with_named_project_search(
    server: &MemoryServer,
    project_name: &str,
    params: &SearchMemoryParams,
    record_access: bool,
    context: impl Into<String>,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access).map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_named_project_store_read(project_name, action)
    }
}

fn with_project_search(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    context: impl Into<String>,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access).map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_project_store(action)
    } else {
        server.with_project_store_read(action)
    }
}

fn with_global_search(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    context: impl Into<String>,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access).map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_global_store(action)
    } else {
        server.with_global_store_read(action)
    }
}

fn recall_cache_recall_opted_in(path_prefix: Option<&str>) -> bool {
    memory_core::path_prefix_opts_into_recall_cache(path_prefix)
}

fn row_text_for_exact_match(row: &serde_json::Value) -> String {
    ["id", "path", "topic", "summary", "excerpt"]
        .into_iter()
        .filter_map(|key| row.get(key).and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

fn row_has_exact_token_match(query: &str, row: &serde_json::Value) -> bool {
    if row.get("match_type").and_then(serde_json::Value::as_str) == Some("exact_token") {
        return true;
    }
    if !memory_core::scorer::is_id_like_exact_query(query) {
        return false;
    }
    row_text_for_exact_match(row).contains(&query.trim().to_ascii_lowercase())
}

fn mark_exact_token_match(row: &mut serde_json::Value) {
    if let Some(obj) = row.as_object_mut() {
        obj.insert("match_type".into(), json!("exact_token"));
    }
}

fn annotate_exact_token_matches(rows: &mut [serde_json::Value], query: &str) {
    for row in rows {
        if !row_has_exact_token_match(query, row) {
            continue;
        }
        mark_exact_token_match(row);
    }
}

fn has_high_confidence_exact_token_top(rows: &[serde_json::Value], query: &str) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    if !row_has_exact_token_match(query, first) {
        return false;
    }
    let fts = first
        .get("score")
        .and_then(|score| score.get("fts"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let symbolic = first
        .get("score")
        .and_then(|score| score.get("symbolic"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    fts >= 0.95 || symbolic >= 0.95
}

pub(crate) async fn search_memory_rows(
    server: &MemoryServer,
    params: SearchMemoryParams,
    project_only: bool,
) -> Result<Vec<serde_json::Value>, String> {
    search_memory_rows_with_access(server, params, project_only, false).await
}

pub(crate) async fn search_memory_rows_with_access(
    server: &MemoryServer,
    mut params: SearchMemoryParams,
    project_only: bool,
    record_access: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let wiki_path_prefix = params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"));
    if !wiki_path_prefix && memory_core::should_skip_query(&params.query) {
        return Ok(vec![]);
    }
    let top_k = params.normalized_top_k();
    params.top_k = top_k;
    params.candidates_per_channel = params.normalized_candidates_per_channel();

    let named_project_vec_available = if let Some(ref project_name) = params.project {
        server
            .with_named_project_store_read(project_name, |store| Ok(store.vec_available))
            .unwrap_or(false)
    } else {
        false
    };
    let default_wiki_vec_available =
        if params.project.is_none() && wiki_path_prefix && named_project_db_exists("wiki") {
            server
                .with_named_project_store_read("wiki", |store| Ok(store.vec_available))
                .unwrap_or(false)
        } else {
            false
        };

    let mut searched_default_wiki = false;

    if params.query_vec.is_none()
        && !parse_env_bool("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING").unwrap_or(false)
        && (server.global_vec_available
            || server.project_vec_available
            || named_project_vec_available
            || default_wiki_vec_available)
    {
        server.ensure_provider_secrets_materialized(&["VOYAGE_API_KEY"]);
        match server.llm.embed_voyage(&params.query, "query").await {
            Ok(query_vec) => {
                params.query_vec = Some(query_vec);
            }
            Err(e) => {
                eprintln!(
                    "[search_memory] query embedding failed, falling back to lexical-only search: {e}"
                );
            }
        }
    }

    let pipeline_enabled = server.pipeline_enabled;

    let mut combined_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();

    let mut searched_named = false;
    if let Some(ref project_name) = params.project {
        if crate::memory_search_ops::search_helpers::named_project_db_exists(project_name) {
            let project_results = with_named_project_search(
                server,
                project_name,
                &params,
                record_access,
                format!("Search failed in project DB '{project_name}'"),
            )?;
            combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
            searched_named = true;
        } else if !project_only {
            return Err(format!(
                "Project '{project_name}' not found (expected DB at {})",
                crate::MemoryServer::resolve_named_project_db_path(project_name)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| e)
            ));
        }
    }

    if params.project.is_none() && wiki_path_prefix && named_project_db_exists("wiki") {
        match with_named_project_search(
            server,
            "wiki",
            &params,
            record_access,
            "Search failed in default wiki project DB",
        ) {
            Ok(wiki_results) => {
                combined_results.extend(wiki_results.into_iter().map(|r| (r, DbScope::Project)));
                searched_default_wiki = true;
            }
            Err(e) => {
                tracing::warn!("Search failed in default wiki project DB: {e}");
            }
        }
    }

    if !searched_named {
        if project_only {
            let named_project =
                crate::memory_search_ops::search_helpers::resolve_workspace_named_project();
            if let Some(ref project_name) = named_project {
                if crate::memory_search_ops::search_helpers::named_project_db_exists(project_name)
                    && (project_name != "wiki" || !searched_default_wiki)
                {
                    let workspace_path = server.project_db_path_buf();
                    let named_path =
                        crate::MemoryServer::resolve_named_project_db_path(project_name).ok();
                    let skip_workspace = workspace_path
                        .as_deref()
                        .zip(named_path.as_deref())
                        .map(|(w, n)| w == n)
                        .unwrap_or(false);

                    if !skip_workspace && server.has_project_db() {
                        let project_results = with_project_search(
                            server,
                            &params,
                            record_access,
                            "Search failed in workspace project DB",
                        )?;
                        combined_results
                            .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                    }

                    if named_path.is_some() {
                        let project_results = with_named_project_search(
                            server,
                            project_name,
                            &params,
                            record_access,
                            format!("Search failed in named project DB '{project_name}'"),
                        )?;
                        combined_results
                            .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                    }
                } else if server.has_project_db() {
                    let project_results = with_project_search(
                        server,
                        &params,
                        record_access,
                        "Search failed in workspace project DB",
                    )?;
                    combined_results
                        .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                }
            } else if server.has_project_db() {
                let project_results = with_project_search(
                    server,
                    &params,
                    record_access,
                    "Search failed in workspace project DB",
                )?;
                combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
            }
        } else {
            let inferred_project = infer_search_project(&params.query, params.domain.as_deref());
            let inferred_db_path = inferred_project
                .as_deref()
                .and_then(|name| crate::MemoryServer::resolve_named_project_db_path(name).ok());
            let workspace_db_path = server.project_db_path_buf();
            let skip_workspace = inferred_db_path.is_some()
                && workspace_db_path.as_ref() == inferred_db_path.as_ref();

            let global_results =
                with_global_search(server, &params, record_access, "Search failed in global DB")?;
            combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

            if let Some(ref project_name) = inferred_project {
                if project_name == "wiki" && searched_default_wiki {
                    // Already searched the canonical wiki store above for
                    // unscoped /wiki queries.
                } else {
                    match with_named_project_search(
                        server,
                        project_name,
                        &params,
                        record_access,
                        format!("Search failed in inferred project DB '{project_name}'"),
                    ) {
                        Ok(project_results) => {
                            combined_results
                                .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Search failed in inferred project DB '{project_name}': {e}"
                            );
                        }
                    }
                }
            } else if server.has_project_db() && !skip_workspace {
                let project_results = with_project_search(
                    server,
                    &params,
                    record_access,
                    "Search failed in project DB",
                )?;
                combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
            }
        }
    }

    if !training_recall_opted_in(&params) {
        combined_results.retain(|(result, _)| !is_training_seed(&result.entry));
    }
    if !recall_cache_recall_opted_in(params.path_prefix.as_deref()) {
        combined_results.retain(|(result, _)| !memory_core::is_recall_cache_entry(&result.entry));
    }
    if !eval_recall_opted_in(&params) {
        combined_results.retain(|(result, _)| !is_eval_entry(&result.entry));
    }
    if let Some(project_name) = project_filter_name(&params, project_only) {
        combined_results.retain(|(result, db_scope)| match db_scope {
            DbScope::Project => project_scope_allows_memory(&project_name, &params, &result.entry),
            DbScope::Global => true,
        });
    }

    apply_guide_context_boosts(
        &mut combined_results,
        params.file_context.as_deref(),
        params.error_context.as_deref(),
    );

    combined_results.sort_by(|a, b| {
        b.0.score
            .final_score
            .partial_cmp(&a.0.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    combined_results = dedup_search_results(combined_results, top_k);

    let mut seen_ids = HashSet::new();
    let mut deduped_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();
    for (result, db_scope) in combined_results {
        if seen_ids.insert(result.entry.id.clone()) {
            deduped_results.push((result, db_scope));
        }
        if deduped_results.len() >= top_k {
            break;
        }
    }

    normalize_search_relevance(&mut deduped_results);

    // Sandbox filtering: if agent_role is specified, filter out denied entries
    if let Some(ref role) = params.agent_role {
        deduped_results.retain(|(result, db_scope)| {
            let allowed = match db_scope {
                DbScope::Global => server.with_global_store_read(|store| {
                    store
                        .check_sandbox_access(role, &result.entry.path, "read")
                        .map(|(allowed, _)| allowed)
                        .map_err(|e| format!("{e}"))
                }),
                DbScope::Project => {
                    if let Some(ref p) = params.project {
                        server.with_named_project_store_read(p, |store| {
                            store
                                .check_sandbox_access(role, &result.entry.path, "read")
                                .map(|(allowed, _)| allowed)
                                .map_err(|e| format!("{e}"))
                        })
                    } else {
                        server.with_project_store_read(|store| {
                            store
                                .check_sandbox_access(role, &result.entry.path, "read")
                                .map(|(allowed, _)| allowed)
                                .map_err(|e| format!("{e}"))
                        })
                    }
                }
            };
            allowed.unwrap_or(true)
        });
    }

    let mut output: Vec<serde_json::Value> = deduped_results
        .iter()
        .map(|(r, db_scope)| {
            let mut row = slim_search_result(r, *db_scope, params.include_metadata);
            if memory_core::scorer::is_id_like_exact_query(&params.query)
                && memory_core::scorer::entry_has_exact_query_token(&r.entry, &params.query)
            {
                mark_exact_token_match(&mut row);
            }
            row
        })
        .collect();
    annotate_exact_token_matches(&mut output, &params.query);

    if pipeline_enabled {
        let mut existing_ids: HashSet<String> = deduped_results
            .iter()
            .map(|(r, _)| r.entry.id.clone())
            .collect();

        if server.has_project_db() {
            let project_rules = server.with_project_store_read(|store| {
                Ok(store
                    .list_by_path("/behavior/global_rules", 50, false)
                    .unwrap_or_default())
            })?;
            for rule in project_rules {
                if !is_active_global_rule(&rule) {
                    continue;
                }
                if !existing_ids.insert(rule.id.clone()) {
                    continue;
                }
                output.push(slim_l0_rule(&rule, DbScope::Project));
            }
        }

        let global_rules = server.with_global_store_read(|store| {
            Ok(store
                .list_by_path("/behavior/global_rules", 50, false)
                .unwrap_or_default())
        })?;
        for rule in global_rules {
            if !is_active_global_rule(&rule) {
                continue;
            }
            if !existing_ids.insert(rule.id.clone()) {
                continue;
            }
            output.push(slim_l0_rule(&rule, DbScope::Global));
        }
    }

    Ok(output)
}

pub(crate) async fn handle_search_memory(
    server: &MemoryServer,
    params: SearchMemoryParams,
    project_only: bool,
) -> Result<String, String> {
    handle_search_memory_with_access(server, params, project_only, false).await
}

pub(crate) async fn handle_search_memory_with_access(
    server: &MemoryServer,
    params: SearchMemoryParams,
    project_only: bool,
    record_access: bool,
) -> Result<String, String> {
    let top_k = params.normalized_top_k();
    let mut search_params = params.clone();
    if params.enable_rerank {
        search_params.top_k = top_k.saturating_mul(3);
        search_params.candidates_per_channel = search_params
            .candidates_per_channel
            .max(search_params.top_k)
            .min(crate::tool_params::MAX_SEARCH_CANDIDATES_PER_CHANNEL);
    }
    let mut rows =
        search_memory_rows_with_access(server, search_params, project_only, record_access).await?;
    if params.enable_rerank && rows.len() > top_k {
        if has_high_confidence_exact_token_top(&rows, &params.query) {
            if let Some(obj) = rows.first_mut().and_then(serde_json::Value::as_object_mut) {
                obj.insert("rerank_policy".into(), json!("skipped_exact_token"));
            }
            rows.truncate(top_k);
        } else if rows.len() >= 3 && search_score(&rows[0]) - search_score(&rows[2]) < 0.15 {
            let (reranked, outcome) = crate::foundry_runtime_ops::rerank_rows_with_outcome(
                server,
                &params.query,
                rows,
                top_k,
            )
            .await;
            if outcome == crate::foundry_runtime_ops::RerankOutcome::Fallback {
                eprintln!(
                    "[search_memory] rerank fail-open: query_hash={} top_k={}",
                    stable_hash(&params.query),
                    top_k
                );
            }
            rows = reranked;
        } else {
            rows.truncate(top_k);
        }
    } else {
        rows.truncate(top_k);
    }
    normalize_json_relevance(&mut rows);
    serde_json::to_string(&rows).map_err(|e| format!("Failed to serialize response: {}", e))
}

pub(crate) async fn handle_find_similar_memory(
    server: &MemoryServer,
    params: FindSimilarMemoryParams,
) -> Result<String, String> {
    if params.query_vec.is_empty() {
        return serde_json::to_string(&json!([]))
            .map_err(|e| format!("Failed to serialize response: {}", e));
    }

    if params.query_vec.iter().any(|v| !v.is_finite()) {
        return Err("query_vec contains non-finite values".to_string());
    }

    let mut combined_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();
    let common_weights = memory_core::HybridWeights {
        semantic: 1.0,
        fts: 0.0,
        symbolic: 0.0,
        decay: 0.0,
        use_rrf: false,
    };

    let top_k = params.normalized_top_k();
    let candidates_per_channel = params.normalized_candidates_per_channel();
    let global_opts = SearchOptions {
        candidates_per_channel,
        top_k,
        weights: common_weights.clone(),
        path_prefix: params.path_prefix.clone(),
        query_vec: Some(params.query_vec.clone()),
        vec_available: server.global_vec_available,
        record_access: false,
        include_archived: params.include_archived,
        include_superseded: false,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        domain: None,
        as_of: None,
        precision_matchers: Vec::new(),
    };

    let global_results = server.with_global_store_read(|store| {
        store
            .search("", Some(global_opts))
            .map_err(|e| format!("Vector search failed in global DB: {}", e))
    })?;
    combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

    if server.has_project_db() {
        let project_opts = SearchOptions {
            candidates_per_channel,
            top_k,
            weights: common_weights,
            path_prefix: params.path_prefix.clone(),
            query_vec: Some(params.query_vec.clone()),
            vec_available: server.project_vec_available,
            record_access: false,
            include_archived: params.include_archived,
            include_superseded: false,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            domain: None,
            as_of: None,
            precision_matchers: Vec::new(),
        };

        let project_results = server.with_project_store_read(|store| {
            store
                .search("", Some(project_opts))
                .map_err(|e| format!("Vector search failed in project DB: {}", e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    }

    combined_results.sort_by(|a, b| {
        b.0.score
            .vector
            .partial_cmp(&a.0.score.vector)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if !find_similar_training_opted_in(&params) {
        combined_results.retain(|(result, _)| !is_training_seed(&result.entry));
    }
    if !recall_cache_recall_opted_in(params.path_prefix.as_deref()) {
        combined_results.retain(|(result, _)| !memory_core::is_recall_cache_entry(&result.entry));
    }

    let mut seen_ids = HashSet::new();
    let mut output: Vec<serde_json::Value> = Vec::new();
    for (result, db_scope) in combined_results {
        if !seen_ids.insert(result.entry.id.clone()) {
            continue;
        }
        let mut obj = match slim_entry(&result.entry, db_scope) {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        obj.insert(
            "similarity".into(),
            json!((result.score.vector * 1000.0).round() / 1000.0),
        );
        output.push(serde_json::Value::Object(obj));
        if output.len() >= params.top_k {
            break;
        }
    }

    serde_json::to_string(&output).map_err(|e| format!("Failed to serialize response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(domain: Option<&str>, path: &str) -> memory_core::MemoryEntry {
        memory_core::MemoryEntry {
            id: "id".into(),
            path: path.into(),
            summary: "summary".into(),
            text: "text".into(),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "test".into(),
            scope: "project".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: domain.map(str::to_string),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        }
    }

    fn params(query: &str) -> SearchMemoryParams {
        SearchMemoryParams {
            query: query.into(),
            query_vec: None,
            top_k: 5,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 5,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: Some("sigil".into()),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }
    }

    #[test]
    fn sigil_project_scope_filters_foreign_domains_for_default_recall() {
        let params = params("Tachi 召回 向量 有没有问题");

        assert!(!project_scope_allows_memory(
            "sigil",
            &params,
            &entry(Some("equity_trading"), "/")
        ));
        assert!(!project_scope_allows_memory(
            "sigil",
            &params,
            &entry(Some("hyperion"), "/scratch/hyperion/v4")
        ));
        assert!(project_scope_allows_memory(
            "sigil",
            &params,
            &entry(Some("scratch"), "/scratch/sigil/recall")
        ));
    }

    #[test]
    fn sigil_project_scope_allows_foreign_domains_when_query_requests_them() {
        let params = params("Hyperion V8 股票召回");

        assert!(project_scope_allows_memory(
            "sigil",
            &params,
            &entry(Some("equity_trading"), "/")
        ));
    }

    #[test]
    fn sigil_project_scope_keeps_filtering_when_query_only_substring_matches_terms() {
        // "change"/"channel"/"quantity" must NOT trip "chan"/"quant"; the
        // foreign-domain filter has to stay active for ordinary coding queries.
        for query in [
            "refactor the channel change handler",
            "compute the quantity of pending jobs",
            "address inequity in scheduling",
        ] {
            let params = params(query);
            assert!(
                !project_scope_allows_memory("sigil", &params, &entry(Some("equity_trading"), "/")),
                "query {query:?} should not unlock foreign trading memories"
            );
        }
    }

    #[test]
    fn sigil_project_scope_allows_foreign_domains_on_whole_word_match() {
        // Whole-word foreign terms (even without CJK) must still open the gate.
        for query in ["quant trading recall", "v8 engine notes", "chan pump-fake"] {
            let params = params(query);
            assert!(
                project_scope_allows_memory("sigil", &params, &entry(Some("equity_trading"), "/")),
                "query {query:?} explicitly names a foreign domain term"
            );
        }
    }
}
