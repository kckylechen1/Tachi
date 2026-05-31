use super::*;
use crate::memory_search_ops::search_helpers::{
    apply_guide_context_boosts, dedup_search_results, infer_search_project, normalize_json_relevance,
    normalize_search_relevance, search_score,
};
use std::collections::HashSet;

pub(crate) async fn search_memory_rows(
    server: &MemoryServer,
    mut params: SearchMemoryParams,
) -> Result<Vec<serde_json::Value>, String> {
    if !params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"))
        && memory_core::should_skip_query(&params.query)
    {
        return Ok(vec![]);
    }
    let top_k = params.top_k.max(1);
    params.top_k = top_k;

    let named_project_vec_available = if let Some(ref project_name) = params.project {
        server
            .with_named_project_store_read(project_name, |store| Ok(store.vec_available))
            .unwrap_or(false)
    } else {
        false
    };

    if params.query_vec.is_none()
        && (server.global_vec_available
            || server.project_vec_available
            || named_project_vec_available)
    {
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

    if let Some(ref project_name) = params.project {
        let project_results = server.with_named_project_store_read(project_name, |store| {
            let vec_avail = store.vec_available;
            let project_opts = params.to_search_options(vec_avail);
            store
                .search(&params.query, Some(project_opts))
                .map_err(|e| format!("Search failed in project DB '{}': {}", project_name, e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    } else {
        let inferred_project = infer_search_project(&params.query, params.domain.as_deref());
        let inferred_db_path = inferred_project
            .as_deref()
            .and_then(|name| crate::MemoryServer::resolve_named_project_db_path(name).ok());
        let workspace_db_path = server.project_db_path_buf();
        let skip_workspace =
            inferred_db_path.is_some() && workspace_db_path.as_ref() == inferred_db_path.as_ref();

        let global_opts = params.to_search_options(server.global_vec_available);
        let global_results = server.with_global_store_read(|store| {
            store
                .search(&params.query, Some(global_opts))
                .map_err(|e| format!("Search failed in global DB: {}", e))
        })?;
        combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

        if let Some(ref project_name) = inferred_project {
            match server.with_named_project_store_read(project_name, |store| {
                let vec_avail = store.vec_available;
                let project_opts = params.to_search_options(vec_avail);
                store
                    .search(&params.query, Some(project_opts))
                    .map_err(|e| {
                        format!("Search failed in inferred project DB '{project_name}': {e}")
                    })
            }) {
                Ok(project_results) => {
                    combined_results
                        .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                }
                Err(e) => {
                    tracing::warn!("Search failed in inferred project DB '{project_name}': {e}");
                }
            }
        } else if server.has_project_db() && !skip_workspace {
            let project_opts = params.to_search_options(server.project_vec_available);
            let project_results = server.with_project_store_read(|store| {
                store
                    .search(&params.query, Some(project_opts))
                    .map_err(|e| format!("Search failed in project DB: {}", e))
            })?;
            combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
        }
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
        .map(|(r, db_scope)| slim_search_result(r, *db_scope))
        .collect();

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
) -> Result<String, String> {
    let top_k = params.top_k.max(1);
    let mut search_params = params.clone();
    if params.enable_rerank {
        search_params.top_k = top_k.saturating_mul(3).max(top_k + 1);
        search_params.candidates_per_channel = search_params
            .candidates_per_channel
            .max(search_params.top_k);
    }
    let mut rows = search_memory_rows(server, search_params).await?;
    if params.enable_rerank && rows.len() > top_k {
        if rows.len() >= 3 && search_score(&rows[0]) - search_score(&rows[2]) < 0.15 {
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

    let global_opts = SearchOptions {
        candidates_per_channel: params.candidates_per_channel.max(params.top_k),
        top_k: params.top_k,
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
    };

    let global_results = server.with_global_store_read(|store| {
        store
            .search("", Some(global_opts))
            .map_err(|e| format!("Vector search failed in global DB: {}", e))
    })?;
    combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

    if server.has_project_db() {
        let project_opts = SearchOptions {
            candidates_per_channel: params.candidates_per_channel.max(params.top_k),
            top_k: params.top_k,
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
