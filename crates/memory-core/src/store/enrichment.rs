//! Enrichment, FTS, and vector maintenance methods on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

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

        db::retry_memory_locked(|| {
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
        db::retry_memory_locked(|| {
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

    /// Like [`entries_missing_vectors`], but can exclude a source and cap row count.
    pub fn entries_missing_vectors_filtered(
        &self,
        exclude_source: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<(String, String, String, i64)>, MemoryError> {
        let limit_val = limit.map(|l| l as i64).unwrap_or(-1);
        let mut out = Vec::new();
        match exclude_source {
            Some(source) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, text, summary, revision FROM memories
                     WHERE id NOT IN (SELECT id FROM memories_vec)
                     AND source != ?1
                     ORDER BY rowid
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![source, limit_val], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    out.push(row?);
                }
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, text, summary, revision FROM memories
                     WHERE id NOT IN (SELECT id FROM memories_vec)
                     ORDER BY rowid
                     LIMIT ?1",
                )?;
                let rows = stmt.query_map(rusqlite::params![limit_val], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    out.push(row?);
                }
            }
        }
        Ok(out)
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
