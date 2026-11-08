//! Auto-link, contradiction, and confidence-reinforcement helpers on
//! [`MemoryStore`].

use std::collections::HashSet;

#[cfg(any(test, feature = "test-support"))]
use rusqlite::Transaction;
use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::db::ConfirmedContradictionOutcome;
use crate::types::ExpectedMemoryState;
use crate::{db, error::MemoryError, MemoryEntry, MemoryStore, SupersessionExpectedState};

impl MemoryStore {
    /// Atomically persist one heuristic reinforcement, but only while both
    /// endpoint rows still match the snapshots used to compute it.
    ///
    /// Search and similarity scoring happen before this call. A concurrent
    /// rewrite of either endpoint therefore makes the judgment stale. The
    /// `BEGIN IMMEDIATE` snapshot check, edge write, and confidence update
    /// share one transaction so a stale judgment or later failure leaves no
    /// partial edge or confidence mutation. Both endpoints must still be
    /// active and unsuperseded, and this fixed heuristic door always stamps
    /// [`db::EdgeAuthority::DerivedHeuristic`] rather than trusting caller
    /// provenance.
    pub fn commit_confidence_reinforcement(
        &mut self,
        edge: &crate::MemoryEdge,
        increment: f64,
        reinforced_at: &str,
        expected_source: &ExpectedMemoryState,
        expected_target: &ExpectedMemoryState,
    ) -> Result<bool, MemoryError> {
        if edge.relation != "reinforces" || edge.source_id == edge.target_id {
            return Err(MemoryError::InvalidArg(
                "confidence reinforcement requires distinct endpoints and a reinforces edge"
                    .to_string(),
            ));
        }
        if !increment.is_finite() || !(0.0..=1.0).contains(&increment) {
            return Err(MemoryError::InvalidArg(
                "confidence reinforcement increment must be finite and within [0, 1]".to_string(),
            ));
        }

        let db_label = self.db_label.clone();
        let reserved_reference_write = self.reserved_reference_write.clone();
        db::retry_memory_locked("confidence_reinforcement", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&reserved_reference_write)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !db::row_matches_expected_state(&tx, &edge.source_id, expected_source)?
                || !db::row_matches_expected_state(&tx, &edge.target_id, expected_target)?
            {
                return Ok(false);
            }
            for id in [&edge.source_id, &edge.target_id] {
                let active_unsuperseded = tx
                    .query_row(
                        "SELECT archived = 0 AND superseded_by IS NULL FROM memories WHERE id = ?1",
                        [id],
                        |row| row.get::<_, bool>(0),
                    )
                    .optional()?
                    .unwrap_or(false);
                if !active_unsuperseded {
                    return Ok(false);
                }
            }
            db::refuse_retired_sticky_row_within_tx(
                &tx,
                &edge.source_id,
                "used as a confidence reinforcement source",
            )?;
            db::refuse_retired_sticky_row_within_tx(&tx, &edge.target_id, "confidence-reinforced")?;
            db::add_edge_with_provenance(
                &tx,
                edge,
                &db::EdgeProvenance {
                    authority: Some(db::EdgeAuthority::DerivedHeuristic),
                    ..Default::default()
                },
            )?;
            let affected = tx.execute(
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
                   WHERE id = ?3 AND archived = 0 AND superseded_by IS NULL"#,
                rusqlite::params![increment, reinforced_at, &edge.target_id],
            )?;
            if affected != 1 {
                return Err(MemoryError::NotFound(edge.target_id.clone()));
            }
            tx.commit()?;
            Ok(true)
        })
    }

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
        db::validate_confirmed_contradiction(contradicts_edge, supersedes_edge, superseded_at)?;
        let superseded_at = db::normalize_utc_iso(superseded_at)?;
        self.with_immutable_supersession_transaction(|replacement| {
            if !db::row_matches_expected_state(
                replacement.transaction(),
                &contradicts_edge.source_id,
                expected_entry,
            )? {
                eprintln!(
                    "[confirmed-contradiction] entry-side snapshot mismatch for {} (candidate {}): the triggering memory changed (or was archived/deleted) between the read that fed the model and the write",
                    contradicts_edge.source_id, contradicts_edge.target_id
                );
                return Ok(ConfirmedContradictionOutcome::StaleSkipped);
            }
            if !db::row_matches_expected_state(
                replacement.transaction(),
                &contradicts_edge.target_id,
                expected_candidate,
            )? {
                return Ok(ConfirmedContradictionOutcome::StaleSkipped);
            }

            let expected = SupersessionExpectedState {
                source: expected_candidate.clone(),
                target: Some(expected_entry.clone()),
            };
            let outcome = match replacement.claim_checked_immutable_supersession_at(
                &contradicts_edge.target_id,
                &contradicts_edge.source_id,
                &expected,
                "confirmed_contradiction_v1",
                "confirmed-contradiction-v1",
                false,
                &superseded_at,
            ) {
                Ok(outcome) => outcome,
                Err(error) if error.to_string().contains("(competing_target)") => {
                    return Err(MemoryError::InvalidArg(format!(
                        "confirmed contradiction lifecycle CAS refused for {} -> {}",
                        contradicts_edge.target_id, contradicts_edge.source_id
                    )));
                }
                Err(error) => return Err(error),
            };
            if outcome.result != crate::SupersessionCommitResult::Applied {
                return Err(MemoryError::InvalidArg(
                    "confirmed contradiction supersession was not applied".to_string(),
                ));
            }

            let receipt_provenance = db::EdgeProvenance {
                authority: Some(db::EdgeAuthority::ModelReceiptBacked),
                ..db::EdgeProvenance::default()
            };
            replacement.add_edge_with_provenance(contradicts_edge, &receipt_provenance)?;
            replacement.add_canonical_supersession_edge(supersedes_edge, &receipt_provenance)?;
            Ok(ConfirmedContradictionOutcome::Committed)
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
    /// (`Self::commit_confirmed_contradiction`) uses the canonical immutable
    /// supersession transaction, which advances `revision` so enrichment CAS
    /// notices the transition.
    ///
    /// Returns the number of rows actually updated (0 or 1 — `id` is the
    /// primary key) so callers can tell "this call is what closed the row"
    /// from "the row was already superseded, this was a no-op" — tachi#1435
    /// slice 5 / #2059 codex round 3: the auto-link write path only busts the
    /// recall cache when this returns `> 0`, since search-visible content
    /// only changed on the former.
    #[cfg(any(test, feature = "test-support"))]
    pub fn mark_superseded_closing_validity(
        &self,
        id: &str,
        superseded_by: &str,
        at: &str,
    ) -> Result<usize, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        db::refuse_retired_sticky_row_within_tx(&tx, id, "superseded by auto-link")?;
        db::refuse_retired_sticky_row_within_tx(
            &tx,
            superseded_by,
            "used as an auto-link supersession target",
        )?;
        let affected = tx.execute(
            "UPDATE memories SET superseded_by = ?1, updated_at = ?2, valid_until = COALESCE(valid_until, ?2) WHERE id = ?3 AND superseded_by IS NULL",
            rusqlite::params![superseded_by, at, id],
        )?;
        tx.commit()?;
        Ok(affected)
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
    use crate::{db::ConfirmedContradictionOutcome, MemoryEdge};
    use rusqlite::Connection;
    use serde_json::json;

    use crate::types::{ExpectedMemoryState, MemoryEntry};
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
    fn confidence_reinforcement_refuses_a_rewritten_source_without_partial_writes() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let source = test_entry("new", vec!["Acme".to_string()]);
        let mut target = test_entry("old", vec!["Acme".to_string()]);
        target.metadata = json!({ "confidence": 0.50 });
        store.upsert(&source).expect("seed source");
        store.upsert(&target).expect("seed target");

        let expected_source = ExpectedMemoryState::from_entry(&source, None);
        let expected_target = ExpectedMemoryState::from_entry(&target, None);
        let edge = crate::MemoryEdge {
            source_id: source.id.clone(),
            target_id: target.id.clone(),
            relation: "reinforces".to_string(),
            weight: 0.8,
            metadata: json!({ "auto_link": true }),
            created_at: "2026-07-05T01:00:00Z".to_string(),
            valid_from: String::new(),
            valid_to: None,
        };

        let mut rewritten_source = source.clone();
        rewritten_source.text = "concurrently rewritten source".to_string();
        store
            .upsert(&rewritten_source)
            .expect("rewrite source after scoring snapshot");

        let committed = store
            .commit_confidence_reinforcement(
                &edge,
                0.08,
                "2026-07-05T01:00:00Z",
                &expected_source,
                &expected_target,
            )
            .expect("stale reinforcement is an ordinary skipped outcome");

        assert!(!committed);
        assert!(store
            .get_edges("new", "outgoing", Some("reinforces"))
            .expect("read edges")
            .is_empty());
        let unchanged_target = store.get("old").expect("read target").expect("target");
        assert_eq!(unchanged_target.metadata["confidence"], 0.50);
        assert!(unchanged_target
            .metadata
            .get("confidence_reinforced_at")
            .is_none());
    }

    #[test]
    fn confidence_reinforcement_refuses_archived_target_without_partial_writes() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let source = test_entry("new-archived-case", vec!["Acme".to_string()]);
        let mut target = test_entry("old-archived-case", vec!["Acme".to_string()]);
        target.archived = true;
        target.metadata = json!({ "confidence": 0.50 });
        store.upsert(&source).expect("seed source");
        store.upsert(&target).expect("seed archived target");
        let source = store
            .get(&source.id)
            .expect("read source")
            .expect("source exists");
        let target = store
            .get_with_options(&target.id, true)
            .expect("read archived target")
            .expect("archived target exists");
        let edge = crate::MemoryEdge {
            source_id: source.id.clone(),
            target_id: target.id.clone(),
            relation: "reinforces".to_string(),
            weight: 0.8,
            metadata: json!({ "auto_link": true }),
            created_at: "2026-07-05T01:00:00Z".to_string(),
            valid_from: String::new(),
            valid_to: None,
        };

        let committed = store
            .commit_confidence_reinforcement(
                &edge,
                0.08,
                "2026-07-05T01:00:00Z",
                &ExpectedMemoryState::from_entry(&source, None),
                &ExpectedMemoryState::from_entry(&target, None),
            )
            .expect("archived reinforcement is an ordinary skipped outcome");

        assert!(!committed);
        assert!(store
            .get_edges(&source.id, "outgoing", Some("reinforces"))
            .expect("read edges")
            .is_empty());
        let unchanged_target = store
            .get_with_options(&target.id, true)
            .expect("read archived target")
            .expect("archived target exists");
        assert_eq!(unchanged_target.metadata["confidence"], 0.50);
        assert!(unchanged_target
            .metadata
            .get("confidence_reinforced_at")
            .is_none());
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
        let source_with_confidence = test_entry("source-with-confidence", vec![]);
        let source_without_confidence = test_entry("source-without-confidence", vec![]);
        let mut with_confidence = test_entry("with-confidence", vec![]);
        with_confidence.metadata = json!({ "confidence": 0.70 });
        store
            .upsert(&source_with_confidence)
            .expect("seed first source");
        store.upsert(&with_confidence).expect("seed confidence");
        let mut without_confidence = test_entry("without-confidence", vec![]);
        without_confidence.importance = 0.60;
        store
            .upsert(&source_without_confidence)
            .expect("seed second source");
        store.upsert(&without_confidence).expect("seed importance");

        for (source_id, target_id, increment) in [
            ("source-with-confidence", "with-confidence", 0.08),
            ("source-without-confidence", "without-confidence", 0.10),
        ] {
            let source = store
                .get(source_id)
                .expect("read source")
                .expect("source exists");
            let target = store
                .get(target_id)
                .expect("read target")
                .expect("target exists");
            let edge = crate::MemoryEdge {
                source_id: source.id.clone(),
                target_id: target.id.clone(),
                relation: "reinforces".to_string(),
                weight: 0.8,
                metadata: json!({ "auto_link": true }),
                created_at: "2026-07-05T01:00:00Z".to_string(),
                valid_from: String::new(),
                valid_to: None,
            };
            assert!(store
                .commit_confidence_reinforcement(
                    &edge,
                    increment,
                    "2026-07-05T01:00:00Z",
                    &ExpectedMemoryState::from_entry(&source, None),
                    &ExpectedMemoryState::from_entry(&target, None),
                )
                .expect("commit typed confidence reinforcement"));
        }

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

    fn confirmed_contradiction_edges(
        source_id: &str,
        target_id: &str,
        completion_status: &str,
    ) -> (MemoryEdge, MemoryEdge, String) {
        let at = "2026-07-30T01:02:03Z".to_string();
        let metadata = json!({
            "auto_contradiction": true,
            "llm_verified": true,
            "confidence": 0.88,
            "reason": "newer fact conflicts",
            "provenance": {
                "model_invocation": {
                    "schema": "model-invocation-v1",
                    "lane": "extract",
                    "engine_kind": "provider_http",
                    "effective_provider": "test-provider",
                    "effective_model": "test-model",
                    "effective_version": null,
                    "fallback_chain": [],
                    "degraded": false,
                    "completion_status": completion_status,
                    "prompt_tokens": 12,
                    "completion_tokens": 4,
                    "total_tokens": 16,
                    "latency_ms": 9
                }
            }
        });
        let edge = |relation: &str| MemoryEdge {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            relation: relation.to_string(),
            weight: 0.88,
            metadata: metadata.clone(),
            created_at: at.clone(),
            valid_from: String::new(),
            valid_to: None,
        };
        (edge("contradicts"), edge("supersedes"), at)
    }

    /// Snapshot a row (entry or candidate) the way the contradiction read path
    /// does, through the same loader the write transaction re-reads with.
    fn expected_state(store: &MemoryStore, id: &str) -> ExpectedMemoryState {
        let entry = store
            .get_with_options(id, true)
            .unwrap()
            .expect("row exists");
        let target = store.supersession_target(id).unwrap().flatten();
        ExpectedMemoryState::from_entry(&entry, target.as_deref())
    }

    fn assert_no_contradiction_mutation(conn: &Connection, target_id: &str) {
        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE relation IN ('contradicts', 'supersedes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let observation_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let state: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT superseded_by, valid_until FROM memories WHERE id = ?1",
                [target_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let receipts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace = 'memory_supersession_receipts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edge_count, 0, "failed mutation must leave no graph rows");
        assert_eq!(
            observation_count, 0,
            "failed mutation must leave no edge observations"
        );
        assert_eq!(state, (None, None), "failed mutation must not supersede");
        assert_eq!(
            receipts, 0,
            "failed mutation must leave no canonical receipt"
        );
    }

    #[test]
    fn confirmed_contradiction_transaction_commits_edges_observations_and_lifecycle() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("confirmed-new", vec![])).unwrap();
        store.upsert(&test_entry("confirmed-old", vec![])).unwrap();
        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("confirmed-new", "confirmed-old", "complete");
        let expected_entry = expected_state(&store, "confirmed-new");
        let expected = expected_state(&store, "confirmed-old");
        let revision_before: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'confirmed-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();
        assert_eq!(outcome, ConfirmedContradictionOutcome::Committed);

        let edge_count: i64 = store.connection()
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE source_id = 'confirmed-new' AND target_id = 'confirmed-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let observation_count: i64 = store.connection()
            .query_row(
                "SELECT COUNT(*) FROM edge_observations WHERE source_id = 'confirmed-new' AND target_id = 'confirmed-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let state: (Option<String>, Option<String>, i64) = store.connection()
            .query_row(
                "SELECT superseded_by, valid_until, revision FROM memories WHERE id = 'confirmed-old'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(edge_count, 2);
        assert_eq!(observation_count, 2);
        assert_eq!(state.0.as_deref(), Some("confirmed-new"));
        assert_eq!(state.1.as_deref(), Some("2026-07-30T01:02:03.000Z"));
        assert_eq!(
            state.2,
            revision_before + 1,
            "supersession must advance revision so revision-scoped CAS callers (e.g. \
             update_enrichment_fields) notice the lifecycle transition"
        );
    }

    #[test]
    fn confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary() {
        let faults = [
            (
                "first_edge",
                "CREATE TRIGGER fail_confirmed_first BEFORE INSERT ON memory_edges \
                 WHEN NEW.relation = 'contradicts' BEGIN SELECT RAISE(ABORT, 'first edge'); END;",
            ),
            (
                "second_edge",
                "CREATE TRIGGER fail_confirmed_second BEFORE INSERT ON memory_edges \
                 WHEN NEW.relation = 'supersedes' BEGIN SELECT RAISE(ABORT, 'second edge'); END;",
            ),
            (
                "lifecycle",
                "CREATE TRIGGER fail_confirmed_lifecycle BEFORE UPDATE OF superseded_by ON memories \
                 BEGIN SELECT RAISE(ABORT, 'lifecycle'); END;",
            ),
        ];

        for (fault_name, trigger) in faults {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("contradiction.sqlite");
            let mut store = MemoryStore::open(path.to_str().unwrap()).unwrap();
            store.upsert(&test_entry("rollback-new", vec![])).unwrap();
            store.upsert(&test_entry("rollback-old", vec![])).unwrap();
            // The store's authorizer remains active; only the independent
            // fixture connection installs fault injection.
            let fault_connection = Connection::open(&path).unwrap();
            fault_connection.execute_batch(trigger).unwrap();
            let (contradicts, supersedes, at) =
                confirmed_contradiction_edges("rollback-new", "rollback-old", "complete");
            let expected_entry = expected_state(&store, "rollback-new");
            let expected = expected_state(&store, "rollback-old");

            let error = store
                .commit_confirmed_contradiction(
                    &contradicts,
                    &supersedes,
                    &at,
                    &expected_entry,
                    &expected,
                )
                .expect_err("injected side-effect failure must abort the mutation");
            assert!(
                error
                    .to_string()
                    .contains(fault_name.split('_').next().unwrap()),
                "unexpected {fault_name} error: {error}"
            );
            assert_no_contradiction_mutation(store.connection(), "rollback-old");
        }
    }

    #[test]
    fn confirmed_contradiction_transaction_rejects_truncated_receipt_before_writes() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("truncated-new", vec![])).unwrap();
        store.upsert(&test_entry("truncated-old", vec![])).unwrap();
        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("truncated-new", "truncated-old", "truncated");
        let expected_entry = expected_state(&store, "truncated-new");
        let expected = expected_state(&store, "truncated-old");

        store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .expect_err("truncated verification receipt must be rejected");
        assert_no_contradiction_mutation(store.connection(), "truncated-old");
    }

    #[test]
    fn confirmed_contradiction_transaction_rejects_non_allowlisted_receipt_fields() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("unsafe-new", vec![])).unwrap();
        store.upsert(&test_entry("unsafe-old", vec![])).unwrap();
        let (mut contradicts, mut supersedes, at) =
            confirmed_contradiction_edges("unsafe-new", "unsafe-old", "complete");
        for edge in [&mut contradicts, &mut supersedes] {
            edge.metadata
                .pointer_mut("/provenance/model_invocation")
                .and_then(serde_json::Value::as_object_mut)
                .expect("receipt object")
                .insert(
                    "raw_provider_error".to_string(),
                    json!("must never become durable"),
                );
        }

        let expected_entry = expected_state(&store, "unsafe-new");
        let expected = expected_state(&store, "unsafe-old");
        store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .expect_err("receipt fields outside the persistence allowlist must fail closed");
        assert_no_contradiction_mutation(store.connection(), "unsafe-old");
    }

    /// #1558: a receipt carrying the new `content_hash`/`memory_id`/`revision`
    /// binding fields is not "unknown field" garbage to the shadow validator --
    /// the contract is unified, not merely tolerated as noise.
    #[test]
    fn confirmed_contradiction_transaction_accepts_receipt_with_content_binding() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("bound-new", vec![])).unwrap();
        store.upsert(&test_entry("bound-old", vec![])).unwrap();
        let (mut contradicts, mut supersedes, at) =
            confirmed_contradiction_edges("bound-new", "bound-old", "complete");
        for edge in [&mut contradicts, &mut supersedes] {
            edge.metadata
                .pointer_mut("/provenance/model_invocation")
                .and_then(serde_json::Value::as_object_mut)
                .expect("receipt object")
                .insert(
                    "content_hash".to_string(),
                    json!("deadbeefcafefeed0011223344556677"),
                );
        }

        let expected_entry = expected_state(&store, "bound-new");
        let expected = expected_state(&store, "bound-old");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .expect(
                "a content-hash-bound receipt is still an allowlisted model-invocation-v1 receipt",
            );
        assert_eq!(outcome, ConfirmedContradictionOutcome::Committed);
    }

    #[test]
    fn confirmed_contradiction_transaction_rejects_blank_content_binding() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store
            .upsert(&test_entry("blank-bound-new", vec![]))
            .unwrap();
        store
            .upsert(&test_entry("blank-bound-old", vec![]))
            .unwrap();
        let (mut contradicts, mut supersedes, at) =
            confirmed_contradiction_edges("blank-bound-new", "blank-bound-old", "complete");
        for edge in [&mut contradicts, &mut supersedes] {
            edge.metadata
                .pointer_mut("/provenance/model_invocation")
                .and_then(serde_json::Value::as_object_mut)
                .expect("receipt object")
                .insert("content_hash".to_string(), json!("   "));
        }

        let expected_entry = expected_state(&store, "blank-bound-new");
        let expected = expected_state(&store, "blank-bound-old");
        store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .expect_err(
                "a present-but-blank content_hash must fail closed, not silently pass through",
            );
        assert_no_contradiction_mutation(store.connection(), "blank-bound-old");
    }

    #[test]
    fn confirmed_contradiction_transaction_rolls_back_when_lifecycle_cas_loses() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("cas-new", vec![])).unwrap();
        store.upsert(&test_entry("cas-old", vec![])).unwrap();
        store
            .upsert(&test_entry("existing-winner", vec![]))
            .unwrap();
        store
            .with_immutable_supersession_transaction(|transaction| {
                transaction.claim_immutable_supersession("cas-old", "existing-winner")
            })
            .unwrap();
        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("cas-new", "cas-old", "complete");
        // Snapshot after the prior claim so the state gate passes and the live
        // lifecycle CAS must reject the competing target.
        let expected_entry = expected_state(&store, "cas-new");
        let expected = expected_state(&store, "cas-old");

        let error = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .expect_err("lost lifecycle CAS must abort both edge writes");
        assert!(
            error.to_string().contains("lifecycle CAS refused"),
            "unexpected competing-claim error: {error}"
        );

        let edge_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memory_edges", [], |row| row.get(0))
            .unwrap();
        let observation_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let winner: Option<String> = store
            .connection()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'cas-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edge_count, 0);
        assert_eq!(observation_count, 0);
        assert_eq!(winner.as_deref(), Some("existing-winner"));
        let receipts: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace = 'memory_supersession_receipts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 1, "a competing claim cannot add a second receipt");
    }

    /// The killer case for the inverse of tachi#1551: enrichment rewrites the
    /// candidate's `summary` **without** bumping `revision`, so neither a revision
    /// guard nor the `superseded_by IS NULL` predicate notices that the verdict is
    /// now about content the database no longer holds.
    #[test]
    fn confirmed_contradiction_skips_a_verdict_about_content_enrichment_rewrote() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("stale-new", vec![])).unwrap();
        store.upsert(&test_entry("stale-old", vec![])).unwrap();
        let expected_entry = expected_state(&store, "stale-new");
        let expected = expected_state(&store, "stale-old");
        let revision_before: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'stale-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(store
            .update_enrichment_fields(
                "stale-old",
                Some("rewritten while the model was being consulted"),
                None,
                None,
                None,
                revision_before,
            )
            .unwrap());
        let revision_after: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'stale-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            revision_after, revision_before,
            "enrichment must not bump revision — that is exactly why revision cannot carry this guard"
        );

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("stale-new", "stale-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
        assert_no_contradiction_mutation(store.connection(), "stale-old");
    }

    /// Revision-bumping drift is caught by the same gate, and is likewise reported
    /// as a skip rather than an error.
    #[test]
    fn confirmed_contradiction_skips_a_verdict_after_a_revision_bumping_rewrite() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("rev-new", vec![])).unwrap();
        store.upsert(&test_entry("rev-old", vec![])).unwrap();
        let expected_entry = expected_state(&store, "rev-new");
        let expected = expected_state(&store, "rev-old");
        let revision_before: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'rev-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(store
            .update_with_revision(
                "rev-old",
                "old fact, rewritten",
                "old fact, rewritten",
                "test",
                &json!({}),
                None,
                revision_before,
            )
            .unwrap());

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("rev-new", "rev-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
        assert_no_contradiction_mutation(store.connection(), "rev-old");
    }

    /// A verdict about a row that has since been deleted must also fail closed:
    /// the snapshot cannot match a row that is not there.
    #[test]
    fn confirmed_contradiction_skips_a_verdict_about_a_deleted_candidate() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("gone-new", vec![])).unwrap();
        store.upsert(&test_entry("gone-old", vec![])).unwrap();
        let expected_entry = expected_state(&store, "gone-new");
        let expected = expected_state(&store, "gone-old");
        assert!(store.delete("gone-old").unwrap());

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("gone-new", "gone-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
        let edge_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE relation IN ('contradicts', 'supersedes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let observation_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(edge_count, 0);
        assert_eq!(observation_count, 0);
    }

    /// tachi#1563 item 1: the entry (the newer fact that triggered detection) can
    /// be rewritten by enrichment during the LLM round-trip exactly like a
    /// candidate can — mirrors
    /// `confirmed_contradiction_skips_a_verdict_about_content_enrichment_rewrote`
    /// above, but on the entry side of the pair. Before this fix the write path
    /// never re-read the entry, so this same rewrite would have gone unnoticed
    /// and the commit would have proceeded.
    #[test]
    fn confirmed_contradiction_skips_a_verdict_about_content_enrichment_rewrote_on_the_entry_side()
    {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store
            .upsert(&test_entry("entry-stale-new", vec![]))
            .unwrap();
        store
            .upsert(&test_entry("entry-stale-old", vec![]))
            .unwrap();
        let expected_entry = expected_state(&store, "entry-stale-new");
        let expected = expected_state(&store, "entry-stale-old");
        let revision_before: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'entry-stale-new'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(store
            .update_enrichment_fields(
                "entry-stale-new",
                Some("rewritten while the model was being consulted"),
                None,
                None,
                None,
                revision_before,
            )
            .unwrap());

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("entry-stale-new", "entry-stale-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
        assert_no_contradiction_mutation(store.connection(), "entry-stale-old");
    }

    /// The entry side of the same guard must also fail closed when the entry was
    /// archived (not merely rewritten) between the read that fed the model and
    /// the write — `archived` is one of the compared fields in
    /// `ExpectedMemoryState::matches`, so no separate lifecycle check is needed
    /// (tachi#1563).
    #[test]
    fn confirmed_contradiction_skips_a_verdict_when_the_entry_was_archived() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store
            .upsert(&test_entry("entry-archived-new", vec![]))
            .unwrap();
        store
            .upsert(&test_entry("entry-archived-old", vec![]))
            .unwrap();
        let expected_entry = expected_state(&store, "entry-archived-new");
        let expected = expected_state(&store, "entry-archived-old");

        assert!(
            store.archive_memory("entry-archived-new").unwrap(),
            "fixture must actually archive the entry"
        );

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("entry-archived-new", "entry-archived-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
        assert_no_contradiction_mutation(store.connection(), "entry-archived-old");
    }

    /// A verdict about an entry that has since been deleted must also fail
    /// closed, mirroring `confirmed_contradiction_skips_a_verdict_about_a_deleted_candidate`
    /// but for the entry side (tachi#1563).
    #[test]
    fn confirmed_contradiction_skips_a_verdict_when_the_entry_was_deleted() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("entry-gone-new", vec![])).unwrap();
        store.upsert(&test_entry("entry-gone-old", vec![])).unwrap();
        let expected_entry = expected_state(&store, "entry-gone-new");
        let expected = expected_state(&store, "entry-gone-old");
        assert!(store.delete("entry-gone-new").unwrap());

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("entry-gone-new", "entry-gone-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
        assert_no_contradiction_mutation(store.connection(), "entry-gone-old");
    }

    /// tachi#1563 item 2: the lifecycle `UPDATE` that supersedes the candidate
    /// must advance `revision`, or a revision-scoped CAS (chief among them
    /// `update_enrichment_fields`) cannot notice the transition and can still
    /// write into a row that has already been superseded.
    #[test]
    fn confirmed_contradiction_supersede_advances_revision_and_blocks_a_stale_enrichment_cas() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("revcas-new", vec![])).unwrap();
        store.upsert(&test_entry("revcas-old", vec![])).unwrap();
        let expected_entry = expected_state(&store, "revcas-new");
        let expected = expected_state(&store, "revcas-old");
        let revision_before: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'revcas-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("revcas-new", "revcas-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();
        assert_eq!(outcome, ConfirmedContradictionOutcome::Committed);

        let revision_after: i64 = store
            .connection()
            .query_row(
                "SELECT revision FROM memories WHERE id = 'revcas-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            revision_after,
            revision_before + 1,
            "supersede must advance revision so revision-scoped observers see the lifecycle transition"
        );

        // An enrichment write that read the row before supersession (and so is
        // still carrying the pre-supersession revision) must lose its CAS rather
        // than land fields on a row that has already been superseded — this is
        // the product-visible reason the revision bump exists: without it, an
        // in-flight enrichment write would silently resurrect content on a
        // memory the graph has already retired.
        let cas_result = store
            .update_enrichment_fields(
                "revcas-old",
                Some("late enrichment racing the supersede"),
                None,
                None,
                None,
                revision_before,
            )
            .unwrap();
        assert!(
            !cas_result,
            "enrichment write against the pre-supersession revision must be refused"
        );

        // The same drift is visible through the read-path snapshot: a candidate
        // matcher built from the pre-supersession row must now disagree with the
        // stored row, both because `revision` moved and because `superseded_by`
        // is no longer NULL.
        let ids = vec!["revcas-old".to_string()];
        let current = crate::db::fetch_by_ids(store.connection(), &ids, true)
            .unwrap()
            .remove("revcas-old")
            .expect("row still exists");
        let superseded_by: Option<String> = store
            .connection()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'revcas-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !expected.matches(&current, superseded_by.as_deref()),
            "the pre-supersession snapshot must no longer match the post-supersede row"
        );
    }

    /// Review follow-up on tachi#1563: `ExpectedMemoryState::matches` no longer
    /// compares `recall_count` / `query_diversity` (see the docstring on
    /// [`ExpectedMemoryState`]). Bump both, through the real search-recording
    /// path, on the entry *and* the candidate between the frozen snapshot and the
    /// write — the exact race unrelated search traffic hitting the hottest row
    /// during the LLM round-trip produces — and confirm the verdict still
    /// commits instead of spuriously landing in `StaleSkipped`.
    #[test]
    fn confirmed_contradiction_commits_despite_recall_count_drift_on_both_sides() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("hot-new", vec![])).unwrap();
        store.upsert(&test_entry("hot-old", vec![])).unwrap();
        let expected_entry = expected_state(&store, "hot-new");
        let expected = expected_state(&store, "hot-old");

        // Unrelated search traffic lands on both rows while this verdict's model
        // round-trip is still in flight. The store search doorway supplies the
        // write authorization needed to record recall on its guarded connection.
        store
            .search(
                "hot",
                Some(crate::SearchOptions {
                    top_k: 2,
                    mmr_threshold: None,
                    recall_config: Some(crate::RecallConfig::default()),
                    ..Default::default()
                }),
            )
            .unwrap();
        let (recall_new, recall_old, diversity_new, diversity_old): (i64, i64, i64, i64) = store
            .connection()
            .query_row(
                "SELECT
                    (SELECT recall_count FROM memories WHERE id = 'hot-new'),
                    (SELECT recall_count FROM memories WHERE id = 'hot-old'),
                    (SELECT query_diversity FROM memories WHERE id = 'hot-new'),
                    (SELECT query_diversity FROM memories WHERE id = 'hot-old')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert!(
            recall_new > 0 && recall_old > 0 && diversity_new > 0 && diversity_old > 0,
            "setup must actually bump recall_count and query_diversity on both rows: \
             recall_new={recall_new} recall_old={recall_old} diversity_new={diversity_new} \
             diversity_old={diversity_old}"
        );

        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("hot-new", "hot-old", "complete");
        let outcome = store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        assert_eq!(
            outcome,
            ConfirmedContradictionOutcome::Committed,
            "recall_count/query_diversity drift alone must not sink an otherwise-valid verdict"
        );
    }

    /// tachi#1646 spot check: the contradiction pipeline — the only writer
    /// census-classified `ModelReceiptBacked` — stamps that class on both graph
    /// projections it writes, unconditionally (hard-coded in
    /// `MemoryStore::commit_confirmed_contradiction`, not caller-configurable).
    #[test]
    fn confirmed_contradiction_transaction_stamps_model_receipt_backed_authority() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store.upsert(&test_entry("auth-mrb-new", vec![])).unwrap();
        store.upsert(&test_entry("auth-mrb-old", vec![])).unwrap();
        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("auth-mrb-new", "auth-mrb-old", "complete");
        let expected_entry = expected_state(&store, "auth-mrb-new");
        let expected = expected_state(&store, "auth-mrb-old");

        store
            .commit_confirmed_contradiction(
                &contradicts,
                &supersedes,
                &at,
                &expected_entry,
                &expected,
            )
            .unwrap();

        let edges =
            crate::db::get_edges(store.connection(), "auth-mrb-new", "outgoing", None).unwrap();
        assert_eq!(edges.len(), 2, "contradicts + supersedes");
        for edge in &edges {
            assert_eq!(
                crate::db::edge_authority(edge),
                Some(crate::db::EdgeAuthority::ModelReceiptBacked),
                "both contradiction-pipeline projections must stamp ModelReceiptBacked: {edge:?}"
            );
        }
    }
}
