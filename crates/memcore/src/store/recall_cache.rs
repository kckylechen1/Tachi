//! Recall-cache methods on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

impl MemoryStore {
    /// Read the DB-authoritative generation used to validate search-cache
    /// entries across independent processes. Missing/corrupt metadata is an
    /// error so callers can bypass the cache safely.
    pub fn search_generation(&self) -> Result<i64, MemoryError> {
        db::search_generation(&self.conn)
    }

    /// Recall-cache lookup by a precomputed context-hash key. Returns a fresh
    /// hit (within `ttl_secs`; non-positive disables the freshness check) or
    /// `None`. The key is opaque here — callers build it from the query
    /// context. See `db::recall_cache`.
    pub fn recall_cache_lookup(
        &self,
        cache_id: &str,
        ttl_secs: i64,
    ) -> Result<Option<db::RecallCacheHit>, MemoryError> {
        db::recall_cache_get(&self.conn, cache_id, ttl_secs, chrono::Utc::now())
    }

    /// Write-through a rendered result set into the recall cache. `reranked`
    /// marks entries upgraded by the background rerank job.
    pub fn recall_cache_store(
        &self,
        cache_id: &str,
        generation_fingerprint: &str,
        query: &str,
        rows_json: &str,
        result_count: i64,
        reranked: bool,
    ) -> Result<(), MemoryError> {
        db::recall_cache_put(
            &self.conn,
            cache_id,
            generation_fingerprint,
            query,
            rows_json,
            result_count,
            reranked,
            &db::now_utc_iso(),
        )
    }

    /// Best-effort hit telemetry bump for a cache id.
    pub fn recall_cache_record_hit(&self, cache_id: &str) -> Result<(), MemoryError> {
        db::recall_cache_record_hit(&self.conn, cache_id, &db::now_utc_iso())
    }

    /// Aggregate recall-cache stats for diagnostics.
    pub fn recall_cache_stats(&self) -> Result<db::RecallCacheStats, MemoryError> {
        db::recall_cache_stats(&self.conn)
    }

    /// Delete recall-cache entries older than `cutoff_rfc3339` (housekeeping).
    pub fn recall_cache_purge_stale(&self, cutoff_rfc3339: &str) -> Result<usize, MemoryError> {
        db::recall_cache_purge_stale(&self.conn, cutoff_rfc3339)
    }

    /// Write-side cache bust: clear every recall-cache row unconditionally
    /// (tachi#1435 slice 3 / #2059). Callers invoke this after a save commits
    /// so the next search never replays a pre-save cached answer. See
    /// `db::recall_cache_invalidate_all`.
    pub fn recall_cache_invalidate_all(&self) -> Result<usize, MemoryError> {
        db::recall_cache_invalidate_all(&self.conn)
    }
}
