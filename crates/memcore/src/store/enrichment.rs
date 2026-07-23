//! Enrichment, FTS, and vector maintenance methods on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use rusqlite::params;

pub const ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS: i64 = 3;

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
        let metadata_json = serde_json::to_string(new_metadata)?;
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };

        let db_label = self.db_label.clone();
        db::retry_memory_locked("update_with_revision", &db_label, || {
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
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };
        let db_label = self.db_label.clone();
        db::retry_memory_locked("update_enrichment_fields", &db_label, || {
            db::update_enrichment_fields(
                &mut self.conn,
                id,
                new_summary,
                vec_blob.as_deref(),
                new_keywords,
                new_entities,
                expected_revision,
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
        db::record_enrichment_failure(&self.conn, id, stage, error)
    }

    /// Set write-side keyword enrichment status (`enriched`/`pending`/`skipped`/`failed`).
    pub fn set_keyword_enrichment_status(&self, id: &str, status: &str) -> Result<(), MemoryError> {
        db::set_keyword_enrichment_status(&self.conn, id, status)
    }

    /// Stamp `keywords_status=pending` only when current status is absent or already
    /// pending — never overwrite a terminal status (`enriched`/`skipped`/`failed`).
    pub fn set_keyword_enrichment_pending_if_unset(&self, id: &str) -> Result<bool, MemoryError> {
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
        let inserted = self.conn.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories
               WHERE id NOT IN (SELECT id FROM memories_fts)"#,
            [],
        )?;
        let _ = self.conn.execute(
            r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
               SELECT id, path, summary, text, keywords, entities, topic
               FROM memories
               WHERE id NOT IN (SELECT id FROM memories_symbolic_fts)"#,
            [],
        )?;
        Ok(inserted)
    }

    /// Full FTS rebuild. Use this when the FTS table is stale or corrupted.
    pub fn rebuild_fts_full(&mut self) -> Result<usize, MemoryError> {
        self.conn
            .execute_batch("DROP TABLE IF EXISTS memories_fts;")?;
        self.conn.execute_batch(
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
        let inserted = self.conn.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories"#,
            [],
        )?;
        self.conn
            .execute_batch("DROP TABLE IF EXISTS memories_symbolic_fts;")?;
        self.conn.execute_batch(
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
        let _ = self.conn.execute(
            r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
               SELECT id, path, summary, text, keywords, entities, topic
               FROM memories"#,
            [],
        )?;
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
