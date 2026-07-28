//! Daily truth-maintenance helpers on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryEntry, MemoryStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierHealthCounts {
    pub total_active: i64,
    pub consolidated: i64,
}

impl MemoryStore {
    /// Archive low-importance memories that were never surfaced by search and
    /// are older than 60 days, sparing permanent/pinned/durable retention
    /// policies.
    ///
    /// tachi#1459: the `access_count = 0` predicate below observes the search
    /// path only; reads through path-listing routes do not increment it. "Never
    /// accessed" here means "`hybrid_search` never returned it", so a row read
    /// constantly through `list_by_path` / `list_by_path_recent` /
    /// `list_memories_by_path_prefix` still qualifies. The other predicates are
    /// what currently keep that from destroying a live row: rows under
    /// `/handoff*` and `/kanban*` with no policy of their own are backfilled to
    /// `pinned`, and `/wiki*` / `/guide*` to `permanent`
    /// (`backfill_retention_defaults` in `db/schema.rs`), and the retention
    /// filter spares those — but that is a second mechanism doing the work, not
    /// this predicate meaning what it reads like.
    pub fn archive_stale_low_value_memories(&self) -> Result<usize, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self.conn.unchecked_transaction()?;
        let archived_at = db::now_utc_iso();
        let mut stmt = tx.prepare(
            "UPDATE memories
             SET archived = 1, updated_at = ?1, revision = revision + 1
             WHERE archived = 0
               AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned', 'durable')
               AND importance < 0.70
               AND access_count = 0
               AND julianday(COALESCE(NULLIF(created_at, ''), timestamp)) < julianday('now', '-60 days')
             RETURNING id",
        )?;
        let mut ids = stmt
            .query_map([&archived_at], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        ids.sort();
        if !ids.is_empty() {
            db::write_gc_archived_receipt(
                &tx,
                &archived_at,
                60,
                serde_json::json!([{
                    "predicate": "stale_low_value_never_search_accessed",
                    "recency_column": "created_at_or_timestamp",
                    "importance_below": 0.70,
                    "retention_scope": "not_permanent_pinned_or_durable",
                    "archived_count": ids.len(),
                    "memory_ids": ids,
                }]),
                ids.len(),
                "memcore::store::maintenance::MemoryStore::archive_stale_low_value_memories",
            )?;
        }
        tx.commit()?;
        Ok(ids.len())
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
    ///
    /// tachi#1459: both counters this gate reads observe the search path only;
    /// reads through path-listing routes do not increment them. This is the
    /// batch twin of the inline gate in `db::record_access_with_updates` and it
    /// inherits the same partial view — it can only fail to promote a
    /// path-listed memory, never promote one on evidence it did not earn.
    pub fn promote_diversely_recalled_raw_memories(
        &self,
        recall_config: &crate::RecallConfig,
    ) -> Result<usize, MemoryError> {
        if recall_config.use_provenance_recency {
            return Ok(0);
        }
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
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

    /// Load active entries that still lack an embedding vector, highest
    /// importance first (non-raw rows before raw when raw embedding is enabled).
    /// Excludes `anchor:`-prefixed rows (tachi#773 item 4: anchors are plumbing
    /// rows for the memory graph, never content that should burn embedding
    /// budget or surface as recall).
    pub fn entries_missing_vectors(&self, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        let tier_filter = crate::embed_config::embed_raw_tier_sql_filter("m.");
        let order_by = crate::embed_config::embed_selection_order_by("m.");
        let sql = format!(
            "SELECT m.id FROM memories m
             LEFT JOIN memories_vec v ON m.id = v.id
             WHERE m.archived = 0
               {tier_filter}
               AND m.id NOT LIKE 'anchor:%'
               AND v.id IS NULL
             {order_by}
             LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let ids = stmt
            .query_map([limit as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let fetched = db::fetch_by_ids(&self.conn, &ids, false)?;
        Ok(ids
            .into_iter()
            .filter_map(|id| fetched.get(&id).cloned())
            .collect())
    }

    /// Load the most-accessed active entries eligible for durable promotion.
    ///
    /// tachi#1459: the `access_count DESC` ordering observes the search path
    /// only; reads through path-listing routes do not increment it. This ranks
    /// by how often search has shown a memory, so a memory reached only by path
    /// listing sorts to the bottom of this list however heavily it is read.
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

    /// Load promotion candidates using the same provenance arm as the daily
    /// promotion gate. OFF preserves [`Self::promotion_candidate_entries`]'s
    /// literal historical ordering.
    pub fn promotion_candidate_entries_for_config(
        &self,
        limit: usize,
        recall_config: &crate::RecallConfig,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !recall_config.use_provenance_recency {
            return self.promotion_candidate_entries(limit);
        }
        let mut stmt = self.conn.prepare(
            "SELECT m.id FROM memories m
             LEFT JOIN access_history ah
               ON ah.memory_id = m.id AND ah.event_kind = 'use'
             WHERE m.archived = 0
               AND COALESCE(m.retention_policy, '') NOT IN ('permanent', 'pinned')
             GROUP BY m.id
             ORDER BY COUNT(DISTINCT date(ah.accessed_at)) DESC, m.timestamp DESC
             LIMIT ?1",
        )?;
        let ids = stmt
            .query_map([limit as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(db::fetch_by_ids(&self.conn, &ids, false)?
            .into_values()
            .collect())
    }

    /// Count distinct access days using the pre-#1446 unfiltered semantics.
    pub fn distinct_access_days(&self, id: &str) -> Result<usize, MemoryError> {
        db::count_distinct_access_days(&self.conn, id)
    }

    /// Count the distinct days feeding the durable-promotion gate. The config
    /// selects the frozen unfiltered OFF arm or the use-only ON arm.
    pub fn distinct_promotion_days(
        &self,
        id: &str,
        recall_config: &crate::RecallConfig,
    ) -> Result<usize, MemoryError> {
        db::count_distinct_promotion_days(&self.conn, id, recall_config)
    }

    /// Pin importance and durable retention once a memory passes the
    /// promotion score gate.
    pub fn promote_memory_to_durable(&self, id: &str) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
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

    use crate::db::AccessEventKind;
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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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
    fn unattended_archival_is_revision_safe_and_receipted_without_noop_events() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("stale-a")).expect("seed stale-a");
        store.upsert(&test_entry("stale-b")).expect("seed stale-b");
        let mut durable = test_entry("durable");
        durable.retention_policy = Some("durable".to_string());
        store.upsert(&durable).expect("seed durable");
        for id in ["stale-a", "stale-b", "durable"] {
            backdate_created_at(&store, id);
        }
        let pre_archive_revision = store.get("stale-a").unwrap().unwrap().revision;
        let pre_archive_updated_at: String = store
            .connection()
            .query_row(
                "SELECT updated_at FROM memories WHERE id = 'stale-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(store.archive_stale_low_value_memories().unwrap(), 2);
        let post_archive_updated_at: String = store
            .connection()
            .query_row(
                "SELECT updated_at FROM memories WHERE id = 'stale-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(post_archive_updated_at, pre_archive_updated_at);
        assert!(
            !store
                .restore_archived_if_revision("stale-a", pre_archive_revision)
                .unwrap(),
            "a pre-archive revision must not restore an unattended archival"
        );
        assert!(!store.get("durable").unwrap().unwrap().archived);

        let payload: String = store
            .connection()
            .query_row(
                "SELECT payload_json FROM tachi_events WHERE event_type = 'memory.gc_archived'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["stale_days"], json!(60));
        assert_eq!(payload["archived_at"], json!(post_archive_updated_at));
        let archived_at = payload["archived_at"].as_str().unwrap();
        let (_, millis_and_z) = archived_at.rsplit_once('.').unwrap();
        assert!(archived_at.ends_with('Z'));
        assert_eq!(
            millis_and_z.len(),
            4,
            "timestamp must use millisecond UTC form"
        );
        let pass = &payload["passes"][0];
        assert_eq!(
            pass["predicate"],
            json!("stale_low_value_never_search_accessed")
        );
        assert_eq!(pass["archived_count"], json!(2));
        assert_eq!(pass["memory_ids"], json!(["stale-a", "stale-b"]));
        assert_eq!(store.archive_stale_low_value_memories().unwrap(), 0);
        let receipt_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM tachi_events WHERE event_type = 'memory.gc_archived'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipt_count, 1, "a no-op write must not emit a receipt");
    }

    #[test]
    fn unattended_archival_receipt_names_every_row_beyond_the_former_sample_boundary() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let expected_ids: Vec<String> = (0..501)
            .map(|index| format!("unattended-arch-{index:03}"))
            .collect();
        for id in &expected_ids {
            store.upsert(&test_entry(id)).expect("seed stale row");
            backdate_created_at(&store, id);
        }

        assert_eq!(
            store.archive_stale_low_value_memories().unwrap(),
            expected_ids.len()
        );
        let payload: String = store
            .connection()
            .query_row(
                "SELECT payload_json FROM tachi_events WHERE event_type = 'memory.gc_archived'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let pass = &payload["passes"][0];
        let ids: Vec<String> = serde_json::from_value(pass["memory_ids"].clone()).unwrap();
        assert_eq!(pass["archived_count"], json!(expected_ids.len()));
        assert_eq!(ids, expected_ids);
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
            .promote_diversely_recalled_raw_memories(&crate::RecallConfig {
                use_provenance_recency: false,
                ..crate::RecallConfig::default()
            })
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
    fn entries_missing_vectors_embed_selection_and_tier_gate() {
        struct EmbedRawTierEnvRestore {
            saved: Option<std::ffi::OsString>,
        }

        impl EmbedRawTierEnvRestore {
            fn capture_and_clear() -> Self {
                let saved = std::env::var_os("TACHI_EMBED_RAW_TIER");
                std::env::remove_var("TACHI_EMBED_RAW_TIER");
                Self { saved }
            }
        }

        impl Drop for EmbedRawTierEnvRestore {
            fn drop(&mut self) {
                match &self.saved {
                    Some(v) => std::env::set_var("TACHI_EMBED_RAW_TIER", v),
                    None => std::env::remove_var("TACHI_EMBED_RAW_TIER"),
                }
            }
        }

        let _restore = EmbedRawTierEnvRestore::capture_and_clear();

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

        std::env::set_var("TACHI_EMBED_RAW_TIER", "1");
        let entries = store.entries_missing_vectors(50).expect("scan vectors");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["missing", "raw-entry"],
            "flag on: raw rows lacking vectors are included alongside non-raw"
        );

        let mut raw_high = test_entry("raw-high");
        raw_high.importance = 0.99;
        store.upsert(&raw_high).expect("seed raw-high");
        let mut consolidated = test_entry("consolidated-low");
        consolidated.tier = "consolidated".to_string();
        consolidated.importance = 0.1;
        store.upsert(&consolidated).expect("seed consolidated");

        let entries = store.entries_missing_vectors(50).expect("scan ordered");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["missing", "consolidated-low", "raw-high", "raw-entry"],
            "non-raw rows must sort before raw regardless of importance"
        );

        std::env::set_var("TACHI_EMBED_RAW_TIER", "0");
        let entries = store
            .entries_missing_vectors(50)
            .expect("scan with raw excluded");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["missing", "consolidated-low"],
            "flag off restores pre-#1242 raw exclusion"
        );
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
    fn promotion_candidate_admission_uses_use_provenance_only_when_enabled() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("used-low-display")).unwrap();
        for index in 0..3 {
            let id = format!("display-heavy-{index}");
            let mut entry = test_entry(&id);
            entry.timestamp = format!("2026-07-06T00:00:0{index}Z");
            entry.access_count = 100;
            store.upsert(&entry).unwrap();
        }
        for day in 1..=4 {
            store.connection().execute(
                "INSERT INTO access_history (memory_id, accessed_at, event_kind) VALUES ('used-low-display', ?1, 'use')",
                [format!("2026-07-0{day}T00:00:00Z")],
            ).unwrap();
        }
        store.upsert(&test_entry("same-day-heavy")).unwrap();
        for _ in 0..10 {
            store
                .connection()
                .execute(
                    "INSERT INTO access_history (memory_id, accessed_at, event_kind)
                 VALUES ('same-day-heavy', '2026-07-01T12:00:00Z', 'use')",
                    [],
                )
                .unwrap();
        }

        let off = crate::RecallConfig {
            use_provenance_recency: false,
            ..crate::RecallConfig::default()
        };
        let on = crate::RecallConfig::default();
        let off_ids: Vec<String> = store
            .promotion_candidate_entries_for_config(1, &off)
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        let legacy_off_ids: Vec<String> = store
            .promotion_candidate_entries(1)
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(
            off_ids, legacy_off_ids,
            "OFF must retain the existing promotion_candidate_entries behavior"
        );
        let on_ids: Vec<String> = store
            .promotion_candidate_entries_for_config(1, &on)
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(
            on_ids,
            vec!["used-low-display"],
            "ON must admit the four-distinct-day memory ahead of a row with ten use events on one day"
        );
        assert!(
            !on_ids.contains(&"display-heavy-0".to_string()),
            "the bounded ON admission must exclude a no-use row"
        );
        assert!(
            !on_ids.contains(&"same-day-heavy".to_string()),
            "duplicate same-day use events must not outrank broader calendar-day evidence"
        );
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

    /// tachi#1446 lever 6: the arm the promotion ratchet reads is decided by
    /// `RecallConfig::use_provenance_recency`, and use provenance is the default.
    ///
    /// This is the knob wiring under test in isolation, because the production
    /// call site reads `RecallConfig::get()` — a process-wide `OnceLock` that
    /// cannot be set per-test without cross-test interference.
    #[test]
    fn promotion_arm_follows_use_provenance_recency_and_defaults_to_use() {
        let default_config = crate::RecallConfig::default();
        assert!(
            default_config.use_provenance_recency,
            "display provenance must not feed the irreversible promotion default"
        );
        assert_eq!(
            AccessEventKind::for_promotion(&default_config),
            Some(AccessEventKind::Use),
            "the default promotion ratchet must read caller-initiated use days"
        );

        let legacy = crate::RecallConfig {
            use_provenance_recency: false,
            ..crate::RecallConfig::default()
        };
        assert_eq!(
            AccessEventKind::for_promotion(&legacy),
            None,
            "the explicit rollback arm preserves the historical unfiltered row set"
        );
    }

    /// tachi#1446 lever 6, end-to-end at the store layer: a memory the system
    /// displayed on many distinct days contributes **zero** promotion days once
    /// the knob is on, while the default arm is untouched by the split.
    #[test]
    fn exposure_days_do_not_reach_the_promotion_gate_with_the_use_knob_on() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry("shown-often"))
            .expect("seed entry");
        for accessed_at in [
            "2026-07-01T08:00:00Z",
            "2026-07-02T08:00:00Z",
            "2026-07-03T08:00:00Z",
            "2026-07-04T08:00:00Z",
            "2026-07-05T08:00:00Z",
            "2026-07-06T08:00:00Z",
        ] {
            store
                .connection()
                .execute(
                    "INSERT INTO access_history (memory_id, accessed_at, event_kind)
                     VALUES (?1, ?2, 'display')",
                    ["shown-often", accessed_at],
                )
                .expect("insert display row");
        }

        let off = crate::RecallConfig {
            use_provenance_recency: false,
            ..crate::RecallConfig::default()
        };
        let on = crate::RecallConfig::default();

        assert_eq!(
            store
                .distinct_promotion_days("shown-often", &off)
                .expect("count days"),
            6,
            "explicit legacy config keeps counting the six display days"
        );
        assert_eq!(
            store
                .distinct_promotion_days("shown-often", &on)
                .expect("count days"),
            0,
            "six displays are six displays — with the knob on, none of them is \
             evidence anyone used this memory, so none of them may ratchet it \
             to durable"
        );
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
