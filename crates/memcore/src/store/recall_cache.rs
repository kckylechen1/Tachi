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

#[cfg(test)]
mod tests {
    use super::*;

    /// tachi#1432: same canonical-shape idiom pinned in `db/common.rs`'s
    /// `canonical_shape()`/`assert_canonical` — millisecond precision, `Z`
    /// suffix, no numeric offset.
    fn assert_canonical_timestamp_shape(ts: &str) {
        let canonical =
            regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap();
        assert!(
            canonical.is_match(ts),
            "not canonical Millis+Z shape: {ts:?}"
        );
    }

    /// tachi#1432 CONCERN 3: `recall_cache_store`'s stamp
    /// (`&db::now_utc_iso()` above) must land in `recall_cache.created_at`/
    /// `updated_at` as canonical millis+Z shape, not a bare
    /// `Utc::now().to_rfc3339()`. Reintroducing the bare form at this call
    /// site must RED here, not just in the renderer's own unit test.
    #[test]
    fn recall_cache_store_writes_canonical_shape_timestamps() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .recall_cache_store("rc:shape", "gen-1", "q", "[]", 0, false)
            .unwrap();
        let (created_at, updated_at): (String, String) = store
            .connection()
            .query_row(
                "SELECT created_at, updated_at FROM recall_cache WHERE cache_id = 'rc:shape'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_canonical_timestamp_shape(&created_at);
        assert_canonical_timestamp_shape(&updated_at);
    }
}
