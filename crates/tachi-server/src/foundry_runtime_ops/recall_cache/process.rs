use super::super::capture::persist_capture_entry;
use super::super::helpers::{normalize_path_prefix_value, path_is_within_prefix, round3};
use super::super::maintenance::with_foundry_store_read;
use super::super::recall::{value_id, value_path, value_relevance, value_topic};
use super::super::{
    FoundryMaintenanceItem, FOUNDRY_RECALL_RERANK_CACHE_SOURCE,
    FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER, FOUNDRY_RECALL_RERANK_TOP_K,
};
use super::config::{durable_recall_cache_enabled, recall_cache_write_path};
use super::queries::resolve_recall_cache_queries;
use super::search::search_rows_for_recall_cache;
use super::text::build_recall_cache_text;
use crate::memory_search_ops::rerank_rows_with_outcome;
use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::SearchMemoryParams;
use crate::utils::{sanitize_safe_path_name, stable_hash};
use chrono::Utc;
use memcore::MemoryEntry;
use serde_json::json;
use tachi_foundry::FoundryJobMetadata;

/// Public entry-point used by the maintenance worker dispatcher.
pub(in crate::foundry_runtime_ops) async fn process_recall_rerank_cache_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    if !durable_recall_cache_enabled() {
        tracing::debug!(
            "[recall_rerank_cache] durable cache writes disabled; skipping job {}",
            item.job.id
        );
        return Ok(0);
    }

    let source_entries = with_foundry_store_read(server, item, |store| {
        let mut entries = Vec::new();
        for memory_id in &item.memory_ids {
            if let Some(entry) = store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {memory_id} for recall cache: {e}"))?
            {
                entries.push(entry);
            }
        }
        Ok(entries)
    })?;

    let queries = resolve_recall_cache_queries(server, item, &source_entries).await;
    if queries.is_empty() {
        return Ok(0);
    }

    let job_metadata = FoundryJobMetadata::new(&item.job.metadata);
    let top_k = job_metadata
        .usize("top_k", FOUNDRY_RECALL_RERANK_TOP_K)
        .max(1);
    let candidate_multiplier = job_metadata
        .usize(
            "candidate_multiplier",
            FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
        )
        .max(1);
    let candidate_top_k = top_k.saturating_mul(candidate_multiplier);
    let path_prefix = job_metadata
        .string("path_prefix")
        .or_else(|| normalize_path_prefix_value(&item.path_prefix));
    let agent_role = job_metadata.string("agent_role");
    let project = job_metadata
        .string("project")
        .or(item.named_project.clone());
    let scope = if item.target_db == DbScope::Project {
        "project".to_string()
    } else {
        "global".to_string()
    };

    let mut updated = 0usize;
    for query in queries {
        let mut rows = search_rows_for_recall_cache(
            server,
            item,
            SearchMemoryParams {
                query: query.clone(),
                query_vec: None,
                top_k: candidate_top_k,
                path_prefix: path_prefix.clone(),
                include_training: false,
                include_archived: false,
                candidates_per_channel: candidate_top_k.max(20),
                mmr_threshold: None,
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                context_symbols: Vec::new(),
                agent_role: agent_role.clone(),
                project: project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
                format: None,
            },
        )
        .await?;
        // `search_memory_rows` applies a *loose* `starts_with` prefix filter at
        // the DB level (for performance). We re-apply the stricter
        // `path_is_within_prefix` here to avoid false positives when a prefix
        // is a partial segment (e.g. "/foo/b" matching "/foo/bar").
        if let Some(prefix) = path_prefix.as_deref() {
            rows.retain(|row| {
                let path = value_path(row);
                !path.is_empty() && path_is_within_prefix(&path, prefix)
            });
        }
        // Never feed cache rows back into the rerank pool — they are not
        // ground truth, just a previous run's projection of it.
        rows.retain(|row| {
            row.get("source").and_then(serde_json::Value::as_str)
                != Some(FOUNDRY_RECALL_RERANK_CACHE_SOURCE)
        });
        let (reranked, rerank_outcome) =
            rerank_rows_with_outcome(server, &query, rows, top_k).await;
        if reranked.is_empty() {
            continue;
        }

        let cache_seed = format!(
            "{}|{}|{}|{}",
            item.named_project.as_deref().unwrap_or("default"),
            item.path_prefix,
            top_k,
            query
        );
        let cache_id = format!("foundry:recall-cache:{}", stable_hash(&cache_seed));
        let cache_topic = sanitize_safe_path_name(&query)
            .chars()
            .take(64)
            .collect::<String>();
        let cache_path = recall_cache_write_path(
            &item.path_prefix,
            &cache_topic,
            item.named_project.as_deref(),
        );
        let result_ids = reranked.iter().map(value_id).collect::<Vec<_>>();
        let result_scores = reranked
            .iter()
            .map(|row| {
                json!({
                    "id": value_id(row),
                    "score": round3(value_relevance(row)),
                    "path": value_path(row),
                    "topic": value_topic(row),
                })
            })
            .collect::<Vec<_>>();
        let timestamp = Utc::now().to_rfc3339();
        let text = build_recall_cache_text(&query, &reranked);
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "query": query,
                "top_k": top_k,
                "candidate_multiplier": candidate_multiplier,
                "candidate_top_k": candidate_top_k,
                "source_memory_ids": item.memory_ids.clone(),
                "result_ids": result_ids.clone(),
                "result_scores": result_scores,
                "rerank_outcome": format!("{:?}", rerank_outcome).to_ascii_lowercase(),
                "job_id": item.job.id.clone(),
                "path_prefix": item.path_prefix.clone(),
            }),
            "foundry_worker",
            "recall_rerank_cache",
            Some(scope.as_str()),
            item.target_db,
            json!({
                "agent_id": item.job.target_agent_id.clone(),
                "path_prefix": item.path_prefix.clone(),
            }),
        );

        let cache_entry = MemoryEntry {
            id: cache_id,
            path: cache_path,
            summary: text.chars().take(100).collect(),
            text,
            importance: 0.35,
            timestamp,
            valid_from: String::new(),
            valid_until: None,
            category: "other".to_string(),
            topic: "recall_rerank_cache".to_string(),
            keywords: vec![
                "foundry".to_string(),
                "recall".to_string(),
                "rerank".to_string(),
                "cache".to_string(),
            ],
            persons: Vec::new(),
            entities: result_ids,
            location: String::new(),
            source: FOUNDRY_RECALL_RERANK_CACHE_SOURCE.to_string(),
            scope: scope.clone(),
            archived: false,
            access_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: Some("ephemeral".to_string()),
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };

        persist_capture_entry(
            server,
            item.target_db,
            item.named_project.as_deref(),
            item.db_path.as_ref(),
            &cache_entry,
        )?;
        // PR #2 / Q4: intentionally NOT calling queue_capture_enrichment.
        // See module-level docs for the rationale.
        updated += 1;
    }

    Ok(updated)
}
