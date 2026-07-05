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
    if auth_class_enrichment_error(error) {
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
