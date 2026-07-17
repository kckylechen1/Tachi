use super::cache::{recall_cache_key, recall_cache_read_enabled, recall_cache_ttl_secs};
use super::rows::{query_with_context_symbols, search_memory_rows_with_access};
use crate::memory_search_ops::{
    apply_search_rerank_policy, expand_search_params_for_rerank, normalize_json_relevance,
};
use crate::tool_params::SearchMemoryParams;
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
    mut params: SearchMemoryParams,
    project_only: bool,
    record_access: bool,
) -> Result<String, String> {
    params.query = query_with_context_symbols(&params.query, &params.context_symbols);
    let top_k = params.normalized_top_k();

    // ── Recall-cache read short-circuit ──────────────────────────────────
    // A fresh hit returns the rendered rows verbatim, skipping the entire
    // hybrid-search (+ optional rerank) round trip. Gated behind
    // TACHI_ENABLE_RECALL_CACHE. We never key on a caller-supplied embedding
    // (the key is the query text) or cache trivial queries. The cache lives in
    // the global DB so cross-DB merged results have a single home. A cache hit
    // intentionally does not bump per-memory access_count (skipping the search
    // is the whole point); hit_count on the cache row carries the telemetry.
    let sandboxed_search = params
        .agent_role
        .as_deref()
        .is_some_and(|role| !role.trim().is_empty());
    let cache_key = (recall_cache_read_enabled()
        && params.query_vec.is_none()
        && !sandboxed_search
        && !memcore::should_skip_query(&params.query))
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
                    if let Err(error) = server_clone.with_global_store(|store| {
                        store
                            .recall_cache_record_hit(&key_clone)
                            .map_err(|e| e.to_string())
                    }) {
                        tracing::warn!(error = %error, "failed to record recall-cache hit");
                    }
                }));
                return Ok(hit.rows_json);
            }
        }
    }

    let mut search_params = params.clone();
    expand_search_params_for_rerank(&mut search_params, top_k);
    let mut rows =
        search_memory_rows_with_access(server, search_params, project_only, record_access).await?;
    let (reranked_rows, _rerank_policy) =
        apply_search_rerank_policy(server, &params.query, rows, top_k, params.enable_rerank).await;
    rows = reranked_rows;
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
            if let Err(error) = server.with_global_store(|store| {
                store
                    .recall_cache_store(
                        &key,
                        &params.query,
                        &serialized,
                        rows.len() as i64,
                        params.enable_rerank,
                    )
                    .map_err(|e| e.to_string())
            }) {
                tracing::warn!(error = %error, "failed to persist recall cache entry");
            }
        }
    }
    Ok(serialized)
}
