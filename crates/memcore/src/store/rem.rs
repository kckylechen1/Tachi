//! REM wiki-evolver persistence helpers on [`MemoryStore`].

use rusqlite::TransactionBehavior;

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

    /// Load unfinished REM draft operations, including archived occupants so
    /// recovery can fail loudly instead of silently minting a replacement.
    pub fn pending_rem_wiki_operations(
        &self,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.pending_rem_wiki_operations_after(None, limit)
    }

    /// Page unfinished REM operations in stable `(timestamp, id)` order.
    /// The cursor prevents one repository's operations in a shared Wiki DB
    /// from starving recovery work owned by another repository.
    pub fn pending_rem_wiki_operations_after(
        &self,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let (after_timestamp, after_id) = after
            .map(|(timestamp, id)| (Some(timestamp), Some(id)))
            .unwrap_or((None, None));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM memories
             WHERE path LIKE '/wiki/drafts/%'
               AND id LIKE 'wiki-rem:%'
               AND json_extract(metadata, '$.rem.producer') = 'weekly_wiki_evolver'
               AND json_extract(metadata, '$.rem.operation_status') = 'pending_sources'
               AND (?1 IS NULL
                    OR timestamp > ?1
                    OR (timestamp = ?1 AND id > ?2))
             ORDER BY timestamp ASC, id ASC
             LIMIT ?3",
            db::MEMORY_SELECT_COLUMNS
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![after_timestamp, after_id, limit as i64],
            db::row_to_entry,
        )?;
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
        &mut self,
        ids: &[String],
        processed_at: &str,
    ) -> Result<(), MemoryError> {
        self.mark_rem_processed_for_draft(ids, processed_at, None)
    }

    /// Atomically mark one store's share of a replay-safe REM source set.
    /// Every named source must still exist and may be claimed only once; a
    /// same-draft replay is idempotent, while a different claimant fails the
    /// whole store-local transaction.
    pub fn mark_rem_processed_for_draft(
        &mut self,
        ids: &[String],
        processed_at: &str,
        draft_id: Option<&str>,
    ) -> Result<(), MemoryError> {
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("mark_rem_processed_for_draft", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for id in ids {
                let state = tx.query_row(
                    "SELECT COALESCE(json_extract(metadata, '$.rem.processed'), 0), \
                            json_extract(metadata, '$.rem.processed_by') \
                     FROM memories WHERE id = ?1",
                    [id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                );
                let (processed, processed_by) = match state {
                    Ok(state) => state,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        return Err(MemoryError::InvalidArg(format!(
                            "REM source disappeared before processing: {id}"
                        )))
                    }
                    Err(error) => return Err(error.into()),
                };
                if processed != 0 {
                    if (draft_id.is_some() && processed_by.as_deref() == draft_id)
                        || (draft_id.is_none() && processed_by.is_none())
                    {
                        continue;
                    }
                    return Err(MemoryError::InvalidArg(format!(
                        "REM source already belongs to another completed operation: {id}"
                    )));
                }
                let changed = tx.execute(
                    r#"UPDATE memories
                       SET metadata = json_set(
                             CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                             '$.rem.processed', 1,
                             '$.rem.processed_at', ?1,
                             '$.rem.processed_by', ?2
                           )
                       WHERE id = ?3
                         AND COALESCE(json_extract(metadata, '$.rem.processed'), 0) = 0"#,
                    rusqlite::params![processed_at, draft_id, id],
                )?;
                if changed != 1 {
                    return Err(MemoryError::InvalidArg(format!(
                        "REM source processing CAS failed: {id}"
                    )));
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Finish a pending REM draft operation after every source-store marker
    /// transaction has committed. Same-operation replay is idempotent.
    pub fn complete_rem_wiki_operation(
        &mut self,
        draft_id: &str,
        completed_at: &str,
    ) -> Result<(), MemoryError> {
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("complete_rem_wiki_operation", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let status = tx.query_row(
                "SELECT archived, superseded_by, json_extract(metadata, '$.rem.operation_status') \
                 FROM memories WHERE id = ?1",
                [draft_id],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            );
            let (archived, superseded_by, status) = match status {
                Ok(status) => status,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(MemoryError::InvalidArg(format!(
                        "REM draft disappeared before completion: {draft_id}"
                    )))
                }
                Err(error) => return Err(error.into()),
            };
            if archived || superseded_by.is_some() {
                return Err(MemoryError::InvalidArg(format!(
                    "REM draft is not an active canonical winner: {draft_id}"
                )));
            }
            if status.as_deref() == Some("complete") {
                tx.commit()?;
                return Ok(());
            }
            if status.as_deref() != Some("pending_sources") {
                return Err(MemoryError::InvalidArg(format!(
                    "REM draft has an invalid operation status: {draft_id}"
                )));
            }
            let changed = tx.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         metadata,
                         '$.rem.operation_status', 'complete',
                         '$.rem.completed_at', ?1
                       )
                   WHERE id = ?2
                     AND archived = 0
                     AND superseded_by IS NULL
                     AND json_extract(metadata, '$.rem.operation_status') = 'pending_sources'"#,
                rusqlite::params![completed_at, draft_id],
            )?;
            if changed != 1 {
                return Err(MemoryError::InvalidArg(format!(
                    "REM draft completion CAS failed: {draft_id}"
                )));
            }
            tx.commit()?;
            Ok(())
        })
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
            scored_count: 0,
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

    #[test]
    fn rem_source_group_rolls_back_and_same_operation_replays() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        for id in ["one", "two"] {
            store
                .upsert(&test_entry(id, "pattern", json!({})))
                .expect("seed source");
        }
        let failing_ids = ["one".to_string(), "missing".to_string()];
        let error = store
            .mark_rem_processed_for_draft(
                &failing_ids,
                "2026-07-05T01:00:00Z",
                Some("wiki-rem:operation"),
            )
            .expect_err("second marker failure must abort the source group");
        assert!(error.to_string().contains("REM source disappeared"));
        for id in ["one", "two"] {
            let source = store.get(id).expect("read source").expect("source exists");
            assert!(source.metadata["rem"]["processed"].is_null());
        }

        let ids = ["one".to_string(), "two".to_string()];
        for _ in 0..2 {
            store
                .mark_rem_processed_for_draft(
                    &ids,
                    "2026-07-05T01:00:00Z",
                    Some("wiki-rem:operation"),
                )
                .expect("same operation is replay-safe");
        }
        for id in ["one", "two"] {
            let source = store.get(id).expect("read source").expect("source exists");
            assert_eq!(source.metadata["rem"]["processed"], 1);
            assert_eq!(source.metadata["rem"]["processed_by"], "wiki-rem:operation");
        }
    }

    #[test]
    fn pending_rem_operation_is_discoverable_and_completion_is_idempotent() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut draft = test_entry(
            "wiki-rem:pending",
            "raw",
            json!({
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "operation_status": "pending_sources"
                }
            }),
        );
        draft.path = "/wiki/drafts/pending".to_string();
        store.upsert(&draft).expect("seed pending draft");
        assert_eq!(
            store
                .pending_rem_wiki_operations(10)
                .expect("list pending")
                .into_iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec!["wiki-rem:pending"]
        );

        for _ in 0..2 {
            store
                .complete_rem_wiki_operation("wiki-rem:pending", "2026-07-05T02:00:00Z")
                .expect("completion replay");
        }
        assert!(store
            .pending_rem_wiki_operations(10)
            .expect("list after completion")
            .is_empty());
        let draft = store
            .get("wiki-rem:pending")
            .expect("read draft")
            .expect("draft exists");
        assert_eq!(draft.metadata["rem"]["operation_status"], "complete");
    }

    #[test]
    fn pending_rem_scan_ignores_ordinary_drafts_with_rem_shaped_metadata() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut ordinary = test_entry(
            "ordinary-draft",
            "raw",
            json!({
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "operation_status": "pending_sources"
                }
            }),
        );
        ordinary.path = "/wiki/drafts/ordinary".to_string();
        let mut canonical = test_entry(
            "wiki-rem:canonical",
            "raw",
            json!({
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "operation_status": "pending_sources"
                }
            }),
        );
        canonical.path = "/wiki/drafts/canonical".to_string();
        store.upsert(&ordinary).expect("seed ordinary draft");
        store.upsert(&canonical).expect("seed canonical REM draft");

        assert_eq!(
            store
                .pending_rem_wiki_operations(10)
                .expect("list canonical pending REM operations")
                .into_iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec!["wiki-rem:canonical"]
        );
    }

    #[test]
    fn pending_rem_winner_refuses_generic_archive_and_supersession_until_complete() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut draft = test_entry(
            "wiki-rem:protected",
            "raw",
            json!({
                "rem": {
                    "producer": "weekly_wiki_evolver",
                    "operation_status": "pending_sources"
                }
            }),
        );
        draft.path = "/wiki/drafts/protected".to_string();
        store.upsert(&draft).expect("seed protected REM draft");

        let archive = store
            .archive_memory(&draft.id)
            .expect_err("pending REM winner must reject generic archive");
        assert!(archive.to_string().contains("pending REM operation winner"));
        let supersede = store
            .supersede_memory(&draft.id, "replacement")
            .expect_err("pending REM winner must reject generic supersession");
        assert!(supersede
            .to_string()
            .contains("pending REM operation winner"));
        let delete = store
            .delete(&draft.id)
            .expect_err("pending REM winner must reject generic deletion");
        assert!(delete.to_string().contains("pending REM operation winner"));

        store
            .complete_rem_wiki_operation(&draft.id, "2026-07-05T02:00:00Z")
            .expect("complete REM operation");
        assert!(store
            .archive_memory(&draft.id)
            .expect("completed REM draft may be archived"));
    }
}
