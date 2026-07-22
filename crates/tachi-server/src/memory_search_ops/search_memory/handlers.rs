use super::cache::{
    recall_cache_epoch, recall_cache_key, recall_cache_read_enabled, recall_cache_ttl_secs,
    recall_cache_write_through,
};
use super::rows::{query_with_context_symbols, search_memory_rows_with_access};
use crate::agent_markdown::{format_search_memory_markdown, wants_explicit_json};
use crate::memory_search_ops::{
    apply_search_rerank_policy, expand_search_params_for_rerank, normalize_json_relevance,
};
use crate::tool_params::SearchMemoryParams;
use crate::MemoryServer;

/// Render a canonical serialized-rows JSON string per `params.format`
/// (tachi#1201 k3): defaults to markdown, "json" opts into the raw string
/// unchanged. Used on both the cache-hit short-circuit and the fresh-compute
/// path so caching stays keyed on the canonical JSON regardless of which
/// format a given caller asked for.
fn render_search_response(
    query: &str,
    format: Option<&str>,
    serialized_rows: String,
) -> Result<String, String> {
    if wants_explicit_json(format) {
        return Ok(serialized_rows);
    }
    let rows: serde_json::Value = serde_json::from_str(&serialized_rows)
        .map_err(|e| format!("Failed to parse rows for markdown rendering: {e}"))?;
    Ok(format_search_memory_markdown(query, &rows))
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

    // tachi#1435 slice 4 / #2059 codex round 2: snapshot the cache epoch
    // BEFORE touching the store at all (read lookup or the miss-path
    // fresh-compute below). If a concurrent save/enrichment/contradiction
    // commits + invalidates anywhere between this snapshot and this search's
    // own write-through, the epoch compare at the write-through site (below)
    // will see a mismatch and discard this run's (now possibly stale)
    // result instead of resurrecting it into the cache. See
    // `search_memory::cache`'s module doc for the full race + the
    // cross-process safety boundary this in-memory counter relies on.
    let epoch_at_read = recall_cache_epoch();

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
                return render_search_response(
                    &params.query,
                    params.format.as_deref(),
                    hit.rows_json,
                );
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
    //
    // Epoch guard (tachi#1435 slice 4 / #2059 codex round 2, TOCTOU-closed in
    // round 3 — see `search_memory::cache`'s module doc for the full
    // mutual-exclusion invariant): `rows` above was computed from a store
    // snapshot taken sometime after `epoch_at_read` — if a
    // save/enrichment/contradiction/auto-link committed AND invalidated in
    // the meantime, writing `rows` now would resurrect exactly the stale
    // content that invalidation was trying to clear. The recheck MUST run
    // INSIDE this `with_global_store` closure (not before it) — that closure
    // holds the same `global_rw_gate` write lock
    // `invalidate_recall_cache_after_write`'s DELETE+bump holds, so the two
    // can never interleave; checking outside the lock and only writing
    // inside it would reopen the exact race this guard exists to close.
    // Discarding is always safe: the next miss just recomputes fresh.
    if let Some(key) = cache_key {
        if !rows.is_empty() {
            let wrote = recall_cache_write_through(
                server,
                epoch_at_read,
                &key,
                &params.query,
                &serialized,
                rows.len() as i64,
                params.enable_rerank,
            );
            if matches!(wrote, Ok(false)) {
                tracing::debug!(
                    "[recall_cache] discarding stale write-through for {key} — \
                     epoch advanced (concurrent invalidation) between read and write"
                );
            }
        }
    }
    render_search_response(&params.query, params.format.as_deref(), serialized)
}
