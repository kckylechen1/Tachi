use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::types::{default_retention_for, MemoryCategory, MemoryEntry, MemoryScope, MemorySource};

use super::common::{normalize_utc_iso, now_utc_iso, row_to_entry};
use super::sqlite_vec::serialize_f32;

// ─── Normalization ────────────────────────────────────────────────────────────

/// Coerce caller-provided enum-like fields to the canonical vocabulary
/// enforced by the CHECK constraints on `memories`. Idempotent.
///
/// - `source`: routed through [`MemorySource::parse_or_external`]; user-controlled
///   non-canonical values become `external:<sanitized>`.
/// - `category`: clamped to one of fact/decision/experience/preference/entity/other.
/// - `scope`: clamped to user/project/general (rejects `self`, `other_agent:*`).
/// - `retention_policy`: defaulted via [`default_retention_for`] if the caller
///   left it `None`.
pub fn normalize_for_write(entry: &mut MemoryEntry) {
    entry.path = crate::path_router::normalize_path(&entry.path);
    entry.source = MemorySource::parse_or_external(&entry.source);
    entry.category = MemoryCategory::normalize(&entry.category).to_string();
    entry.scope = MemoryScope::normalize(&entry.scope).to_string();
    entry.importance = entry.importance.clamp(0.0, 1.0);
    if entry.retention_policy.is_none() {
        if let Some(d) = default_retention_for(&entry.path, &entry.source) {
            entry.retention_policy = Some(d.to_string());
        }
    }
}

// ─── UPSERT ───────────────────────────────────────────────────────────────────

/// Insert or update a memory entry (and its embedding vector if provided).
pub fn upsert(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
) -> Result<(), MemoryError> {
    if entry.id.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "entry.id must be provided by caller".to_string(),
        ));
    }

    // Normalize only the fields enforced by CHECK constraints; avoid cloning
    // the full entry/vector on the hot write path.
    let path = crate::path_router::normalize_path(&entry.path);
    let source = MemorySource::parse_or_external(&entry.source);
    let category = MemoryCategory::normalize(&entry.category);
    let scope = MemoryScope::normalize(&entry.scope);
    let retention_policy = entry
        .retention_policy
        .clone()
        .or_else(|| default_retention_for(&path, &source).map(str::to_string));

    let timestamp_utc = normalize_utc_iso(&entry.timestamp)?;
    let last_access_utc = entry
        .last_access
        .as_deref()
        .map(normalize_utc_iso)
        .transpose()?;
    let write_time_utc = now_utc_iso();

    let metadata_json = serde_json::to_string(&entry.metadata)?;
    let kws_json = serde_json::to_string(&entry.keywords)?;
    let p_json = serde_json::to_string(&entry.persons)?;
    let e_json = serde_json::to_string(&entry.entities)?;

    // All writes for one upsert must be atomic across main table + FTS + vec.
    let tx = conn.transaction()?;

    // Write to main table
    tx.execute(
        r#"INSERT INTO memories
              (id, path, summary, text, importance,
               timestamp, category, topic, keywords, persons, entities,
               location, source, scope, archived, created_at, updated_at,
               access_count, last_access, revision, metadata,
               retention_policy, domain)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)
           ON CONFLICT(id) DO UPDATE SET
               path         = excluded.path,
               summary      = excluded.summary,
               text         = excluded.text,
               importance   = excluded.importance,
               timestamp    = excluded.timestamp,
               category     = excluded.category,
               topic        = excluded.topic,
               keywords     = excluded.keywords,
               persons      = excluded.persons,
               entities     = excluded.entities,
               location     = excluded.location,
               source       = excluded.source,
               scope        = excluded.scope,
               archived     = excluded.archived,
               created_at   = memories.created_at,
               updated_at   = excluded.updated_at,
               access_count = excluded.access_count,
               last_access  = excluded.last_access,
               revision     = memories.revision + 1,
               metadata     = excluded.metadata,
               retention_policy = excluded.retention_policy,
               domain       = excluded.domain"#,
        params![
            entry.id,
            &path,
            entry.summary,
            entry.text,
            entry.importance,
            timestamp_utc,
            category,
            entry.topic,
            kws_json,
            p_json,
            e_json,
            entry.location,
            &source,
            scope,
            entry.archived,
            &write_time_utc,
            &write_time_utc,
            entry.access_count,
            last_access_utc,
            entry.revision.max(1),
            metadata_json,
            &retention_policy,
            entry.domain,
        ],
    )?;

    // Sync FTS: delete old row (if any) then re-insert
    let kws = entry.keywords.join(" ");
    let ents = entry.entities.join(" ");
    tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![entry.id])?;
    tx.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![entry.id, &path, entry.summary, entry.text, kws, ents],
    )?;

    if let Some(vec) = &entry.vector {
        if vec_available {
            let blob = serialize_f32(vec);
            // vec0 virtual tables do NOT support ON CONFLICT / UPSERT.
            // Use DELETE + INSERT (same pattern as FTS sync above).
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![entry.id])?;
            tx.execute(
                "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
                params![entry.id, blob],
            )?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// Check if an event has already been processed by a specific worker.
pub fn is_event_processed(
    conn: &Connection,
    event_hash: &str,
    worker: &str,
) -> Result<bool, MemoryError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
        params![event_hash, worker],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Mark an event as processed by a specific worker.
pub fn mark_event_processed(
    conn: &Connection,
    event_hash: &str,
    event_id: &str,
    worker: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT OR IGNORE INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![event_hash, event_id, worker, now],
    )?;
    Ok(())
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

    tx.execute(
        "UPDATE memories
         SET text = ?1, summary = ?2, source = ?3, metadata = ?4, updated_at = ?5, revision = ?6
         WHERE id = ?7 AND revision = ?8",
        params![
            new_text,
            new_summary,
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

/// Update only the enrichment fields (summary + embedding) if the revision
/// hasn't changed since the enrichment was queued. This prevents stale
/// background enrichment from overwriting concurrent updates.
pub fn update_enrichment_fields(
    conn: &mut Connection,
    id: &str,
    new_summary: Option<&str>,
    new_vec: Option<&[u8]>,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    if new_summary.is_none() && new_vec.is_none() {
        return Ok(true); // nothing to do
    }

    let now = now_utc_iso();
    let tx = conn.transaction()?;

    // Always check revision first, regardless of which fields are being updated.
    // This prevents stale enrichment from overwriting concurrent edits.
    let rows_affected = if let Some(summary) = new_summary {
        tx.execute(
            "UPDATE memories SET summary = ?1, updated_at = ?2 WHERE id = ?3 AND revision = ?4",
            params![summary, &now, id, expected_revision],
        )?
    } else {
        // No summary to update — still verify revision by touching updated_at
        tx.execute(
            "UPDATE memories SET updated_at = ?1 WHERE id = ?2 AND revision = ?3",
            params![&now, id, expected_revision],
        )?
    };

    if rows_affected == 0 {
        tx.commit()?;
        return Ok(false); // revision mismatch — entry was updated concurrently, discard enrichment
    }

    // Refresh FTS if summary was updated
    if new_summary.is_some() {
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

    tx.commit()?;
    Ok(true)
}

// ─── VECTOR SEARCH ────────────────────────────────────────────────────────────

/// KNN vector search via sqlite-vec.
/// Returns (doc_id → cosine_distance) for the top `top_k` results.
/// Cosine *distance* is in [0, 2]; we convert to similarity [0, 1]:
///   similarity = 1 − distance/2
pub fn search_vec(
    conn: &Connection,
    query_vec: &[f32],
    top_k: usize,
    include_archived: bool,
    path_prefix: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let blob = serialize_f32(query_vec);
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let mut stmt = conn.prepare(
        r#"SELECT v.id, v.distance
           FROM memories_vec v
           JOIN memories m ON m.id = v.id
           WHERE v.embedding MATCH ?1
             AND k = ?3
             AND (?2 = 1 OR m.archived = 0)
             AND (?4 IS NULL OR m.path LIKE ?4)
           ORDER BY v.distance"#,
    )?;

    let rows = stmt.query_map(
        params![blob, include_archived as i64, top_k as i64, path_like],
        |row| {
            let id: String = row.get(0)?;
            let dist: f64 = row.get(1)?;
            Ok((id, dist))
        },
    )?;

    let mut scores = HashMap::new();
    for r in rows {
        let (id, dist) = r?;
        // sqlite-vec returns L2 / cosine distance depending on vec0 config;
        // treat as cosine distance ∈ [0, 2] → similarity ∈ [0, 1]
        let sim = (1.0 - dist / 2.0).clamp(0.0, 1.0);
        scores.insert(id, sim);
    }
    Ok(scores)
}

// ─── FTS SEARCH ───────────────────────────────────────────────────────────────

/// Full-text search using the FTS5 virtual table.
/// Returns (doc_id → normalised BM25 score [0, 1]).
pub fn search_fts(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    path_prefix: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    // Sanitise query: remove potentially dangerous characters
    let safe_query: String = query
        .chars()
        .filter(|c| {
            c.is_alphanumeric() || c.is_whitespace() || matches!(c, '"' | '\'' | '-' | '_' | '.')
        })
        .collect();

    if safe_query.trim().is_empty() {
        return Ok(HashMap::new());
    }

    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    // Use simple_query() for automatic CJK segmentation in MATCH clause
    let mut stmt = conn.prepare(
        r#"SELECT memories_fts.id, -bm25(memories_fts) AS score
           FROM memories_fts
           JOIN memories m ON m.id = memories_fts.id
           WHERE memories_fts MATCH simple_query(?1)
             AND (?2 = 1 OR m.archived = 0)
             AND (?4 IS NULL OR m.path LIKE ?4)
           ORDER BY bm25(memories_fts)
           LIMIT ?3"#,
    )?;

    let rows = stmt.query_map(
        params![safe_query, include_archived as i64, limit as i64, path_like],
        |row| {
            let id: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            Ok((id, score))
        },
    )?;

    let mut raw: Vec<(String, f64)> = rows.filter_map(|r| r.ok()).collect();
    if raw.is_empty() {
        return Ok(HashMap::new());
    }

    // Normalise BM25 scores to [0, 1] based on the max in this result set
    let max_score = raw
        .iter()
        .map(|(_, s)| *s)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_score = if max_score <= 0.0 { 1.0 } else { max_score };

    Ok(raw
        .drain(..)
        .map(|(id, s)| (id, (s / max_score).clamp(0.0, 1.0)))
        .collect())
}

// ─── BULK FETCH ───────────────────────────────────────────────────────────────

/// Maximum IDs per batch for IN clause queries (SQLite has a 999 parameter limit).
const IN_BATCH_SIZE: usize = 900;

/// Fetch multiple entries by their IDs in one query.
/// Also hydrates vectors from memories_vec if available.
/// Handles batching internally to stay under SQLite's 999 parameter limit.
pub fn fetch_by_ids(
    conn: &Connection,
    ids: &[String],
    include_archived: bool,
) -> Result<HashMap<String, MemoryEntry>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut out = HashMap::new();

    for batch in ids.chunks(IN_BATCH_SIZE) {
        let placeholders = batch
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let mut sql = format!(
            "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
             FROM memories WHERE id IN ({})",
            placeholders
        );
        if !include_archived {
            sql.push_str(" AND archived = 0");
        }

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), row_to_entry)?;

        for r in rows {
            let entry = r?;
            out.insert(entry.id.clone(), entry);
        }

        // Hydrate vectors from memories_vec (best-effort: table may not exist)
        let vec_sql = format!(
            "SELECT id, embedding FROM memories_vec WHERE id IN ({})",
            placeholders
        );
        if let Ok(mut vec_stmt) = conn.prepare(&vec_sql) {
            if let Ok(vec_rows) = vec_stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
                let id: String = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            }) {
                for r in vec_rows.flatten() {
                    let (id, blob) = r;
                    if let Some(entry) = out.get_mut(&id) {
                        if blob.len() % 4 == 0 {
                            let vec: Vec<f32> = blob
                                .chunks_exact(4)
                                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                                .collect();
                            entry.vector = Some(vec);
                        }
                    }
                }
            }
        }
    }

    Ok(out)
}

/// Fetch the most recent entries, up to `limit`. Returns sorted dynamically by inserted time.
pub fn get_all(
    conn: &Connection,
    limit: usize,
    include_archived: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let sql = if include_archived {
        "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
         FROM memories ORDER BY timestamp DESC LIMIT ?"
    } else {
        "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
         FROM memories WHERE archived = 0 ORDER BY timestamp DESC LIMIT ?"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], row_to_entry)?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch entries under a path prefix using SQL pushdown instead of full-table scans.
pub fn list_by_path(
    conn: &Connection,
    path_prefix: &str,
    limit: usize,
    include_archived: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let mut normalized = path_prefix.trim().to_string();
    if normalized.is_empty() {
        normalized = "/".to_string();
    }
    if !normalized.starts_with('/') {
        normalized = format!("/{normalized}");
    }
    if normalized.len() > 1 {
        normalized = normalized.trim_end_matches('/').to_string();
    }
    let like_prefix = if normalized == "/" {
        "/%".to_string()
    } else {
        format!("{normalized}/%")
    };

    let sql = if include_archived {
        "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
         FROM memories
         WHERE path = ?1 OR path LIKE ?2
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
    } else {
        "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
         FROM memories
         WHERE (path = ?1 OR path LIKE ?2) AND archived = 0
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![normalized, like_prefix, limit], row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Bump access_count and last_access for a list of IDs (called after every search).
/// Also records access timestamps for ACT-R base-level activation.
pub fn record_access(conn: &Connection, ids: &[String]) -> Result<(), MemoryError> {
    if ids.is_empty() {
        return Ok(());
    }

    let now = now_utc_iso();
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute(
            "UPDATE memories
             SET access_count = access_count + 1, last_access = ?1, updated_at = ?1
             WHERE id = ?2",
            params![&now, id],
        )?;
        // Also record timestamp for ACT-R base-level activation
        tx.execute(
            "INSERT INTO access_history (memory_id, accessed_at) VALUES (?1, ?2)",
            params![id, &now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Fetch access timestamps for a set of memory IDs (for ACT-R base-level activation).
/// Returns a map from memory_id -> sorted list of seconds-since-epoch (age in seconds).
/// Handles batching internally to stay under SQLite's 999 parameter limit.
pub fn get_access_times(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, Vec<f64>>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let now = Utc::now();
    let mut result: HashMap<String, Vec<f64>> = HashMap::new();

    for batch in ids.chunks(IN_BATCH_SIZE) {
        let placeholders: Vec<String> = batch
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT memory_id, accessed_at FROM access_history WHERE memory_id IN ({}) ORDER BY accessed_at DESC",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params_vec.as_slice(), |row| {
            let mem_id: String = row.get(0)?;
            let at: String = row.get(1)?;
            Ok((mem_id, at))
        })?;

        for row in rows {
            let (mem_id, at_str) = row?;
            if let Ok(dt) = at_str.parse::<DateTime<Utc>>() {
                let age_secs = (now - dt).num_seconds().max(1) as f64;
                result.entry(mem_id).or_default().push(age_secs);
            }
        }
    }

    Ok(result)
}

// ─── DELETE ───────────────────────────────────────────────────────────────────

/// Delete a memory entry by ID from main table, FTS index, and vector table.
/// Returns true if an entry was found and deleted.
pub fn delete(conn: &mut Connection, id: &str, vec_available: bool) -> Result<bool, MemoryError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }

    let tx = conn.transaction()?;

    // Delete from main table and check if anything was actually removed
    tx.execute("DELETE FROM memories WHERE id = ?1", params![trimmed])?;
    let deleted = tx.changes() > 0;

    if deleted {
        // Clean up FTS index
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![trimmed])?;

        // Clean up vector table (best-effort)
        if vec_available {
            let _ = tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![trimmed]);
        }

        // Clean up graph edges
        tx.execute(
            "DELETE FROM memory_edges WHERE source_id = ?1 OR target_id = ?1",
            params![trimmed],
        )?;

        // Clean up access history (CASCADE)
        tx.execute(
            "DELETE FROM access_history WHERE memory_id = ?1",
            params![trimmed],
        )?;

        // Clean up agent known state (CASCADE)
        tx.execute(
            "DELETE FROM agent_known_state WHERE memory_id = ?1",
            params![trimmed],
        )?;
    }

    tx.commit()?;
    Ok(deleted)
}

pub fn archive_memory(conn: &Connection, id: &str) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "UPDATE memories SET archived = 1, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 0",
        params![now, id],
    )?;
    Ok(conn.changes() > 0)
}
