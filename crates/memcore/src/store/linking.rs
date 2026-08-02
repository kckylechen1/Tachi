//! Auto-link, contradiction, and confidence-reinforcement helpers on
//! [`MemoryStore`].

use std::collections::HashSet;

use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::db::ConfirmedContradictionOutcome;
use crate::types::ExpectedMemoryState;
use crate::{db, error::MemoryError, MemoryEntry, MemoryStore};

impl MemoryStore {
    /// Atomically persist the two graph projections and lifecycle transition
    /// produced by one successful contradiction-verification invocation.
    ///
    /// The DB seam validates the fixed relations, matching endpoints, matching
    /// receipt-bearing metadata, and the unsuperseded lifecycle CAS. Any
    /// failure drops this `BEGIN IMMEDIATE`, including edge observations that
    /// were appended before a later mutation failed.
    ///
    /// `expected_entry` and `expected_candidate` are the entry's and
    /// candidate's complete state as read on the way into verification. The
    /// model round-trip happens outside any transaction, so these snapshots —
    /// not `superseded_by IS NULL`, and not `revision`, which enrichment does
    /// not bump — are what keep the verdict bound to the content it was
    /// actually about, on both sides of the judged pair (tachi#1563 extends
    /// the candidate-only binding from tachi#1551 to the entry too, since the
    /// entry can equally be rewritten or archived while the model call is in
    /// flight). A snapshot mismatch on either side returns
    /// [`ConfirmedContradictionOutcome::StaleSkipped`] with nothing written.
    pub fn commit_confirmed_contradiction(
        &mut self,
        contradicts_edge: &crate::MemoryEdge,
        supersedes_edge: &crate::MemoryEdge,
        superseded_at: &str,
        expected_entry: &ExpectedMemoryState,
        expected_candidate: &ExpectedMemoryState,
    ) -> Result<ConfirmedContradictionOutcome, MemoryError> {
        let db_label = self.db_label.clone();
        let reserved_reference_write = self.reserved_reference_write.clone();
        db::retry_memory_locked("confirmed_contradiction", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&reserved_reference_write)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let outcome = db::persist_confirmed_contradiction_within_tx(
                &tx,
                contradicts_edge,
                supersedes_edge,
                superseded_at,
                expected_entry,
                expected_candidate,
            )?;
            tx.commit()?;
            Ok(outcome)
        })
    }

    /// Inspect the lifecycle supersession edge for any row, including rows
    /// excluded from active recall because they are archived.
    pub fn supersession_target(&self, id: &str) -> Result<Option<Option<String>>, MemoryError> {
        Ok(self
            .conn
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Mark `id` as superseded by `superseded_by` and close its validity
    /// window, but only when the row has not been superseded already.
    ///
    /// Closing `valid_until` at supersession time keeps `as_of` point-in-time
    /// recall from returning the superseded fact forever (same invariant as
    /// [`MemoryStore::supersede_memory`]); COALESCE keeps any explicit window
    /// intact. Unlike `supersede_memory`, this uses the caller-supplied
    /// timestamp and does not bump `revision`, matching the auto-link write
    /// path that calls it directly. The LLM-confirmed contradiction path
    /// (`db::persist_confirmed_contradiction_within_tx`) does **not** call
    /// this function — it runs its own lifecycle `UPDATE` that does advance
    /// `revision`, so `update_enrichment_fields`'s CAS notices the
    /// supersession (tachi#1563).
    ///
    /// Returns the number of rows actually updated (0 or 1 — `id` is the
    /// primary key) so callers can tell "this call is what closed the row"
    /// from "the row was already superseded, this was a no-op" — tachi#1435
    /// slice 5 / #2059 codex round 3: the auto-link write path only busts the
    /// recall cache when this returns `> 0`, since search-visible content
    /// only changed on the former.
    pub fn mark_superseded_closing_validity(
        &self,
        id: &str,
        superseded_by: &str,
        at: &str,
    ) -> Result<usize, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let affected = self.conn.execute(
            "UPDATE memories SET superseded_by = ?1, updated_at = ?2, valid_until = COALESCE(valid_until, ?2) WHERE id = ?3 AND superseded_by IS NULL",
            rusqlite::params![superseded_by, at, id],
        )?;
        Ok(affected)
    }

    /// Bump `$.confidence` in metadata by `increment` (capped at 1.0), falling
    /// back to `importance` when no confidence has been recorded yet.
    pub fn reinforce_confidence(
        &self,
        id: &str,
        increment: f64,
        reinforced_at: &str,
    ) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        self.conn.execute(
            r#"UPDATE memories
               SET metadata = json_set(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.confidence',
                   min(
                       1.0,
                       coalesce(
                           CAST(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.confidence') AS REAL),
                           importance
                       ) + ?1
                   ),
                   '$.confidence_reinforced_at', ?2
               ),
               updated_at = ?2
               WHERE id = ?3"#,
            rusqlite::params![increment, reinforced_at, id],
        )?;
        Ok(())
    }

    /// Load active memories that share at least one entity with `entities`,
    /// newest first per entity batch, excluding `exclude_id`.
    pub fn entity_overlap_candidates(
        &self,
        exclude_id: &str,
        entities: &[String],
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut entities = entities
            .iter()
            .map(|entity| entity.trim())
            .filter(|entity| !entity.is_empty())
            .collect::<Vec<_>>();
        entities.sort_unstable();
        entities.dedup();
        if entities.is_empty() {
            return Ok(Vec::new());
        }

        let mut candidate_ids = Vec::new();
        let mut seen = HashSet::<String>::new();
        for batch in entities.chunks(200) {
            let placeholders = (2..batch.len() + 2)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                r#"SELECT id
                   FROM memories
                   WHERE archived = 0
                     AND id != ?1
                     AND EXISTS (
                         SELECT 1 FROM json_each(memories.entities)
                         WHERE json_each.value IN ({placeholders})
                     )
                   ORDER BY timestamp DESC
                   LIMIT {}"#,
                (batch.len() * 5).clamp(5, 50)
            );
            let params = std::iter::once(exclude_id).chain(batch.iter().copied());
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                let candidate_id = row?;
                if seen.insert(candidate_id.clone()) {
                    candidate_ids.push(candidate_id);
                }
            }
        }

        Ok(db::fetch_by_ids(&self.conn, &candidate_ids, false)?
            .into_values()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::types::MemoryEntry;
    use crate::MemoryStore;

    fn test_entry(id: &str, entities: Vec<String>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: format!("{id} summary"),
            text: format!("{id} text"),
            importance: 0.6,
            timestamp: "2026-07-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities,
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

    fn superseded_state(store: &MemoryStore, id: &str) -> (Option<String>, Option<String>) {
        store
            .connection()
            .query_row(
                "SELECT superseded_by, valid_until FROM memories WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read superseded state")
    }

    #[test]
    fn mark_superseded_closing_validity_updates_only_unsuperseded_rows() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store.upsert(&test_entry("old", vec![])).expect("seed old");
        store.upsert(&test_entry("new", vec![])).expect("seed new");

        let first_affected = store
            .mark_superseded_closing_validity("old", "new", "2026-07-05T01:00:00Z")
            .expect("first supersession");
        assert_eq!(
            first_affected, 1,
            "the first supersession must report 1 row actually updated"
        );
        let (superseded_by, valid_until) = superseded_state(&store, "old");
        assert_eq!(superseded_by.as_deref(), Some("new"));
        assert_eq!(valid_until.as_deref(), Some("2026-07-05T01:00:00Z"));

        let second_affected = store
            .mark_superseded_closing_validity("old", "other", "2026-07-06T00:00:00Z")
            .expect("second supersession");
        assert_eq!(
            second_affected, 0,
            "an already-superseded row must report 0 rows affected (WHERE superseded_by IS NULL excludes it) — \
             this is exactly the signal callers use to skip a no-op recall-cache invalidation"
        );
        let (superseded_by, valid_until) = superseded_state(&store, "old");
        assert_eq!(superseded_by.as_deref(), Some("new"));
        assert_eq!(valid_until.as_deref(), Some("2026-07-05T01:00:00Z"));
    }

    #[test]
    fn reinforce_confidence_increments_and_falls_back_to_importance() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut with_confidence = test_entry("with-confidence", vec![]);
        with_confidence.metadata = json!({ "confidence": 0.70 });
        store.upsert(&with_confidence).expect("seed confidence");
        let mut without_confidence = test_entry("without-confidence", vec![]);
        without_confidence.importance = 0.60;
        store.upsert(&without_confidence).expect("seed importance");

        store
            .reinforce_confidence("with-confidence", 0.08, "2026-07-05T01:00:00Z")
            .expect("reinforce existing confidence");
        store
            .reinforce_confidence("without-confidence", 0.10, "2026-07-05T01:00:00Z")
            .expect("reinforce importance fallback");

        let reinforced = store
            .get("with-confidence")
            .expect("read entry")
            .expect("entry exists");
        let confidence = reinforced.metadata["confidence"].as_f64().unwrap();
        assert!((confidence - 0.78).abs() < 1e-9, "confidence={confidence}");
        assert_eq!(
            reinforced.metadata["confidence_reinforced_at"],
            "2026-07-05T01:00:00Z"
        );

        let fallback = store
            .get("without-confidence")
            .expect("read entry")
            .expect("entry exists");
        let confidence = fallback.metadata["confidence"].as_f64().unwrap();
        assert!((confidence - 0.70).abs() < 1e-9, "confidence={confidence}");
    }

    #[test]
    fn entity_overlap_candidates_filters_self_archived_and_disjoint() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry("self", vec!["Acme".to_string()]))
            .expect("seed self");
        store
            .upsert(&test_entry("match", vec!["Acme".to_string()]))
            .expect("seed match");
        store
            .upsert(&test_entry("disjoint", vec!["Beta".to_string()]))
            .expect("seed disjoint");
        let mut archived = test_entry("archived", vec!["Acme".to_string()]);
        archived.archived = true;
        store.upsert(&archived).expect("seed archived");

        let candidates = store
            .entity_overlap_candidates("self", &[" Acme ".to_string(), String::new()])
            .expect("collect candidates");
        let ids: Vec<&str> = candidates.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["match"]);

        assert!(store
            .entity_overlap_candidates("self", &[])
            .expect("empty entities")
            .is_empty());
    }
}
