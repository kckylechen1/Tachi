use serde_json::json;

use super::cache::{recall_cache_key, recall_cache_read_enabled, recall_cache_ttl_secs};
use super::exact::has_high_confidence_exact_token_top;
use super::rows::search_memory_rows_with_access;
use crate::memory_search_ops::search_helpers::{normalize_json_relevance, search_score};
use crate::tool_params::SearchMemoryParams;
use crate::utils::stable_hash;
use crate::MemoryServer;

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

    // ── Recall-cache read short-circuit ──────────────────────────────────
    // A fresh hit returns the rendered rows verbatim, skipping the entire
    // hybrid-search (+ optional rerank) round trip. Gated behind
    // TACHI_ENABLE_RECALL_CACHE. We never key on a caller-supplied embedding
    // (the key is the query text) or cache trivial queries. The cache lives in
    // the global DB so cross-DB merged results have a single home. A cache hit
    // intentionally does not bump per-memory access_count (skipping the search
    // is the whole point); hit_count on the cache row carries the telemetry.
    let cache_key = (recall_cache_read_enabled()
        && params.query_vec.is_none()
        && !memory_core::should_skip_query(&params.query))
    .then(|| recall_cache_key(&params, top_k, project_only));

    if let Some(ref key) = cache_key {
        let ttl = recall_cache_ttl_secs();
        if let Ok(Some(hit)) = server.with_global_store_read(|store| {
            store
                .recall_cache_lookup(key, ttl)
                .map_err(|e| e.to_string())
        }) {
            // If the caller asked for a reranked ordering but the cache only
            // holds the hybrid one, fall through and do the real work.
            if !params.enable_rerank || hit.reranked {
                let server_clone = (*server).clone();
                let key_clone = key.clone();
                std::mem::drop(tokio::task::spawn_blocking(move || {
                    let _ = server_clone.with_global_store(|store| {
                        store
                            .recall_cache_record_hit(&key_clone)
                            .map_err(|e| e.to_string())
                    });
                }));
                return Ok(hit.rows_json);
            }
        }
    }

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
    let serialized =
        serde_json::to_string(&rows).map_err(|e| format!("Failed to serialize response: {}", e))?;

    // ── Recall-cache write-through ───────────────────────────────────────
    // Cache the rendered rows so the next identical query short-circuits.
    // Skip empty result sets so a transiently-empty answer never masks
    // newly-added memories until the TTL elapses. `reranked` records whether
    // this run actually reranked, so the read side can honor rerank intent.
    if let Some(key) = cache_key {
        if !rows.is_empty() {
            let _ = server.with_global_store(|store| {
                store
                    .recall_cache_store(
                        &key,
                        &params.query,
                        &serialized,
                        rows.len() as i64,
                        params.enable_rerank,
                    )
                    .map_err(|e| e.to_string())
            });
        }
    }
    Ok(serialized)
}
