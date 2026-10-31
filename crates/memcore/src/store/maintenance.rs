//! Daily truth-maintenance helpers on [`MemoryStore`].

use rusqlite::{Transaction, TransactionBehavior};
use serde::Serialize;

use crate::{db, error::MemoryError, MemoryEntry, MemoryStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierHealthCounts {
    pub total_active: i64,
    pub consolidated: i64,
}

/// Why a missing historical boundary cannot be inferred without inventing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupersededValidityUnresolvedReason {
    InvalidValidFrom,
    InvalidUpdatedAt,
    NonPositiveWindow,
    InvalidRevision,
    ProtectedRow,
}

/// Per-row evidence from an explicit historical-boundary repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SupersededValidityRepairOutcome {
    Repaired {
        id: String,
        superseded_by: String,
        valid_from: String,
        original_updated_at: String,
        inferred_valid_until: String,
        revision_before: i64,
        revision_after: i64,
    },
    Unresolved {
        id: String,
        reason: SupersededValidityUnresolvedReason,
    },
}

/// A committed operation report, not proof of the true supersession time.
/// The database owner must retain this separately from snapshot-import receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupersededValidityRepairReport {
    pub inference_policy: &'static str,
    pub db_label: String,
    pub physical_db_identity: Option<String>,
    pub examined: usize,
    pub repaired: usize,
    pub unresolved: usize,
    pub outcomes: Vec<SupersededValidityRepairOutcome>,
}

impl MemoryStore {
    /// Explicit database-owner maintenance for superseded rows missing an end.
    ///
    /// This infers `valid_until` from the stored `updated_at` only when it is
    /// parseable and strictly later than `valid_from` (or its timestamp fallback).
    /// It cannot recover the actual supersession instant. Unresolvable/protected
    /// rows remain unchanged and are listed; historical reads still refuse them.
    /// Existing ends and non-superseded rows are untouched. Original timestamps
    /// are preserved and each repaired revision advances once.
    ///
    /// All writes commit atomically; database errors roll the operation back.
    /// Nothing calls this on open or during import. After `import_snapshot_batch`,
    /// verify and retain its original checksums before explicitly calling this
    /// method and separately retaining this report. No schema migration occurs.
    /// This owner API must not be exposed as an untrusted model repair tool.
    pub fn repair_superseded_validity_from_updated_at(
        &mut self,
    ) -> Result<SupersededValidityRepairReport, MemoryError> {
        struct Candidate {
            id: String,
            superseded_by: String,
            valid_from: Option<String>,
            timestamp: Option<String>,
            updated_at: Option<String>,
            revision: i64,
            path: String,
            category: String,
            topic: String,
        }
        let mut report = SupersededValidityRepairReport {
            inference_policy: "superseded-validity-from-updated-at-v1",
            db_label: self.db_label.clone(),
            physical_db_identity: self.opened_physical_db_identity().map(str::to_owned),
            examined: 0,
            repaired: 0,
            unresolved: 0,
            outcomes: Vec::new(),
        };
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT id, superseded_by, valid_from, timestamp, updated_at, revision,
                        path, category, topic FROM memories
                 WHERE superseded_by IS NOT NULL AND valid_until IS NULL ORDER BY id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok(Candidate {
                        id: row.get(0)?,
                        superseded_by: row.get(1)?,
                        valid_from: row.get(2)?,
                        timestamp: row.get(3)?,
                        updated_at: row.get(4)?,
                        revision: row.get(5)?,
                        path: row.get(6)?,
                        category: row.get(7)?,
                        topic: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        report.examined = candidates.len();
        for candidate in candidates {
            use SupersededValidityUnresolvedReason as Reason;
            let decision = (|| -> Result<(String, String, i64), Reason> {
                db::refuse_reserved_write_identity(
                    &candidate.id,
                    &crate::path_router::normalize_path(&candidate.path),
                    &candidate.topic,
                    false,
                    false,
                )
                .map_err(|_| Reason::ProtectedRow)?;
                crate::path_router::validate_retired_sticky_write(
                    &candidate.path,
                    &candidate.category,
                )
                .map_err(|_| Reason::ProtectedRow)?;
                let from = candidate
                    .valid_from
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .or(candidate.timestamp.as_deref())
                    .ok_or(Reason::InvalidValidFrom)?;
                let from = db::normalize_utc_iso(from).map_err(|_| Reason::InvalidValidFrom)?;
                let updated = candidate
                    .updated_at
                    .as_deref()
                    .ok_or(Reason::InvalidUpdatedAt)?;
                let until = db::normalize_utc_iso(updated).map_err(|_| Reason::InvalidUpdatedAt)?;
                if until <= from {
                    return Err(Reason::NonPositiveWindow);
                }
                let revision = candidate
                    .revision
                    .checked_add(1)
                    .filter(|_| candidate.revision > 0)
                    .ok_or(Reason::InvalidRevision)?;
                Ok((from, until, revision))
            })();
            match decision {
                Ok((valid_from, inferred_valid_until, revision_after)) => {
                    let changed = tx.execute(
                        "UPDATE memories SET valid_until = ?1, revision = ?2
                         WHERE id = ?3 AND superseded_by = ?4 AND valid_until IS NULL AND revision = ?5",
                        rusqlite::params![inferred_valid_until, revision_after, candidate.id,
                            candidate.superseded_by, candidate.revision],
                    )?;
                    if changed != 1 {
                        return Err(MemoryError::Internal(
                            "superseded validity repair lost its transaction snapshot".into(),
                        ));
                    }
                    report.repaired += 1;
                    report
                        .outcomes
                        .push(SupersededValidityRepairOutcome::Repaired {
                            id: candidate.id,
                            superseded_by: candidate.superseded_by,
                            valid_from,
                            original_updated_at: candidate.updated_at.expect("validated timestamp"),
                            inferred_valid_until,
                            revision_before: candidate.revision,
                            revision_after,
                        });
                }
                Err(reason) => {
                    report.unresolved += 1;
                    report
                        .outcomes
                        .push(SupersededValidityRepairOutcome::Unresolved {
                            id: candidate.id,
                            reason,
                        });
                }
            }
        }
        tx.commit()?;
        Ok(report)
    }

    /// Archive stale low-value memories using this store's host-injected
    /// recall configuration (tachi#1585 D5; was the process-wide
    /// `RecallConfig::get()`).
    pub fn archive_stale_low_value_memories(&self) -> Result<usize, MemoryError> {
        self.archive_stale_low_value_memories_with_config(&self.policy.recall)
    }

    /// Archive low-importance memories that have no qualifying activity and
    /// are older than 60 days, sparing permanent/pinned/durable retention
    /// policies.
    ///
    /// With provenance recency enabled, only `last_use_at` protects a row:
    /// merely displaying it in search must not turn exposure into retention.
    /// The explicit rollback arm preserves the historical `access_count = 0`
    /// behavior.
    pub fn archive_stale_low_value_memories_with_config(
        &self,
        recall_config: &crate::RecallConfig,
    ) -> Result<usize, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let archived_at = db::now_utc_iso();
        let (activity_predicate, receipt_predicate, activity_column) =
            if recall_config.use_provenance_recency {
                (
                    "last_use_at IS NULL",
                    "stale_low_value_never_used",
                    "last_use_at",
                )
            } else {
                (
                    "access_count = 0",
                    "stale_low_value_never_search_accessed",
                    "access_count",
                )
            };
        let candidate_sql = format!(
            "SELECT id FROM memories
             WHERE archived = 0
               AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned', 'durable')
               AND importance < 0.70
               AND {activity_predicate}
               AND julianday(COALESCE(NULLIF(created_at, ''), timestamp)) < julianday('now', '-60 days')"
        );
        let candidate_ids = {
            let mut stmt = tx.prepare(&candidate_sql)?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        };
        let mut ordinary_ids = Vec::with_capacity(candidate_ids.len());
        for id in candidate_ids {
            match db::refuse_retired_sticky_row_within_tx(&tx, &id, "archived by stale maintenance")
            {
                Ok(()) => ordinary_ids.push(id),
                Err(MemoryError::InvalidArg(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let mut ids = Vec::with_capacity(ordinary_ids.len());
        for id in ordinary_ids {
            if tx.execute(
                "UPDATE memories
                 SET archived = 1, updated_at = ?1, revision = revision + 1
                 WHERE id = ?2",
                rusqlite::params![archived_at, id],
            )? == 1
            {
                ids.push(id);
            }
        }
        ids.sort();
        if !ids.is_empty() {
            db::write_gc_archived_receipt(
                &tx,
                &archived_at,
                60,
                serde_json::json!([{
                    "predicate": receipt_predicate,
                    "activity_column": activity_column,
                    "recency_column": "created_at_or_timestamp",
                    "importance_below": 0.70,
                    "retention_scope": "not_permanent_pinned_or_durable",
                    "archived_count": ids.len(),
                    "memory_ids": ids,
                }]),
                ids.len(),
                "memcore::store::maintenance::MemoryStore::archive_stale_low_value_memories_with_config",
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
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let candidate_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM memories
                 WHERE archived = 0 AND tier = 'raw' AND recall_count >= 3
                   AND query_diversity >= 3
                   AND COALESCE(retention_policy, '') NOT IN ('ephemeral')",
            )?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        };
        let mut ordinary_ids = Vec::with_capacity(candidate_ids.len());
        for id in candidate_ids {
            match db::refuse_retired_sticky_row_within_tx(
                &tx,
                &id,
                "promoted by recall maintenance",
            ) {
                Ok(()) => ordinary_ids.push(id),
                Err(MemoryError::InvalidArg(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let mut changed = 0usize;
        for id in ordinary_ids {
            changed += tx.execute(
                "UPDATE memories
                 SET tier = 'consolidated', updated_at = datetime('now')
                 WHERE id = ?1",
                [&id],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Load active entries that still lack an embedding vector, highest
    /// importance first (non-raw rows before raw when raw embedding is enabled).
    /// Excludes `anchor:`-prefixed rows (tachi#773 item 4: anchors are plumbing
    /// rows for the memory graph, never content that should burn embedding
    /// budget or surface as recall).
    pub fn entries_missing_vectors(&self, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        // tachi#1585 D5: this store's `KernelPolicy::embed`, not a
        // `TACHI_EMBED_RAW_TIER` env read.
        let tier_filter = crate::embed_config::embed_raw_tier_sql_filter(
            "m.",
            self.policy.embed.raw_tier_enabled,
        );
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
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        db::refuse_retired_sticky_row_within_tx(&tx, id, "promoted to durable")?;
        tx.execute(
            "UPDATE memories
             SET importance = 0.7, retention_policy = 'durable', updated_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id],
        )?;
        tx.commit()?;
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

    use crate::db::{self, AccessEventKind};
    use crate::types::MemoryEntry;
    use crate::MemoryStore;

    fn legacy_superseded(id: &str) -> crate::PortableImportEntry {
        let mut entry = test_entry(id);
        entry.source = "migration".into();
        crate::PortableImportEntry {
            entry,
            created_at: "2026-07-05T00:00:00Z".into(),
            updated_at: "2026-08-01T08:00:00+08:00".into(),
            superseded_by: Some("winner".into()),
        }
    }

    #[test]
    fn superseded_validity_repair_is_explicit_selective_and_idempotent() {
        use super::{
            SupersededValidityRepairOutcome as Outcome,
            SupersededValidityUnresolvedReason as Reason,
        };
        let mut store = MemoryStore::open_in_memory().unwrap();
        let good = legacy_superseded("good");
        let mut archived = legacy_superseded("archived");
        archived.entry.archived = true;
        let mut closed = legacy_superseded("closed");
        closed.entry.valid_until = Some("2026-07-20T00:00:00Z".into());
        let mut active = legacy_superseded("winner");
        active.superseded_by = None;
        let mut equal = legacy_superseded("equal");
        equal.updated_at = equal.entry.timestamp.clone();
        let mut backwards = legacy_superseded("backwards");
        backwards.updated_at = "2026-01-01T00:00:00Z".into();
        let mut bad_from = legacy_superseded("bad-from");
        bad_from.entry.valid_from = "not-a-time".into();
        let mut bad_updated = legacy_superseded("bad-updated");
        bad_updated.updated_at.clear();
        let mut max_revision = legacy_superseded("max-revision");
        max_revision.entry.revision = i64::MAX;
        let original = vec![
            good,
            archived,
            closed,
            active,
            equal,
            backwards,
            bad_from,
            bad_updated,
            max_revision,
        ];
        let receipt = store.import_snapshot_batch(&original).unwrap();
        assert_eq!(receipt.rows_imported, original.len());
        let before: Vec<_> = original
            .iter()
            .map(|row| {
                store
                    .get_with_options(&row.entry.id, true)
                    .unwrap()
                    .unwrap()
            })
            .collect();
        assert!(store.get("good").unwrap().unwrap().valid_until.is_none());
        let generation_before = db::search_generation(store.connection()).unwrap();

        let report = store.repair_superseded_validity_from_updated_at().unwrap();
        assert_eq!(
            (report.examined, report.repaired, report.unresolved),
            (7, 2, 5)
        );
        assert_eq!(
            report.inference_policy,
            "superseded-validity-from-updated-at-v1"
        );
        assert_eq!(report.outcomes.len(), report.examined);
        assert!(report.outcomes.iter().any(|row| matches!(row,
            Outcome::Repaired { id, original_updated_at, inferred_valid_until, revision_before: 1, revision_after: 2, .. }
            if id == "good" && original_updated_at == "2026-08-01T08:00:00+08:00"
                && inferred_valid_until == "2026-08-01T00:00:00.000Z")));
        for (id, expected) in [
            ("equal", Reason::NonPositiveWindow),
            ("backwards", Reason::NonPositiveWindow),
            ("bad-from", Reason::InvalidValidFrom),
            ("bad-updated", Reason::InvalidUpdatedAt),
            ("max-revision", Reason::InvalidRevision),
        ] {
            assert!(report.outcomes.contains(&Outcome::Unresolved {
                id: id.into(),
                reason: expected
            }));
        }
        for (row, mut expected) in original.iter().zip(before) {
            if matches!(row.entry.id.as_str(), "good" | "archived") {
                expected.valid_until = Some("2026-08-01T00:00:00.000Z".into());
                expected.revision += 1;
            }
            let actual = store
                .get_with_options(&row.entry.id, true)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            let updated: String = store
                .conn
                .query_row(
                    "SELECT updated_at FROM memories WHERE id=?1",
                    [&row.entry.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(updated, row.updated_at);
        }
        let generation_after = db::search_generation(store.connection()).unwrap();
        assert_eq!(generation_after, generation_before + 2);
        let second = store.repair_superseded_validity_from_updated_at().unwrap();
        assert_eq!(
            (second.examined, second.repaired, second.unresolved),
            (5, 0, 5)
        );
        assert_eq!(
            db::search_generation(store.connection()).unwrap(),
            generation_after
        );
    }

    #[test]
    fn superseded_validity_repair_rolls_back_on_database_error() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        store
            .import_snapshot_batch(&[legacy_superseded("a"), legacy_superseded("b")])
            .unwrap();
        // The existing generation trigger admits one update, then aborts the
        // second. No fixture DDL or authorizer bypass is needed.
        store
            .conn
            .execute(
                "UPDATE memory_search_generation SET generation=?1 WHERE id=1",
                [i64::MAX - 1],
            )
            .unwrap();
        let error = store
            .repair_superseded_validity_from_updated_at()
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("memory search generation exhausted"));
        for id in ["a", "b"] {
            let row = store.get(id).unwrap().unwrap();
            assert!(row.valid_until.is_none());
            assert_eq!(row.revision, 1);
        }
        assert_eq!(
            db::search_generation(store.connection()).unwrap(),
            i64::MAX - 1
        );
    }

    #[test]
    fn superseded_validity_repair_preserves_retired_and_reserved_rows() {
        use super::{
            SupersededValidityRepairOutcome as Outcome,
            SupersededValidityUnresolvedReason as Reason,
        };
        let mut store = MemoryStore::open_in_memory().unwrap();
        store
            .import_snapshot_batch(&[
                legacy_superseded("ordinary"),
                legacy_superseded("sticky"),
                legacy_superseded("reserved"),
            ])
            .unwrap();
        {
            let _authorization =
                db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
            store.conn.execute("UPDATE memories SET path='/sticky/legacy', category='sticky' WHERE id='sticky'", []).unwrap();
            store
                .conn
                .execute(
                    "UPDATE memories SET id='wiki-rem:legacy' WHERE id='reserved'",
                    [],
                )
                .unwrap();
        }
        let report = store.repair_superseded_validity_from_updated_at().unwrap();
        assert_eq!((report.repaired, report.unresolved), (1, 2));
        for id in ["sticky", "wiki-rem:legacy"] {
            assert!(report.outcomes.contains(&Outcome::Unresolved {
                id: id.into(),
                reason: Reason::ProtectedRow
            }));
            assert!(store.get(id).unwrap().unwrap().valid_until.is_none());
        }
    }

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

    fn retire_as_sticky_fixture(store: &MemoryStore, id: &str) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize raw legacy sticky fixture");
        store
            .connection()
            .execute(
                "UPDATE memories SET path='/sticky/legacy', category='sticky' WHERE id=?1",
                [id],
            )
            .expect("retire fixture row");
    }

    fn retire_as_malformed_sticky_fixture(store: &MemoryStore, id: &str) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize raw legacy sticky fixture");
        store
            .connection()
            .execute(
                "UPDATE memories SET path='//STICKY///legacy', category='fact' WHERE id=?1",
                [id],
            )
            .expect("retire malformed fixture row");
    }

    #[test]
    fn maintenance_skips_retired_sticky_while_ordinary_rows_progress() {
        let mut archive_store = MemoryStore::open_in_memory().expect("open archive store");
        for id in ["ordinary-stale", "sticky-stale"] {
            archive_store
                .upsert(&test_entry(id))
                .expect("seed stale row");
            backdate_created_at(&archive_store, id);
        }
        retire_as_sticky_fixture(&archive_store, "sticky-stale");
        assert_eq!(archive_store.archive_stale_low_value_memories().unwrap(), 1);
        assert!(
            archive_store
                .get_with_options("ordinary-stale", true)
                .unwrap()
                .unwrap()
                .archived
        );
        let sticky_archive: (i64, i64) = archive_store
            .connection()
            .query_row(
                "SELECT archived,revision FROM memories WHERE id='sticky-stale'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sticky_archive, (0, 1), "GC must not touch retired sticky");

        let mut promotion_store = MemoryStore::open_in_memory().expect("open promotion store");
        for id in ["ordinary-promote", "sticky-promote"] {
            let mut entry = test_entry(id);
            entry.recall_count = 3;
            entry.query_diversity = 3;
            promotion_store.upsert(&entry).expect("seed promotion row");
        }
        retire_as_sticky_fixture(&promotion_store, "sticky-promote");
        assert_eq!(
            promotion_store
                .promote_diversely_recalled_raw_memories(&crate::RecallConfig {
                    use_provenance_recency: false,
                    ..crate::RecallConfig::default()
                })
                .unwrap(),
            1
        );
        assert_eq!(
            promotion_store
                .get("ordinary-promote")
                .unwrap()
                .unwrap()
                .tier,
            "consolidated"
        );
        assert_eq!(
            promotion_store.get("sticky-promote").unwrap().unwrap().tier,
            "raw"
        );
    }

    #[test]
    fn maintenance_archive_skips_malformed_sticky_while_ordinary_progresses() {
        let mut store = MemoryStore::open_in_memory().expect("open archive store");
        for id in ["ordinary-stale", "sticky-stale"] {
            store.upsert(&test_entry(id)).expect("seed stale row");
            backdate_created_at(&store, id);
        }
        retire_as_malformed_sticky_fixture(&store, "sticky-stale");

        assert_eq!(store.archive_stale_low_value_memories().unwrap(), 1);
        assert!(
            store
                .get_with_options("ordinary-stale", true)
                .unwrap()
                .unwrap()
                .archived
        );
        assert_eq!(
            store
                .connection()
                .query_row(
                    "SELECT path, archived, revision FROM memories WHERE id='sticky-stale'",
                    [],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?
                    )),
                )
                .unwrap(),
            ("//STICKY///legacy".to_string(), 0, 1)
        );
    }

    #[test]
    fn maintenance_promotion_skips_malformed_sticky_while_ordinary_progresses() {
        let mut store = MemoryStore::open_in_memory().expect("open promotion store");
        for id in ["ordinary-promote", "sticky-promote"] {
            let mut entry = test_entry(id);
            entry.recall_count = 3;
            entry.query_diversity = 3;
            store.upsert(&entry).expect("seed promotion row");
        }
        retire_as_malformed_sticky_fixture(&store, "sticky-promote");

        assert_eq!(
            store
                .promote_diversely_recalled_raw_memories(&crate::RecallConfig {
                    use_provenance_recency: false,
                    ..crate::RecallConfig::default()
                })
                .unwrap(),
            1
        );
        assert_eq!(
            store.get("ordinary-promote").unwrap().unwrap().tier,
            "consolidated"
        );
        let sticky = store.get("sticky-promote").unwrap().unwrap();
        assert_eq!(
            (sticky.path, sticky.tier),
            ("//STICKY///legacy".to_string(), "raw".to_string())
        );
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
    fn unattended_archival_uses_genuine_use_by_default_and_preserves_legacy_rollback() {
        let mut provenance_store = MemoryStore::open_in_memory().expect("open provenance store");
        for id in ["display-only", "genuinely-used"] {
            provenance_store
                .upsert(&test_entry(id))
                .expect("seed provenance row");
            backdate_created_at(&provenance_store, id);
        }
        provenance_store
            .connection()
            .execute(
                "UPDATE memories SET access_count = 7 WHERE id = 'display-only'",
                [],
            )
            .expect("record display-only exposure");
        provenance_store
            .connection()
            .execute(
                "UPDATE memories SET access_count = 1, last_use_at = '2026-01-01T00:00:00Z' WHERE id = 'genuinely-used'",
                [],
            )
            .expect("record genuine use");

        assert_eq!(
            provenance_store
                .archive_stale_low_value_memories_with_config(&crate::RecallConfig::default())
                .expect("archive with provenance semantics"),
            1,
            "display-only exposure must not protect an otherwise stale low-value row"
        );
        assert!(
            provenance_store
                .get_with_options("display-only", true)
                .unwrap()
                .unwrap()
                .archived
        );
        assert!(
            !provenance_store
                .get("genuinely-used")
                .unwrap()
                .unwrap()
                .archived,
            "genuine use must protect the row under the default provenance arm"
        );

        let mut legacy_store = MemoryStore::open_in_memory().expect("open legacy store");
        legacy_store
            .upsert(&test_entry("display-only"))
            .expect("seed legacy row");
        backdate_created_at(&legacy_store, "display-only");
        legacy_store
            .connection()
            .execute(
                "UPDATE memories SET access_count = 7 WHERE id = 'display-only'",
                [],
            )
            .expect("record legacy exposure");
        let legacy_config = crate::RecallConfig {
            use_provenance_recency: false,
            ..crate::RecallConfig::default()
        };
        assert_eq!(
            legacy_store
                .archive_stale_low_value_memories_with_config(&legacy_config)
                .expect("archive with legacy semantics"),
            0,
            "the explicit rollback arm must preserve access_count behavior"
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
        assert_eq!(pass["predicate"], json!("stale_low_value_never_used"));
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
        // tachi#1585 D5: `entries_missing_vectors` reads
        // `self.policy.embed.raw_tier_enabled` (host-injected), not a
        // `TACHI_EMBED_RAW_TIER` env read, so this test drives the gate
        // through the store's `KernelPolicy` directly instead of mutating
        // process env.
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        assert!(store.vec_available, "sqlite-vec required for this test");
        assert!(
            store.kernel_policy().embed.raw_tier_enabled,
            "pure default matches the historical TACHI_EMBED_RAW_TIER-unset behavior: on"
        );

        let mut missing = test_entry("missing");
        missing.tier = "consolidated".to_string();
        store.upsert(&missing).expect("seed missing");
        let mut embedded = test_entry("embedded");
        embedded.tier = "consolidated".to_string();
        embedded.vector = Some(vec![0.5; 1024]);
        store.upsert(&embedded).expect("seed embedded");
        store.upsert(&test_entry("raw-entry")).expect("seed raw");

        store.policy.embed.raw_tier_enabled = true;
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

        store.policy.embed.raw_tier_enabled = false;
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
