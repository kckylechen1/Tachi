//! REM wiki-evolver persistence helpers on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryEntry, MemoryStore};

impl MemoryStore {
    /// Load recent `tier=pattern` memories not yet processed by the weekly REM
    /// pass, excluding training seeds, best candidates first.
    ///
    /// tachi#1459: the `access_count DESC` half of "best candidates first"
    /// observes the search path only; reads through path-listing routes do not
    /// increment it. Within a `LIMIT 200` this only reorders — it never
    /// excludes on that basis — but the tiebreak favours what search has shown,
    /// not what has been read.
    pub fn unprocessed_pattern_memories(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM memories
             WHERE archived = 0
               AND tier = 'pattern'
               AND path != '/sft'
               AND path NOT LIKE '/sft/%'
               AND topic != 'sft-memory'
               AND source != 'sft_seed'
               AND COALESCE(json_extract(metadata, '$.training_sample'), 0) = 0
               AND created_at > datetime('now', '-7 day')
               AND (json_extract(metadata, '$.rem.processed') IS NULL
                    OR json_extract(metadata, '$.rem.processed') = 0)
             ORDER BY importance DESC, access_count DESC
             LIMIT 200",
            db::MEMORY_SELECT_COLUMNS
        ))?;
        let rows = stmt.query_map([], db::row_to_entry)?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Stamp `review_status=pending` on a freshly saved wiki draft, only when
    /// no review status has been recorded yet.
    pub fn mark_wiki_draft_review_pending(
        &self,
        path: &str,
        generated_at: &str,
    ) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        self.conn.execute(
            r#"UPDATE memories
               SET metadata = json_set(
                     CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                     '$.review_status', 'pending',
                     '$.rem.generated_at', ?1
                   )
               WHERE path = ?2
                 AND (json_extract(metadata, '$.review_status') IS NULL)"#,
            rusqlite::params![generated_at, path],
        )?;
        Ok(())
    }

    /// Mark source memories as REM-processed so the next weekly pass skips
    /// them.
    pub fn mark_rem_processed(
        &self,
        ids: &[String],
        processed_at: &str,
    ) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        for id in ids {
            self.conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.rem.processed', 1,
                         '$.rem.processed_at', ?1
                       )
                   WHERE id = ?2"#,
                rusqlite::params![processed_at, id],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::types::MemoryEntry;
    use crate::MemoryStore;

    fn test_entry(id: &str, tier: &str, metadata: serde_json::Value) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: format!("{id} summary"),
            text: format!("{id} text"),
            importance: 0.6,
            timestamp: "2026-07-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "experience".to_string(),
            topic: "queue".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: tier.to_string(),
        }
    }

    #[test]
    fn unprocessed_pattern_memories_filters_processed_and_non_pattern_rows() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry("fresh", "pattern", json!({})))
            .expect("seed fresh");
        store
            .upsert(&test_entry(
                "processed",
                "pattern",
                json!({ "rem": { "processed": 1 } }),
            ))
            .expect("seed processed");
        store
            .upsert(&test_entry("raw-entry", "raw", json!({})))
            .expect("seed raw");
        let mut sft = test_entry("sft-seed", "pattern", json!({}));
        sft.path = "/sft/v4/1".to_string();
        store.upsert(&sft).expect("seed sft");

        let entries = store
            .unprocessed_pattern_memories()
            .expect("collect patterns");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["fresh"]);
    }

    #[test]
    fn mark_wiki_draft_review_pending_sets_status_only_once() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut draft = test_entry("draft", "raw", json!({}));
        draft.path = "/wiki/drafts/queue-pattern".to_string();
        store.upsert(&draft).expect("seed draft");

        store
            .mark_wiki_draft_review_pending("/wiki/drafts/queue-pattern", "2026-07-05T01:00:00Z")
            .expect("first stamp");
        store
            .mark_wiki_draft_review_pending("/wiki/drafts/queue-pattern", "2026-07-06T01:00:00Z")
            .expect("second stamp");

        let entry = store.get("draft").expect("read draft").expect("exists");
        assert_eq!(entry.metadata["review_status"], "pending");
        assert_eq!(
            entry.metadata["rem"]["generated_at"],
            "2026-07-05T01:00:00Z"
        );
    }

    #[test]
    fn mark_rem_processed_stamps_all_given_ids() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry("one", "pattern", json!({})))
            .expect("seed one");
        store
            .upsert(&test_entry("two", "pattern", json!({ "keep": true })))
            .expect("seed two");

        store
            .mark_rem_processed(
                &["one".to_string(), "two".to_string()],
                "2026-07-05T01:00:00Z",
            )
            .expect("mark processed");

        for id in ["one", "two"] {
            let entry = store.get(id).expect("read entry").expect("exists");
            assert_eq!(entry.metadata["rem"]["processed"], 1);
            assert_eq!(
                entry.metadata["rem"]["processed_at"],
                "2026-07-05T01:00:00Z"
            );
        }
        let two = store.get("two").expect("read two").expect("exists");
        assert_eq!(two.metadata["keep"], true);
    }
}
