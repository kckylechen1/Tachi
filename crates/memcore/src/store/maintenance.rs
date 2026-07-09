//! Daily truth-maintenance helpers on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryEntry, MemoryStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierHealthCounts {
    pub total_active: i64,
    pub consolidated: i64,
}

impl MemoryStore {
    /// Archive low-importance memories that were never accessed and are older
    /// than 60 days, sparing permanent/pinned/durable retention policies.
    pub fn archive_stale_low_value_memories(&self) -> Result<usize, MemoryError> {
        Ok(self.conn.execute(
            "UPDATE memories
             SET archived = 1, updated_at = datetime('now')
             WHERE archived = 0
               AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned', 'durable')
               AND importance < 0.70
               AND access_count = 0
               AND julianday(COALESCE(NULLIF(created_at, ''), timestamp)) < julianday('now', '-60 days')",
            [],
        )?)
    }

    /// Count active memories and how many reached consolidated/pattern tier.
    pub fn tier_health_counts(&self) -> Result<TierHealthCounts, MemoryError> {
        let total_active: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE archived = 0",
            [],
            |r| r.get(0),
        )?;
        let consolidated: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE archived = 0 AND tier IN ('consolidated','pattern')",
            [],
            |r| r.get(0),
        )?;
        Ok(TierHealthCounts {
            total_active,
            consolidated,
        })
    }

    /// Promote raw memories that earned consolidation through repeated exact
    /// recall from diverse queries (the same gate as `record_access`); a raw
    /// note must not be promoted merely because it was accessed often.
    pub fn promote_diversely_recalled_raw_memories(&self) -> Result<usize, MemoryError> {
        Ok(self.conn.execute(
            "UPDATE memories
             SET tier = 'consolidated', updated_at = datetime('now')
             WHERE archived = 0
               AND tier = 'raw'
               AND recall_count >= 3
               AND query_diversity >= 3
               AND COALESCE(retention_policy, '') NOT IN ('ephemeral')",
            [],
        )?)
    }

    /// Load active non-raw entries that still lack an embedding vector,
    /// highest importance first.
    pub fn entries_missing_vectors(&self, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id FROM memories m
             LEFT JOIN memories_vec v ON m.id = v.id
             WHERE m.archived = 0
               AND m.tier != 'raw'
               AND v.id IS NULL
             ORDER BY m.importance DESC
             LIMIT ?1",
        )?;
        let ids = stmt
            .query_map([limit as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(db::fetch_by_ids(&self.conn, &ids, false)?
            .into_values()
            .collect())
    }

    /// Load the most-accessed active entries eligible for durable promotion.
    pub fn promotion_candidate_entries(
        &self,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM memories
             WHERE archived = 0
               AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned')
             ORDER BY access_count DESC, timestamp DESC
             LIMIT ?1",
        )?;
        let ids = stmt
            .query_map([limit as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(db::fetch_by_ids(&self.conn, &ids, false)?
            .into_values()
            .collect())
    }

    /// Count the distinct days on which a memory was accessed.
    pub fn distinct_access_days(&self, id: &str) -> Result<usize, MemoryError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT date(accessed_at)) FROM access_history WHERE memory_id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Pin importance and durable retention once a memory passes the
    /// promotion score gate.
    pub fn promote_memory_to_durable(&self, id: &str) -> Result<(), MemoryError> {
        self.conn.execute(
            "UPDATE memories
             SET importance = 0.7, retention_policy = 'durable', updated_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    /// Count non-archived memories (split-brain and health diagnostics).
    pub fn count_active_memories(&self) -> Result<i64, MemoryError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE archived = 0",
            [],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::types::MemoryEntry;
    use crate::MemoryStore;

    fn test_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: format!("{id} summary"),
            text: format!("{id} text"),
            importance: 0.5,
            timestamp: "2026-07-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn backdate_created_at(store: &MemoryStore, id: &str) {
        store
            .connection()
            .execute(
                "UPDATE memories SET created_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
                [id],
            )
            .expect("backdate created_at");
    }

    #[test]
    fn archive_stale_low_value_memories_spares_recent_and_durable_rows() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("stale")).expect("seed stale");
        let mut durable = test_entry("durable");
        durable.retention_policy = Some("durable".to_string());
        store.upsert(&durable).expect("seed durable");
        store.upsert(&test_entry("recent")).expect("seed recent");
        backdate_created_at(&store, "stale");
        backdate_created_at(&store, "durable");

        let archived = store
            .archive_stale_low_value_memories()
            .expect("archive stale");
        assert_eq!(archived, 1);
        assert!(
            store
                .get_with_options("stale", true)
                .expect("read stale")
                .expect("stale exists")
                .archived
        );
        assert!(
            !store
                .get("durable")
                .expect("read durable")
                .expect("durable exists")
                .archived
        );
    }

    #[test]
    fn tier_health_counts_and_diverse_recall_promotion() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut promotable = test_entry("promotable");
        promotable.recall_count = 3;
        promotable.query_diversity = 3;
        store.upsert(&promotable).expect("seed promotable");
        let mut narrow = test_entry("narrow");
        narrow.recall_count = 5;
        narrow.query_diversity = 1;
        store.upsert(&narrow).expect("seed narrow");
        let mut consolidated = test_entry("consolidated");
        consolidated.tier = "consolidated".to_string();
        store.upsert(&consolidated).expect("seed consolidated");

        let counts = store.tier_health_counts().expect("tier health");
        assert_eq!(counts.total_active, 3);
        assert_eq!(counts.consolidated, 1);

        let promoted = store
            .promote_diversely_recalled_raw_memories()
            .expect("promote raw");
        assert_eq!(promoted, 1);
        let entry = store
            .get("promotable")
            .expect("read promotable")
            .expect("promotable exists");
        assert_eq!(entry.tier, "consolidated");
        let entry = store
            .get("narrow")
            .expect("read narrow")
            .expect("narrow exists");
        assert_eq!(entry.tier, "raw");
    }

    #[test]
    fn entries_missing_vectors_skips_raw_and_embedded_entries() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        assert!(store.vec_available, "sqlite-vec required for this test");
        let mut missing = test_entry("missing");
        missing.tier = "consolidated".to_string();
        store.upsert(&missing).expect("seed missing");
        let mut embedded = test_entry("embedded");
        embedded.tier = "consolidated".to_string();
        embedded.vector = Some(vec![0.5; 1024]);
        store.upsert(&embedded).expect("seed embedded");
        store.upsert(&test_entry("raw-entry")).expect("seed raw");

        let entries = store.entries_missing_vectors(50).expect("scan vectors");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["missing"]);
    }

    #[test]
    fn promotion_candidates_and_durable_promotion() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("candidate")).expect("seed entry");
        let mut permanent = test_entry("permanent");
        permanent.retention_policy = Some("permanent".to_string());
        store.upsert(&permanent).expect("seed permanent");

        let entries = store
            .promotion_candidate_entries(200)
            .expect("scan candidates");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["candidate"]);

        store
            .promote_memory_to_durable("candidate")
            .expect("promote candidate");
        let entry = store
            .get("candidate")
            .expect("read candidate")
            .expect("candidate exists");
        assert_eq!(entry.importance, 0.7);
        assert_eq!(entry.retention_policy.as_deref(), Some("durable"));
    }

    #[test]
    fn distinct_access_days_counts_unique_dates() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("tracked")).expect("seed entry");
        for accessed_at in [
            "2026-07-01T08:00:00Z",
            "2026-07-01T20:00:00Z",
            "2026-07-02T08:00:00Z",
        ] {
            store
                .connection()
                .execute(
                    "INSERT INTO access_history (memory_id, accessed_at) VALUES (?1, ?2)",
                    ["tracked", accessed_at],
                )
                .expect("insert access row");
        }

        assert_eq!(
            store.distinct_access_days("tracked").expect("count days"),
            2
        );
        assert_eq!(store.distinct_access_days("other").expect("count days"), 0);
    }

    #[test]
    fn count_active_memories_excludes_archived() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("active")).expect("seed active");
        let mut archived = test_entry("archived");
        archived.archived = true;
        store.upsert(&archived).expect("seed archived");

        assert_eq!(store.count_active_memories().expect("count active"), 1);
    }
}
