use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::types::MemorySource;

use super::now_utc_iso;

const ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS: i64 = 3;

fn auth_class_enrichment_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("auth_failed")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("unusable")
}

/// Atomically try to claim an event for processing.
/// Uses INSERT OR IGNORE: if the row didn't exist, it's inserted and we return true (claimed).
/// If the row already existed, nothing happens and we return false (already processed).
/// This fixes the TOCTOU race in the old check-then-mark pattern.
pub fn try_claim_event(
    conn: &Connection,
    event_hash: &str,
    event_id: &str,
    worker: &str,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let rows_changed = conn.execute(
        "INSERT OR IGNORE INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![event_hash, event_id, worker, now],
    )?;
    Ok(rows_changed > 0)
}

/// Release a previously claimed event so it can be retried.
/// Called when background processing fails to ensure at-least-once delivery.
pub fn release_event_claim(
    conn: &Connection,
    event_hash: &str,
    worker: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "DELETE FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
        params![event_hash, worker],
    )?;
    Ok(())
}

/// Update a memory row only when its revision matches `expected_revision`.
/// Returns `Ok(true)` when updated, `Ok(false)` on revision mismatch.
#[allow(clippy::too_many_arguments)]
pub fn update_with_revision(
    conn: &mut Connection,
    id: &str,
    new_text: &str,
    new_summary: &str,
    new_source: &str,
    new_metadata: &str,
    new_vec: Option<&[u8]>,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let new_revision = expected_revision + 1;
    // Normalize source to satisfy CHECK constraint.
    let new_source = MemorySource::parse_or_external(new_source);
    let tx = conn.transaction()?;

    let clean_text = crate::noise::scrub_think_tags(new_text);
    let clean_summary = crate::noise::scrub_think_tags(new_summary);

    tx.execute(
        "UPDATE memories
         SET text = ?1, summary = ?2, source = ?3, metadata = ?4, updated_at = ?5, revision = ?6
         WHERE id = ?7 AND revision = ?8",
        params![
            &clean_text,
            &clean_summary,
            new_source,
            new_metadata,
            &now,
            new_revision,
            id,
            expected_revision
        ],
    )?;
    let updated = tx.changes() > 0;

    if updated {
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        tx.execute(
            r#"INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
               SELECT
                 id,
                 path,
                 summary,
                 text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories WHERE id = ?1"#,
            params![id],
        )?;

        if let Some(vec_blob) = new_vec {
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![id])?;
            tx.execute(
                "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
                params![id, vec_blob],
            )?;
        }
    }

    tx.commit()?;
    Ok(updated)
}

/// Update only the enrichment fields (summary, embedding, keywords, entities) if
/// the revision hasn't changed since the enrichment was queued. This prevents
/// stale background enrichment from overwriting concurrent updates.
pub fn update_enrichment_fields(
    conn: &mut Connection,
    id: &str,
    new_summary: Option<&str>,
    new_vec: Option<&[u8]>,
    new_keywords: Option<&[String]>,
    new_entities: Option<&[String]>,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    if new_summary.is_none()
        && new_vec.is_none()
        && new_keywords.is_none()
        && new_entities.is_none()
    {
        return Ok(true); // nothing to do
    }

    let now = now_utc_iso();
    let tx = conn.transaction()?;
    let clean_summary = new_summary.map(crate::noise::scrub_think_tags);
    let keywords_json = new_keywords.map(serde_json::to_string).transpose()?;
    let entities_json = new_entities.map(serde_json::to_string).transpose()?;

    // Always check revision first, regardless of which fields are being updated.
    // This prevents stale enrichment from overwriting concurrent edits.
    let rows_affected = match (&clean_summary, &keywords_json, &entities_json) {
        (Some(summary), Some(keywords), Some(entities)) => tx.execute(
            "UPDATE memories SET summary = ?1, keywords = ?2, entities = ?3, updated_at = ?4 WHERE id = ?5 AND revision = ?6",
            params![summary, keywords, entities, &now, id, expected_revision],
        )?,
        (Some(summary), Some(keywords), None) => tx.execute(
            "UPDATE memories SET summary = ?1, keywords = ?2, updated_at = ?3 WHERE id = ?4 AND revision = ?5",
            params![summary, keywords, &now, id, expected_revision],
        )?,
        (Some(summary), None, Some(entities)) => tx.execute(
            "UPDATE memories SET summary = ?1, entities = ?2, updated_at = ?3 WHERE id = ?4 AND revision = ?5",
            params![summary, entities, &now, id, expected_revision],
        )?,
        (Some(summary), None, None) => tx.execute(
            "UPDATE memories SET summary = ?1, updated_at = ?2 WHERE id = ?3 AND revision = ?4",
            params![summary, &now, id, expected_revision],
        )?,
        (None, Some(keywords), Some(entities)) => tx.execute(
            "UPDATE memories SET keywords = ?1, entities = ?2, updated_at = ?3 WHERE id = ?4 AND revision = ?5",
            params![keywords, entities, &now, id, expected_revision],
        )?,
        (None, Some(keywords), None) => tx.execute(
            "UPDATE memories SET keywords = ?1, updated_at = ?2 WHERE id = ?3 AND revision = ?4",
            params![keywords, &now, id, expected_revision],
        )?,
        (None, None, Some(entities)) => tx.execute(
            "UPDATE memories SET entities = ?1, updated_at = ?2 WHERE id = ?3 AND revision = ?4",
            params![entities, &now, id, expected_revision],
        )?,
        (None, None, None) => tx.execute(
            "UPDATE memories SET updated_at = ?1 WHERE id = ?2 AND revision = ?3",
            params![&now, id, expected_revision],
        )?,
    };

    if rows_affected == 0 {
        tx.commit()?;
        eprintln!(
            "[enrichment] discarded stale enrichment for id={id}: revision {expected_revision} no longer current"
        );
        return Ok(false);
    }

    // Refresh FTS when any searchable text field changed.
    //
    // FTS safety (#943): keywords/entities land in `memories` via parameterized
    // UPDATE binds above; this INSERT copies column content with `WHERE id = ?1`.
    // Keyword text becomes document content (not a MATCH expression), so FTS
    // operators inside keywords are not injection vectors. User queries still
    // go through `simple_query` / `simple_query_input` on the search path.
    if new_summary.is_some() || new_keywords.is_some() || new_entities.is_some() {
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        tx.execute(
            r#"INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
               SELECT
                 id,
                 path,
                 summary,
                 text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories WHERE id = ?1"#,
            params![id],
        )?;
    }

    if let Some(vec_blob) = new_vec {
        tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![id])?;
        tx.execute(
            "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
            params![id, vec_blob],
        )?;
    }

    let status = match (
        new_vec.is_some(),
        new_summary.is_some(),
        new_keywords.is_some() || new_entities.is_some(),
    ) {
        (true, true, true) => "embedded+summarized+metadata",
        (true, true, false) => "embedded+summarized",
        (true, false, true) => "embedded+metadata",
        (true, false, false) => "embedded",
        (false, true, true) => "summarized+metadata",
        (false, true, false) => "summarized",
        (false, false, true) => "metadata",
        (false, false, false) => "touched",
    };

    // Mixed-stage merge (#943): if another enrichment stage already recorded a
    // durable failure that this write does NOT resolve, preserve aggregate
    // failure metadata (failed_stage / last_error / last_failure_at / retry)
    // and only stamp last_success_at for the stages that succeeded. Clearing
    // then re-applying keywords_status alone used to drop operator-visible
    // failure context on partial success.
    let existing_failed_stage: Option<String> = tx
        .query_row(
            r#"SELECT json_extract(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.failed_stage'
               )
               FROM memories WHERE id = ?1"#,
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or(None);
    let failed_stage_resolved = match existing_failed_stage.as_deref() {
        None => true,
        Some("embedding") => new_vec.is_some(),
        Some("summary") => new_summary.is_some(),
        // Both metadata extraction and write-side keyword enrichment land in
        // the keywords/entities columns.
        Some("metadata") | Some("keywords") => {
            new_keywords.is_some() || new_entities.is_some()
        }
        // Unknown / db_update: do not claim resolution from a field write.
        Some(_) => false,
    };

    if failed_stage_resolved {
        tx.execute(
            r#"UPDATE memories
               SET metadata = json_remove(
                     json_set(
                       CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                       '$.enrichment.status', ?1,
                       '$.enrichment.last_success_at', ?2,
                       '$.enrichment.last_error', NULL
                     ),
                     '$.enrichment.failed_stage',
                     '$.enrichment.last_failure_at',
                     '$.enrichment.retry'
                   )
               WHERE id = ?3"#,
            params![status, &now, id],
        )?;
    } else {
        // Preserve failure aggregate; still record that some stages succeeded.
        tx.execute(
            r#"UPDATE memories
               SET metadata = json_set(
                     CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                     '$.enrichment.last_success_at', ?1,
                     '$.enrichment.partial_success_status', ?2
                   )
               WHERE id = ?3"#,
            params![&now, status, id],
        )?;
    }

    tx.commit()?;
    Ok(true)
}

pub fn record_enrichment_failure(
    conn: &Connection,
    id: &str,
    stage: &str,
    error: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    // Write-side keyword enrichment (#921) keeps a dedicated keywords_status so
    // operators can distinguish enriched/pending/skipped/failed without
    // collapsing it into the multi-stage overall enrichment.status string.
    let keywords_status = if stage == "keywords" {
        Some("failed")
    } else {
        None
    };
    if auth_class_enrichment_error(error) {
        if let Some(kw_status) = keywords_status {
            conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.enrichment.status', 'failed',
                         '$.enrichment.failed_stage', ?1,
                         '$.enrichment.last_error', ?2,
                         '$.enrichment.last_failure_at', ?3,
                         '$.enrichment.keywords_status', ?4,
                         '$.enrichment.retry.kind', 'auth',
                         '$.enrichment.retry.attempts',
                            COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.attempts') AS INTEGER), 0),
                         '$.enrichment.retry.max_attempts', ?5,
                         '$.enrichment.retry.next_retry_at', ?3
                       ),
                       updated_at = ?3
                   WHERE id = ?6"#,
                params![
                    stage,
                    error,
                    &now,
                    kw_status,
                    ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS,
                    id
                ],
            )?;
        } else {
            conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.enrichment.status', 'failed',
                         '$.enrichment.failed_stage', ?1,
                         '$.enrichment.last_error', ?2,
                         '$.enrichment.last_failure_at', ?3,
                         '$.enrichment.retry.kind', 'auth',
                         '$.enrichment.retry.attempts',
                            COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.attempts') AS INTEGER), 0),
                         '$.enrichment.retry.max_attempts', ?4,
                         '$.enrichment.retry.next_retry_at', ?3
                       ),
                       updated_at = ?3
                   WHERE id = ?5"#,
                params![stage, error, &now, ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS, id],
            )?;
        }
    } else if let Some(kw_status) = keywords_status {
        conn.execute(
            r#"UPDATE memories
               SET metadata = json_remove(
                     json_set(
                       CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                       '$.enrichment.status', 'failed',
                       '$.enrichment.failed_stage', ?1,
                       '$.enrichment.last_error', ?2,
                       '$.enrichment.last_failure_at', ?3,
                       '$.enrichment.keywords_status', ?4
                     ),
                     '$.enrichment.retry'
                   ),
                   updated_at = ?3
               WHERE id = ?5"#,
            params![stage, error, &now, kw_status, id],
        )?;
    } else {
        conn.execute(
            r#"UPDATE memories
               SET metadata = json_remove(
                     json_set(
                       CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                       '$.enrichment.status', 'failed',
                       '$.enrichment.failed_stage', ?1,
                       '$.enrichment.last_error', ?2,
                       '$.enrichment.last_failure_at', ?3
                     ),
                     '$.enrichment.retry'
                   ),
                   updated_at = ?3
               WHERE id = ?4"#,
            params![stage, error, &now, id],
        )?;
    }
    Ok(())
}

/// Set operator-visible write-side keyword enrichment status (#921).
///
/// Values: `enriched` | `pending` | `skipped` | `failed`.
pub fn set_keyword_enrichment_status(
    conn: &Connection,
    id: &str,
    status: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        r#"UPDATE memories
           SET metadata = json_set(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status', ?1
               ),
               updated_at = ?2
           WHERE id = ?3"#,
        params![status, &now, id],
    )?;
    Ok(())
}

/// Stamp `keywords_status=pending` only when absent or already pending.
///
/// Returns `true` when a row was updated. Terminal statuses
/// (`enriched` / `skipped` / `failed`) are never overwritten — this closes the
/// race where a late pending write lands after a fast worker already finished
/// (#943).
pub fn set_keyword_enrichment_pending_if_unset(
    conn: &Connection,
    id: &str,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let rows = conn.execute(
        r#"UPDATE memories
           SET metadata = json_set(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status', 'pending'
               ),
               updated_at = ?1
           WHERE id = ?2
             AND (
               json_extract(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status'
               ) IS NULL
               OR json_extract(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status'
               ) = 'pending'
             )"#,
        params![&now, id],
    )?;
    Ok(rows > 0)
}
