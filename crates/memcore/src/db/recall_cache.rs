//! Recall-cache persistence.
//!
//! Stores rendered hybrid-search result rows (a JSON array, exactly as the
//! recall path would have returned them) keyed by an opaque context hash that
//! the caller computes from `(query, project, path_prefix, top_k, …)`. This
//! module is deliberately ignorant of how the key is built — it only reads and
//! writes rows by id, with a TTL freshness check at read time.
//!
//! See `schema.rs::recall_cache` for the table definition and the rationale
//! for keeping these rows out of the `memories` table.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

/// A fresh cache hit: the stored rendered rows JSON plus whether it was
/// produced by a background rerank upgrade (`reranked = 1`).
pub struct RecallCacheHit {
    pub rows_json: String,
    pub reranked: bool,
}

/// Aggregate diagnostics for the recall cache (surfaced in status output).
#[derive(Debug, Default, Clone)]
pub struct RecallCacheStats {
    pub entries: i64,
    pub reranked_entries: i64,
    pub total_hits: i64,
    pub oldest_updated: String,
    pub newest_updated: String,
}

/// Look up a cache entry by id. Returns the hit only when it is within
/// `ttl_secs` of its `updated_at`; stale or unparseable rows are reported as a
/// miss (and left for the next write-through / purge to overwrite). A
/// non-positive `ttl_secs` disables the freshness check.
pub fn recall_cache_get(
    conn: &Connection,
    cache_id: &str,
    ttl_secs: i64,
    now: DateTime<Utc>,
) -> Result<Option<RecallCacheHit>, MemoryError> {
    let row = conn
        .query_row(
            "SELECT rows_json, reranked, updated_at FROM recall_cache WHERE cache_id = ?1",
            params![cache_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((rows_json, reranked, updated_at)) = row else {
        return Ok(None);
    };
    if ttl_secs > 0 {
        let Ok(updated) = DateTime::parse_from_rfc3339(&updated_at) else {
            return Ok(None);
        };
        let age = now.signed_duration_since(updated.with_timezone(&Utc));
        if age.num_seconds() > ttl_secs {
            return Ok(None);
        }
    }
    Ok(Some(RecallCacheHit {
        rows_json,
        reranked: reranked != 0,
    }))
}

/// Insert or refresh a cache entry. `created_at` is preserved across updates so
/// age-based diagnostics reflect first-seen, while the TTL uses `updated_at`.
pub fn recall_cache_put(
    conn: &Connection,
    cache_id: &str,
    query: &str,
    rows_json: &str,
    result_count: i64,
    reranked: bool,
    now_rfc3339: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO recall_cache
            (cache_id, query, rows_json, result_count, reranked, hit_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)
         ON CONFLICT(cache_id) DO UPDATE SET
            query        = excluded.query,
            rows_json    = excluded.rows_json,
            result_count = excluded.result_count,
            reranked     = excluded.reranked,
            updated_at   = excluded.updated_at",
        params![
            cache_id,
            query,
            rows_json,
            result_count,
            reranked as i64,
            now_rfc3339
        ],
    )?;
    Ok(())
}

/// Best-effort hit telemetry: bump `hit_count` and stamp `last_hit_at`.
pub fn recall_cache_record_hit(
    conn: &Connection,
    cache_id: &str,
    now_rfc3339: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE recall_cache SET hit_count = hit_count + 1, last_hit_at = ?2 WHERE cache_id = ?1",
        params![cache_id, now_rfc3339],
    )?;
    Ok(())
}

/// Delete entries whose `updated_at` is older than `cutoff_rfc3339`. Returns the
/// number of rows removed. Used by housekeeping; not required for correctness
/// because reads already TTL-gate stale rows.
pub fn recall_cache_purge_stale(
    conn: &Connection,
    cutoff_rfc3339: &str,
) -> Result<usize, MemoryError> {
    let removed = conn.execute(
        "DELETE FROM recall_cache WHERE updated_at < ?1",
        params![cutoff_rfc3339],
    )?;
    Ok(removed)
}

/// Unconditionally clear every recall-cache row (tachi#1435 slice 3 / #2059).
///
/// A save must never be masked by a stale cached search result — unlike
/// `recall_cache_purge_stale` (age-gated housekeeping), this is the write-side
/// cache-bust: it runs after every successful save commit, regardless of any
/// row's TTL, so the very next search recomputes instead of replaying a
/// pre-save answer. Blunt (whole-table) by design for this slice; precise
/// per-key invalidation is explicitly out of scope (see the frozen spec's
/// "不做" list).
pub fn recall_cache_invalidate_all(conn: &Connection) -> Result<usize, MemoryError> {
    let removed = conn.execute("DELETE FROM recall_cache", [])?;
    Ok(removed)
}

/// Aggregate stats for diagnostics.
pub fn recall_cache_stats(conn: &Connection) -> Result<RecallCacheStats, MemoryError> {
    let stats = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(reranked), 0),
                COALESCE(SUM(hit_count), 0),
                COALESCE(MIN(updated_at), ''),
                COALESCE(MAX(updated_at), '')
         FROM recall_cache",
        [],
        |r| {
            Ok(RecallCacheStats {
                entries: r.get(0)?,
                reranked_entries: r.get(1)?,
                total_hits: r.get(2)?,
                oldest_updated: r.get(3)?,
                newest_updated: r.get(4)?,
            })
        },
    )?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reuse the real store open path so the `simple` FTS tokenizer + full
    // schema (including `recall_cache`) are present — a bare in-memory
    // Connection + init_schema would fail on the FTS5 virtual table.
    fn store() -> crate::MemoryStore {
        crate::MemoryStore::open_in_memory().unwrap()
    }

    #[test]
    fn put_then_get_roundtrips_within_ttl() {
        let s = store();
        let c = &s.conn;
        let now = Utc::now();
        recall_cache_put(
            c,
            "rc:1",
            "q",
            "[{\"id\":\"a\"}]",
            1,
            false,
            &now.to_rfc3339(),
        )
        .unwrap();
        let hit = recall_cache_get(c, "rc:1", 900, now).unwrap();
        assert!(hit.is_some());
        let hit = hit.unwrap();
        assert_eq!(hit.rows_json, "[{\"id\":\"a\"}]");
        assert!(!hit.reranked);
    }

    #[test]
    fn get_misses_when_stale() {
        let s = store();
        let c = &s.conn;
        let written = Utc::now() - chrono::Duration::seconds(3600);
        recall_cache_put(c, "rc:2", "q", "[]", 0, false, &written.to_rfc3339()).unwrap();
        // ttl 900s, written 3600s ago → stale → miss
        assert!(recall_cache_get(c, "rc:2", 900, Utc::now())
            .unwrap()
            .is_none());
        // ttl 0 disables the freshness check → hit
        assert!(recall_cache_get(c, "rc:2", 0, Utc::now())
            .unwrap()
            .is_some());
    }

    #[test]
    fn get_misses_for_unknown_key() {
        let s = store();
        let c = &s.conn;
        assert!(recall_cache_get(c, "rc:missing", 900, Utc::now())
            .unwrap()
            .is_none());
    }

    #[test]
    fn put_upserts_and_preserves_created_at() {
        let s = store();
        let c = &s.conn;
        let t0 = Utc::now() - chrono::Duration::seconds(10);
        recall_cache_put(c, "rc:3", "q1", "[1]", 1, false, &t0.to_rfc3339()).unwrap();
        let t1 = Utc::now();
        recall_cache_put(c, "rc:3", "q2", "[1,2]", 2, true, &t1.to_rfc3339()).unwrap();
        let created: String = c
            .query_row(
                "SELECT created_at FROM recall_cache WHERE cache_id='rc:3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            created,
            t0.to_rfc3339(),
            "created_at preserved across upsert"
        );
        let hit = recall_cache_get(c, "rc:3", 900, Utc::now())
            .unwrap()
            .unwrap();
        assert_eq!(hit.rows_json, "[1,2]");
        assert!(hit.reranked, "reranked flag updated on upsert");
    }

    #[test]
    fn record_hit_and_purge() {
        let s = store();
        let c = &s.conn;
        let now = Utc::now();
        recall_cache_put(c, "rc:4", "q", "[1]", 1, false, &now.to_rfc3339()).unwrap();
        recall_cache_record_hit(c, "rc:4", &now.to_rfc3339()).unwrap();
        let stats = recall_cache_stats(c).unwrap();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.total_hits, 1);
        // purge everything older than "now + 1s" → removes the row
        let cutoff = (now + chrono::Duration::seconds(1)).to_rfc3339();
        assert_eq!(recall_cache_purge_stale(c, &cutoff).unwrap(), 1);
        assert_eq!(recall_cache_stats(c).unwrap().entries, 0);
    }

    #[test]
    fn invalidate_all_clears_every_row_regardless_of_ttl() {
        let s = store();
        let c = &s.conn;
        let now = Utc::now();
        // A fresh row (well within any TTL) and a stale one — invalidate_all
        // must not TTL-gate; both must be gone afterward.
        recall_cache_put(c, "rc:fresh", "q", "[1]", 1, false, &now.to_rfc3339()).unwrap();
        let old = now - chrono::Duration::seconds(10_000);
        recall_cache_put(c, "rc:stale", "q", "[2]", 1, false, &old.to_rfc3339()).unwrap();
        assert_eq!(recall_cache_stats(c).unwrap().entries, 2);

        let removed = recall_cache_invalidate_all(c).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(recall_cache_stats(c).unwrap().entries, 0);
        assert!(recall_cache_get(c, "rc:fresh", 0, now).unwrap().is_none());
    }
}
