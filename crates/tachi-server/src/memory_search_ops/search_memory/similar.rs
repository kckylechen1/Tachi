use std::collections::HashSet;

use memcore::SearchOptions;
use serde_json::json;

use super::cache::recall_cache_recall_opted_in;
use super::filters::find_similar_training_opted_in;
use crate::memory_search_ops::auto_link::is_training_seed;
use crate::shared_defs::slim_entry;
use crate::tool_params::FindSimilarMemoryParams;
use crate::{DbScope, MemoryServer};

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

    let mut combined_results: Vec<(memcore::SearchResult, DbScope)> = Vec::new();
    let common_weights = memcore::HybridWeights {
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
        vec_available: server.global_vec_available(),
        record_access: false,
        include_archived: params.include_archived,
        include_superseded: false,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        domain: None,
        as_of: None,
        precision_matchers: Vec::new(),
        recall_config: None,
        decay_policy: None,
    };

    if let Some(ref project_name) = params.project {
        let global_results = server.with_global_store_read(|store| {
            store
                .search("", Some(global_opts))
                .map_err(|e| format!("Vector search failed in global DB: {}", e))
        })?;
        combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

        let project_vec_available = server
            .with_named_project_store_read(project_name, |store| Ok(store.vec_available))
            .unwrap_or(false);
        let project_opts = SearchOptions {
            candidates_per_channel,
            top_k,
            weights: common_weights.clone(),
            path_prefix: params.path_prefix.clone(),
            query_vec: Some(params.query_vec.clone()),
            vec_available: project_vec_available,
            record_access: false,
            include_archived: params.include_archived,
            include_superseded: false,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            domain: None,
            as_of: None,
            precision_matchers: Vec::new(),
            recall_config: None,
            decay_policy: None,
        };
        let project_results = server.with_named_project_store_read(project_name, |store| {
            store
                .search("", Some(project_opts))
                .map_err(|e| format!("Vector search failed in project DB '{project_name}': {e}"))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    } else {
        let global_results = server.with_global_store_read(|store| {
            store
                .search("", Some(global_opts))
                .map_err(|e| format!("Vector search failed in global DB: {}", e))
        })?;
        combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));
    }

    if params.project.is_none() && server.has_project_db() {
        let project_opts = SearchOptions {
            candidates_per_channel,
            top_k,
            weights: common_weights,
            path_prefix: params.path_prefix.clone(),
            query_vec: Some(params.query_vec.clone()),
            vec_available: server.project_vec_available(),
            record_access: false,
            include_archived: params.include_archived,
            include_superseded: false,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            domain: None,
            as_of: None,
            precision_matchers: Vec::new(),
            recall_config: None,
            decay_policy: None,
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
        combined_results.retain(|(result, _)| !memcore::is_recall_cache_entry(&result.entry));
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
