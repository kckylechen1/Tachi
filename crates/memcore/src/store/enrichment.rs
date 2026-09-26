//! Enrichment, FTS, and vector maintenance methods on [`MemoryStore`].

use crate::{db, error::MemoryError, types::ExpectedMemoryState, MemoryStore};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use rusqlite::{params, TransactionBehavior};

pub const ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS: i64 = 3;

/// Persisted model-invocation receipts paired with the generated enrichment
/// fields that they produced.
///
/// The values are serialized forms of tachi-llm's
/// `PersistedModelInvocationReceiptV1`; MemCore deliberately does not mirror
/// that schema. A receipt is accepted only alongside the generated field for
/// its stage, inside the same revision-checked transaction.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnrichmentInvocationReceipts<'a> {
    pub summary: Option<&'a serde_json::Value>,
    pub metadata: Option<&'a serde_json::Value>,
    pub keywords: Option<&'a serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentRetryCandidate {
    pub id: String,
    pub text: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub revision: i64,
    pub needs_embedding: bool,
    pub needs_summary: bool,
    pub needs_metadata: bool,
    pub failed_stage: String,
    pub attempts: i64,
    pub max_attempts: i64,
}

/// One operator-inspected summary-backfill candidate row, fetched by exact id.
///
/// `text` is the full stored body; callers that report plans must not echo it
/// into operator-facing output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryBackfillRow {
    pub id: String,
    pub text: String,
    pub has_summary: bool,
    pub archived: bool,
    pub revision: i64,
}

fn now_utc_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn retry_backoff_until(next_attempt: i64) -> String {
    let attempt = next_attempt.clamp(1, ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS);
    let seconds = 60_i64.saturating_mul(1_i64 << (attempt - 1));
    (Utc::now() + ChronoDuration::seconds(seconds)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_json_string_list(raw: String) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()
}

impl MemoryStore {
    /// Revision-checked update used for optimistic locking in merge flows.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_revision(
        &mut self,
        id: &str,
        new_text: &str,
        new_summary: &str,
        new_source: &str,
        new_metadata: &serde_json::Value,
        new_vec: Option<&[f32]>,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        if crate::namespace::is_reserved_wiki_rem_id(id) {
            return Err(MemoryError::InvalidArg(format!(
                "invariant: reserved REM operation {id} cannot be revision-updated through a generic enrichment seam"
            )));
        }
        let metadata_json = serde_json::to_string(new_metadata)?;
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };

        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("update_with_revision", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::update_with_revision(
                &mut self.conn,
                id,
                new_text,
                new_summary,
                new_source,
                &metadata_json,
                vec_blob.as_deref(),
                expected_revision,
            )
        })
    }

    /// Apply an ordinary revision update only when a typed complete-state
    /// snapshot still matches under the same `BEGIN IMMEDIATE` transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_revision_if_expected_state(
        &mut self,
        id: &str,
        new_text: &str,
        new_summary: &str,
        new_source: &str,
        new_metadata: &serde_json::Value,
        new_vec: Option<&[f32]>,
        expected: &ExpectedMemoryState,
    ) -> Result<bool, MemoryError> {
        if crate::namespace::is_reserved_wiki_rem_id(id) {
            return Err(MemoryError::InvalidArg(format!(
                "invariant: reserved REM operation {id} cannot be revision-updated through a generic enrichment seam"
            )));
        }
        let metadata_json = serde_json::to_string(new_metadata)?;
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("update_with_revision_if_expected_state", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::update_with_revision_if_expected_state(
                &mut self.conn,
                id,
                new_text,
                new_summary,
                new_source,
                &metadata_json,
                vec_blob.as_deref(),
                expected,
            )
        })
    }

    /// Atomically write final migration metadata and supersede an exact source
    /// state. A mismatch performs zero writes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn supersede_with_metadata_if_expected_state(
        &mut self,
        id: &str,
        superseded_by: &str,
        new_metadata: &serde_json::Value,
        expected: &ExpectedMemoryState,
    ) -> Result<bool, MemoryError> {
        if crate::namespace::is_reserved_wiki_rem_id(id) {
            return Err(MemoryError::InvalidArg(format!(
                "invariant: reserved REM operation {id} cannot be superseded through a migration seam"
            )));
        }
        let metadata_json = serde_json::to_string(new_metadata)?;
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked(
            "supersede_with_metadata_if_expected_state",
            &db_label,
            || {
                let _authorization = db::authorize_reserved_reference_write(&authorization)?;
                db::supersede_with_metadata_if_expected_state(
                    &mut self.conn,
                    id,
                    superseded_by,
                    &metadata_json,
                    expected,
                )
            },
        )
    }

    /// Atomically archive an exact deterministic occupant and replace its
    /// migration metadata. The row is retained for audit and replay diagnosis.
    pub fn archive_with_metadata_if_expected_state(
        &mut self,
        id: &str,
        new_metadata: &serde_json::Value,
        expected: &ExpectedMemoryState,
    ) -> Result<bool, MemoryError> {
        if crate::namespace::is_reserved_wiki_rem_id(id) {
            return Err(MemoryError::InvalidArg(format!(
                "invariant: reserved REM operation {id} cannot be archived through a migration seam"
            )));
        }
        let metadata_json = serde_json::to_string(new_metadata)?;
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("archive_with_metadata_if_expected_state", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::archive_with_metadata_if_expected_state(
                &mut self.conn,
                id,
                &metadata_json,
                expected,
            )
        })
    }

    /// Atomically restore an exact archived occupant to the active lifecycle
    /// and replace its migration metadata in the same transaction. Inverse of
    /// [`MemoryStore::archive_with_metadata_if_expected_state`], used by the
    /// Wiki corpus in-band repair of rows a sibling-worker race archived.
    pub fn restore_with_metadata_if_expected_state(
        &mut self,
        id: &str,
        new_metadata: &serde_json::Value,
        expected: &ExpectedMemoryState,
    ) -> Result<bool, MemoryError> {
        if crate::namespace::is_reserved_wiki_rem_id(id) {
            return Err(MemoryError::InvalidArg(format!(
                "invariant: reserved REM operation {id} cannot be restored through a migration seam"
            )));
        }
        let metadata_json = serde_json::to_string(new_metadata)?;
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("restore_with_metadata_if_expected_state", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::restore_with_metadata_if_expected_state(
                &mut self.conn,
                id,
                &metadata_json,
                expected,
            )
        })
    }

    /// Update only enrichment fields (summary, vector, keywords, entities) with revision check.
    /// Returns false if revision mismatch (entry was updated since enrichment started).
    pub fn update_enrichment_fields(
        &mut self,
        id: &str,
        new_summary: Option<&str>,
        new_vec: Option<&[f32]>,
        new_keywords: Option<&[String]>,
        new_entities: Option<&[String]>,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        self.update_enrichment_fields_with_receipts(
            id,
            new_summary,
            new_vec,
            new_keywords,
            new_entities,
            expected_revision,
            EnrichmentInvocationReceipts::default(),
        )
    }

    /// Update enrichment fields and their successful model invocation receipts
    /// under one revision-checked SQLite transaction.
    ///
    /// A receipt without its corresponding generated field is rejected. This
    /// prevents a skipped or empty model stage from becoming a receipt-only
    /// success record.
    #[allow(clippy::too_many_arguments)]
    pub fn update_enrichment_fields_with_receipts(
        &mut self,
        id: &str,
        new_summary: Option<&str>,
        new_vec: Option<&[f32]>,
        new_keywords: Option<&[String]>,
        new_entities: Option<&[String]>,
        expected_revision: i64,
        receipts: EnrichmentInvocationReceipts<'_>,
    ) -> Result<bool, MemoryError> {
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };
        let summary_receipt = receipts.summary.map(serde_json::to_string).transpose()?;
        let metadata_receipt = receipts.metadata.map(serde_json::to_string).transpose()?;
        let keywords_receipt = receipts.keywords.map(serde_json::to_string).transpose()?;
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("update_enrichment_fields", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::update_enrichment_fields(
                &mut self.conn,
                id,
                new_summary,
                vec_blob.as_deref(),
                new_keywords,
                new_entities,
                expected_revision,
                summary_receipt.as_deref(),
                metadata_receipt.as_deref(),
                keywords_receipt.as_deref(),
            )
        })
    }

    /// Record an asynchronous enrichment failure on the memory metadata.
    pub fn record_enrichment_failure(
        &self,
        id: &str,
        stage: &str,
        error: &str,
    ) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::record_enrichment_failure(&self.conn, id, stage, error)
    }

    /// Record an asynchronous enrichment failure only when the row is still
    /// at `expected_revision`. Returns `false` — with the row untouched —
    /// when a concurrent writer moved it on, so a stale sweep observation
    /// cannot pollute the new revision's metadata or `updated_at`.
    pub fn record_enrichment_failure_if_revision(
        &self,
        id: &str,
        stage: &str,
        error: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::record_enrichment_failure_if_revision(&self.conn, id, stage, error, expected_revision)
    }

    /// Set write-side keyword enrichment status (`enriched`/`pending`/`skipped`/`failed`).
    pub fn set_keyword_enrichment_status(&self, id: &str, status: &str) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::set_keyword_enrichment_status(&self.conn, id, status)
    }

    /// Stamp `keywords_status=pending` only when current status is absent or already
    /// pending — never overwrite a terminal status (`enriched`/`skipped`/`failed`).
    pub fn set_keyword_enrichment_pending_if_unset(&self, id: &str) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::set_keyword_enrichment_pending_if_unset(&self.conn, id)
    }

    /// Claim failed auth-class enrichment rows for retry after a vault unlock.
    ///
    /// Retry state lives in metadata, so no schema migration is needed: each
    /// claim increments `enrichment.retry.attempts` and pushes
    /// `next_retry_at` forward with bounded exponential backoff.
    pub fn claim_auth_failed_enrichment_retries(
        &mut self,
        limit: usize,
    ) -> Result<Vec<EnrichmentRetryCandidate>, MemoryError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now = now_utc_iso();
        let max_attempts = ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS;
        let limit = limit.min(256) as i64;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let rows = {
            let mut stmt = tx.prepare(
                r#"SELECT
                     id,
                     text,
                     summary,
                     keywords,
                     entities,
                     revision,
                     CASE WHEN id NOT IN (SELECT id FROM memories_vec) THEN 1 ELSE 0 END,
                     CASE WHEN trim(summary) = '' THEN 1 ELSE 0 END,
                     CASE WHEN trim(keywords) IN ('', '[]') THEN 1 ELSE 0 END,
                     COALESCE(json_extract(metadata, '$.enrichment.failed_stage'), ''),
                     COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.attempts') AS INTEGER), 0),
                     COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.max_attempts') AS INTEGER), ?1)
                   FROM memories
                   WHERE json_extract(metadata, '$.enrichment.status') = 'failed'
                     AND (
                       COALESCE(json_extract(metadata, '$.enrichment.retry.kind'), '') = 'auth'
                       OR lower(COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')) LIKE '%auth_failed%'
                       OR lower(COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')) LIKE '%401%'
                       OR lower(COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')) LIKE '%403%'
                       OR lower(COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')) LIKE '%unauthorized%'
                       OR lower(COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')) LIKE '%invalid api key%'
                       OR lower(COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')) LIKE '%unusable%'
                     )
                     AND COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.attempts') AS INTEGER), 0)
                         < COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.max_attempts') AS INTEGER), ?1)
                     AND COALESCE(json_extract(metadata, '$.enrichment.retry.next_retry_at'), '') <= ?2
                   ORDER BY rowid
                   LIMIT ?3"#,
            )?;
            let rows = stmt.query_map(params![max_attempts, &now, limit], |row| {
                Ok(EnrichmentRetryCandidate {
                    id: row.get(0)?,
                    text: row.get(1)?,
                    summary: row.get(2)?,
                    keywords: parse_json_string_list(row.get::<_, String>(3)?),
                    entities: parse_json_string_list(row.get::<_, String>(4)?),
                    revision: row.get(5)?,
                    needs_embedding: row.get::<_, i64>(6)? != 0,
                    needs_summary: row.get::<_, i64>(7)? != 0,
                    needs_metadata: row.get::<_, i64>(8)? != 0,
                    failed_stage: row.get::<_, String>(9)?,
                    attempts: row.get(10)?,
                    max_attempts: row.get(11)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        for row in &rows {
            db::refuse_retired_sticky_row_within_tx(&tx, &row.id, "claimed for enrichment retry")?;
        }

        for row in &rows {
            let next_attempt = row.attempts + 1;
            let next_retry_at = retry_backoff_until(next_attempt);
            tx.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.enrichment.retry.kind', 'auth',
                         '$.enrichment.retry.attempts', ?1,
                         '$.enrichment.retry.max_attempts', ?2,
                         '$.enrichment.retry.last_retry_at', ?3,
                         '$.enrichment.retry.next_retry_at', ?4
                       )
                   WHERE id = ?5
                     AND json_extract(metadata, '$.enrichment.status') = 'failed'"#,
                params![
                    next_attempt,
                    row.max_attempts,
                    &now,
                    &next_retry_at,
                    &row.id
                ],
            )?;
        }

        tx.commit()?;
        Ok(rows)
    }

    /// List entries that are missing summaries.
    /// Returns (id, text, revision) tuples.
    pub fn entries_missing_summaries(&self) -> Result<Vec<(String, String, i64)>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, revision FROM memories
             WHERE trim(summary) = ''
             ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Fetch summary-backfill candidate rows for an exact, operator-supplied
    /// ID set in THIS database only — no cross-DB discovery.
    ///
    /// Rows come back in the caller's `ids` order (the caller is responsible
    /// for deduplicating that slice deterministically first). Ids absent from
    /// this database are surfaced by the returned Vec being shorter than
    /// `ids`, so the caller fails the whole request before any provider call
    /// or write instead of silently ignoring a typo'd id.
    pub fn summary_backfill_rows_for_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<SummaryBackfillRow>, MemoryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, text, trim(summary) != '', archived, revision
             FROM memories
             WHERE id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                Ok(SummaryBackfillRow {
                    id: row.get(0)?,
                    text: row.get(1)?,
                    has_summary: row.get::<_, i64>(2)? != 0,
                    archived: row.get::<_, i64>(3)? != 0,
                    revision: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let by_id = rows
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect::<std::collections::HashMap<String, SummaryBackfillRow>>();
        Ok(ids.iter().filter_map(|id| by_id.get(id).cloned()).collect())
    }

    /// List entries missing recall keywords.
    ///
    /// Entities are optional: some short notes and diagnostic rows have no
    /// meaningful named entity, and repeatedly retrying them only creates false
    /// enrichment gaps.
    /// Returns (id, text, summary, revision) tuples.
    pub fn entries_missing_metadata(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, summary, revision FROM memories
             WHERE trim(keywords) IN ('', '[]')
             ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count entries with recall keywords.
    ///
    /// Entities are optional for generic memory rows; keyword coverage is the
    /// hard completeness signal used by backfill and runtime enrichment.
    pub fn metadata_stats(&self) -> Result<(i64, i64), MemoryError> {
        let (total, missing): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE
                        WHEN trim(keywords) IN ('', '[]')
                        THEN 1 ELSE 0 END), 0)
             FROM memories",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((total, total - missing))
    }

    /// Get total memory count and FTS index count.
    pub fn fts_stats(&self) -> Result<(i64, i64), MemoryError> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        let with_fts: i64 =
            self.conn
                .query_row("SELECT COUNT(DISTINCT id) FROM memories_fts", [], |r| {
                    r.get(0)
                })?;
        Ok((total, with_fts))
    }

    /// Backfill FTS index for entries missing from memories_fts.
    /// Returns the number of rows inserted.
    ///
    /// NULL ids (`memories.id` is `TEXT PRIMARY KEY` without NOT NULL; FTS5
    /// columns are untyped), same guards as the open-time backfill (#1985):
    /// - `WHERE id IS NOT NULL` in the subquery: one NULL-id projection row
    ///   would make `id NOT IN (..., NULL)` NULL for every memory and
    ///   silently suppress the whole repair.
    /// - `id IS NOT NULL` on `memories`: a NULL-id memory can never be matched
    ///   back to an FTS hit, and projecting it would create exactly such a
    ///   poisoning row (`NULL NOT IN (<empty>)` is TRUE).
    pub fn backfill_fts_missing(&mut self) -> Result<usize, MemoryError> {
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories
               WHERE id IS NOT NULL
                 AND id NOT IN (SELECT id FROM memories_fts WHERE id IS NOT NULL)"#,
            [],
        )?;
        let symbolic_inserted = tx.execute(
            r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
               SELECT id, path, summary, text, keywords, entities, topic
               FROM memories
               WHERE id IS NOT NULL
                 AND id NOT IN (SELECT id FROM memories_symbolic_fts WHERE id IS NOT NULL)"#,
            [],
        )?;
        if inserted + symbolic_inserted > 0 {
            crate::db::bump_search_generation(&tx)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Full FTS rebuild. Use this when the FTS table is stale or corrupted.
    pub fn rebuild_fts_full(&mut self) -> Result<usize, MemoryError> {
        let tx = self.conn.transaction()?;
        tx.execute_batch("DROP TABLE IF EXISTS memories_fts;")?;
        tx.execute_batch(
            r#"CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                   id UNINDEXED,
                   path,
                   summary,
                   text,
                   keywords,
                   entities,
                   tokenize = 'simple'
               );"#,
        )?;
        let inserted = tx.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories"#,
            [],
        )?;
        tx.execute_batch("DROP TABLE IF EXISTS memories_symbolic_fts;")?;
        tx.execute_batch(
            r#"CREATE VIRTUAL TABLE IF NOT EXISTS memories_symbolic_fts USING fts5(
                   id,
                   path,
                   summary,
                   text,
                   keywords,
                   entities,
                   topic,
                   tokenize = 'trigram case_sensitive 0'
               );"#,
        )?;
        let _ = tx.execute(
            r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
               SELECT id, path, summary, text, keywords, entities, topic
               FROM memories"#,
            [],
        )?;
        crate::db::bump_search_generation(&tx)?;
        tx.commit()?;
        Ok(inserted)
    }

    /// Get total memory count and vector count.
    pub fn vector_stats(&self) -> Result<(i64, i64), MemoryError> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        let with_vec: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT v.id)
                     FROM memories_vec v
                     JOIN memories m ON m.id = v.id",
            [],
            |r| r.get(0),
        )?;
        Ok((total, with_vec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    fn test_entry(id: &str) -> crate::MemoryEntry {
        crate::MemoryEntry {
            id: id.to_string(),
            path: "/test/enrichment".to_string(),
            summary: "enrichment authorization fixture".to_string(),
            text: "enrichment authorization fixture text".to_string(),
            importance: 0.7,
            timestamp: "2026-07-26T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "enrichment".to_string(),
            keywords: vec!["seed-keyword".to_string()],
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

    fn assert_sqlite_authorization_denied(result: Result<usize, rusqlite::Error>, context: &str) {
        assert!(
            matches!(
                &result,
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ffi::ErrorCode::AuthorizationForStatementDenied
                        && error.extended_code == rusqlite::ffi::SQLITE_AUTH
            ),
            "{context}: expected SQLITE_AUTH, got {result:?}"
        );
    }

    #[test]
    fn enrichment_update_is_authorized_without_leaking_raw_metadata_write_access() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let entry = test_entry("enrichment-authorization");
        store.upsert(&entry).expect("seed entry");

        let raw_before = store.connection().execute(
            "UPDATE memories SET metadata = ?1 WHERE id = ?2",
            params![r#"{"raw_before":true}"#, &entry.id],
        );
        assert_sqlite_authorization_denied(
            raw_before,
            "raw connection must not mutate protected metadata before typed enrichment",
        );

        let keywords = vec!["enriched-keyword".to_string()];
        assert!(
            store
                .update_enrichment_fields(
                    &entry.id,
                    None,
                    None,
                    Some(&keywords),
                    None,
                    entry.revision
                )
                .expect("typed enrichment update"),
            "matching revision must accept the typed enrichment write"
        );

        let stored = store
            .get(&entry.id)
            .expect("load enriched entry")
            .expect("enriched entry exists");
        assert_eq!(stored.keywords, keywords);

        let raw_after = store.connection().execute(
            "UPDATE memories SET metadata = ?1 WHERE id = ?2",
            params![r#"{"raw_after":true}"#, &entry.id],
        );
        assert_sqlite_authorization_denied(
            raw_after,
            "typed enrichment authorization must end before raw metadata writes resume",
        );
    }

    #[test]
    fn enrichment_update_rejects_stale_revision_without_mutation() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let entry = test_entry("enrichment-stale-revision");
        store.upsert(&entry).expect("seed entry");

        let stale_keywords = vec!["stale-keyword".to_string()];
        assert!(
            !store
                .update_enrichment_fields(
                    &entry.id,
                    None,
                    None,
                    Some(&stale_keywords),
                    None,
                    entry.revision + 1,
                )
                .expect("stale typed enrichment update"),
            "stale revision must be rejected"
        );

        let stored = store
            .get(&entry.id)
            .expect("load entry after stale update")
            .expect("entry exists after stale update");
        assert_eq!(stored.keywords, entry.keywords);
        assert_eq!(stored.revision, entry.revision);
    }

    /// Exact-id summary selection: caller order is preserved, summary and
    /// archival status are surfaced (not hidden), and unknown ids are
    /// reported by length mismatch so the caller can fail the whole set.
    #[test]
    fn summary_backfill_rows_for_ids_reports_exact_rows_in_caller_order() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut missing = test_entry("missing-summary");
        missing.summary = String::new();
        store.upsert(&missing).expect("seed missing-summary row");

        let mut summarized = test_entry("has-summary");
        summarized.summary = "existing summary".to_string();
        store.upsert(&summarized).expect("seed summarized row");

        let mut archived = test_entry("archived-row");
        archived.archived = true;
        store.upsert(&archived).expect("seed archived row");

        let rows = store
            .summary_backfill_rows_for_ids(&[
                "archived-row".to_string(),
                "unknown-id".to_string(),
                "missing-summary".to_string(),
                "has-summary".to_string(),
            ])
            .expect("select exact-id candidates");

        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["archived-row", "missing-summary", "has-summary"],
            "rows keep the caller's deterministic order; the unknown id is simply absent"
        );
        assert_eq!(
            rows.len(),
            3,
            "unknown ids surface as a length mismatch, never a silent ignore"
        );
        assert!(rows[0].archived);
        assert!(rows[0].has_summary);
        assert!(!rows[1].archived);
        assert!(!rows[1].has_summary);
        assert!(rows[2].has_summary);
        assert_eq!(rows[1].text, "enrichment authorization fixture text");
        assert_eq!(rows[1].revision, 1);

        assert!(
            store
                .summary_backfill_rows_for_ids(&[])
                .expect("empty id set")
                .is_empty(),
            "an empty explicit selection stays empty instead of sweeping everything"
        );
    }

    /// #2 (Sol rereview): a failure observed at a stale revision must not
    /// stamp the concurrently-moved row's metadata; only the matching
    /// revision is stamped, and observation fields never change.
    #[test]
    fn record_enrichment_failure_if_revision_guards_against_concurrent_revisions() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let entry = test_entry("guarded-failure");
        store.upsert(&entry).expect("seed entry"); // revision 1
                                                   // Baseline from the stored row: write-time normalization may rewrite
                                                   // the constructed entry's timestamp formatting.
        let seeded = store.get("guarded-failure").expect("read").expect("exists");

        assert!(
            store
                .record_enrichment_failure_if_revision(
                    "guarded-failure",
                    "summary",
                    "provider 503",
                    1
                )
                .expect("record at the matching revision"),
            "a matching revision must stamp the failure"
        );
        let stamped = store.get("guarded-failure").expect("read").expect("exists");
        assert_eq!(
            stamped
                .metadata
                .pointer("/enrichment/failed_stage")
                .and_then(|value| value.as_str()),
            Some("summary")
        );
        assert_eq!(
            stamped.timestamp, seeded.timestamp,
            "observation timestamp preserved"
        );
        assert_eq!(
            stamped.valid_from, seeded.valid_from,
            "valid_from preserved"
        );
        assert_eq!(stamped.text, seeded.text, "raw text preserved");

        // A concurrent writer bumps the revision; the guarded stamp refuses.
        let mut moved = stamped.clone();
        moved.importance = 0.9;
        store.upsert(&moved).expect("concurrent revision bump"); // revision 2
        let after_bump = store.get("guarded-failure").expect("read").expect("exists");
        let updated_at_after_bump: String = store
            .connection()
            .query_row(
                "SELECT updated_at FROM memories WHERE id = 'guarded-failure'",
                [],
                |row| row.get(0),
            )
            .expect("read updated_at after bump");

        assert!(
            !store
                .record_enrichment_failure_if_revision(
                    "guarded-failure",
                    "summary",
                    "stale observation error",
                    1
                )
                .expect("guarded record must answer, not fail"),
            "a stale revision must not be stamped"
        );
        let untouched = store.get("guarded-failure").expect("read").expect("exists");
        assert_eq!(
            untouched.metadata, after_bump.metadata,
            "the moved revision's metadata stays unpolluted"
        );
        let updated_at_after_refusal: String = store
            .connection()
            .query_row(
                "SELECT updated_at FROM memories WHERE id = 'guarded-failure'",
                [],
                |row| row.get(0),
            )
            .expect("read updated_at after refused stamp");
        assert_eq!(
            updated_at_after_refusal, updated_at_after_bump,
            "the moved revision's updated_at stays untouched by the refused stamp"
        );

        assert!(
            store
                .record_enrichment_failure_if_revision(
                    "guarded-failure",
                    "summary",
                    "fresh observation error",
                    2
                )
                .expect("record at the new matching revision"),
            "the new current revision can be stamped"
        );
        let restamped = store.get("guarded-failure").expect("read").expect("exists");
        assert_eq!(
            restamped
                .metadata
                .pointer("/enrichment/last_error")
                .and_then(|value| value.as_str()),
            Some("fresh observation error")
        );
    }

    fn count(store: &MemoryStore, sql: &str) -> i64 {
        store
            .connection()
            .query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// Seeds two memories (`upsert` projects both into both FTS tables), then
    /// in `table` only drops `fts-null-guard-b`'s row and adds one NULL-id
    /// row. FTS5 columns are untyped, so such a row is representable.
    fn seed_null_id_fts_row_and_missing_row(table: &str) -> MemoryStore {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        for id in ["fts-null-guard-a", "fts-null-guard-b"] {
            store.upsert(&test_entry(id)).expect("seed entry");
        }
        let conn = store.connection();
        conn.execute(
            &format!("DELETE FROM {table} WHERE id = 'fts-null-guard-b'"),
            [],
        )
        .expect("drop one real projection row");
        let insert_null = if table == "memories_symbolic_fts" {
            "INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
             VALUES (NULL, '/null', 'null', 'null', '', '', '')"
        } else {
            "INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
             VALUES (NULL, '/null', 'null', 'null', '', '')"
        };
        conn.execute(insert_null, [])
            .expect("insert NULL-id FTS row");
        assert_eq!(
            count(
                &store,
                &format!("SELECT COUNT(*) FROM {table} WHERE id IS NULL")
            ),
            1,
            "{table}: fixture must hold exactly one NULL-id row"
        );
        store
    }

    fn assert_repaired_exactly_once(store: &MemoryStore, table: &str) {
        assert_eq!(
            count(
                store,
                &format!("SELECT COUNT(*) FROM {table} WHERE id = 'fts-null-guard-b'")
            ),
            1,
            "{table}: the missing real row must be reinserted despite the NULL-id row"
        );
        assert_eq!(
            count(
                store,
                &format!("SELECT COUNT(*) FROM {table} WHERE id IS NOT NULL")
            ),
            2,
            "{table}: real rows repaired exactly once, no duplicates"
        );
        assert_eq!(
            count(
                store,
                &format!("SELECT COUNT(*) FROM {table} WHERE id IS NULL")
            ),
            1,
            "{table}: backfill is insert-missing only; it neither adds nor prunes NULL-id rows"
        );
    }

    /// Under SQL three-valued logic `x NOT IN (..., NULL)` is NULL for every
    /// `x`, so one NULL-id `memories_fts` row used to suppress every repair.
    #[test]
    fn backfill_fts_missing_repairs_memories_fts_despite_null_id_row() {
        let mut store = seed_null_id_fts_row_and_missing_row("memories_fts");

        let inserted = store.backfill_fts_missing().expect("backfill");

        assert_eq!(inserted, 1, "memories_fts: one missing row reinserted");
        assert_repaired_exactly_once(&store, "memories_fts");
        assert_eq!(
            store.backfill_fts_missing().expect("second backfill"),
            0,
            "a second pass finds nothing missing"
        );
        assert_repaired_exactly_once(&store, "memories_fts");
    }

    /// Same NULL poisoning for the symbolic trigram projection. The return
    /// value counts only `memories_fts`, so assert on table state.
    #[test]
    fn backfill_fts_missing_repairs_symbolic_fts_despite_null_id_row() {
        let mut store = seed_null_id_fts_row_and_missing_row("memories_symbolic_fts");

        store.backfill_fts_missing().expect("backfill");

        assert_repaired_exactly_once(&store, "memories_symbolic_fts");
        store.backfill_fts_missing().expect("second backfill");
        assert_repaired_exactly_once(&store, "memories_symbolic_fts");
    }

    /// A NULL `memories.id` can never be matched back to an FTS hit by id, so
    /// backfill must not project it. Projecting it would create exactly the
    /// NULL-id FTS row that poisons later `NOT IN` repairs. With an empty
    /// projection `NULL NOT IN (<empty>)` is TRUE, so only an explicit
    /// `id IS NOT NULL` guard on `memories` keeps it out.
    #[test]
    fn backfill_fts_missing_never_projects_null_memory_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("null-memory-id.db");
        let path = path.to_str().expect("utf8 db path");
        {
            let mut store = MemoryStore::open(path).expect("create store");
            store
                .upsert(&test_entry("fts-null-guard-real"))
                .expect("seed entry");
        }
        {
            // The store connection's authorizer denies raw `memories` writes,
            // so a legacy NULL-id row is seeded through a plain connection.
            let _ = libsimple::enable_auto_extension();
            crate::db::register_sqlite_vec();
            let raw = rusqlite::Connection::open(path).expect("open raw connection");
            // Guard triggers call this per-connection function; report
            // "not enabled" so they keep enforcing on this plain row.
            raw.create_scalar_function(
                "tachi_reserved_reference_write_enabled",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| Ok(0_i64),
            )
            .expect("register guard function");
            raw.execute(
                "INSERT INTO memories (id, path, summary, text, timestamp, valid_from)
                 VALUES (NULL, '/null', 'null id', 'null id text',
                         '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z')",
                [],
            )
            .expect("insert NULL-id memory");
        }
        let mut store = MemoryStore::open(path).expect("reopen store");
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM memories WHERE id IS NULL"),
            1,
            "fixture must hold exactly one NULL-id memory"
        );
        // Start from empty projections; raw FTS deletes are permitted.
        let conn = store.connection();
        conn.execute("DELETE FROM memories_fts", [])
            .expect("empty memories_fts");
        conn.execute("DELETE FROM memories_symbolic_fts", [])
            .expect("empty memories_symbolic_fts");

        let inserted = store.backfill_fts_missing().expect("backfill");

        assert_eq!(inserted, 1, "only the real memory is projected");
        for table in ["memories_fts", "memories_symbolic_fts"] {
            assert_eq!(
                count(
                    &store,
                    &format!("SELECT COUNT(*) FROM {table} WHERE id IS NULL")
                ),
                0,
                "{table}: a NULL memory id must not be projected"
            );
            assert_eq!(
                count(
                    &store,
                    &format!("SELECT COUNT(*) FROM {table} WHERE id = 'fts-null-guard-real'")
                ),
                1,
                "{table}: the real memory is projected"
            );
        }
    }
}
