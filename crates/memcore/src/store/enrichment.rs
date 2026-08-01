//! Enrichment, FTS, and vector maintenance methods on [`MemoryStore`].

use crate::{db, error::MemoryError, types::ExpectedMemoryState, MemoryStore};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use rusqlite::params;

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
        let tx = self.conn.transaction()?;

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
    pub fn backfill_fts_missing(&mut self) -> Result<usize, MemoryError> {
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories
               WHERE id NOT IN (SELECT id FROM memories_fts)"#,
            [],
        )?;
        let symbolic_inserted = tx.execute(
            r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
               SELECT id, path, summary, text, keywords, entities, topic
               FROM memories
               WHERE id NOT IN (SELECT id FROM memories_symbolic_fts)"#,
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
}
