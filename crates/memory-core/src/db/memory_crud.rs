use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::types::{default_retention_for, MemoryCategory, MemoryEntry, MemoryScope, MemorySource};

use super::common::{normalize_utc_iso, now_utc_iso, row_to_entry};
use super::sqlite_vec::serialize_f32;

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// FNV-1a 32-bit hash of a query string for query_diversity tracking.
fn fnv1a_hash(s: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for byte in s.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("{hash:08x}")
}

/// Token-based Jaccard similarity between two texts.
/// Uses the same tokeniser as [`crate::scorer::tokenize`].
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    use std::collections::HashSet;
    let ta: HashSet<String> = crate::scorer::tokenize(a).into_iter().collect();
    let tb: HashSet<String> = crate::scorer::tokenize(b).into_iter().collect();
    if ta.is_empty() && tb.is_empty() {
        return 0.0;
    }
    let intersection = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

const MEMORY_SELECT_COLUMNS: &str = "id,path,summary,text,importance,timestamp,valid_from,valid_until,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain,recall_count,query_diversity,tier";

fn sync_memories_fts(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
    tx.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, path, summary, text, keywords_joined, entities_joined],
    )?;
    Ok(())
}

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
    entry.fold_persons_into_entities();
    entry.fold_location_into_metadata();
}

/// Serialize `entities` with legacy `persons` folded in.
fn canonical_entities_json(entry: &MemoryEntry) -> Result<String, MemoryError> {
    let mut entities = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut entities, entry.persons.clone());
    Ok(serde_json::to_string(&entities)?)
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

    let clean_text = crate::noise::scrub_think_tags(&entry.text);
    let clean_summary = crate::noise::scrub_think_tags(&entry.summary);
    let force = entry
        .metadata
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let importance = crate::types::normalize_importance(entry.importance, category, force);

    let timestamp_utc = normalize_utc_iso(&entry.timestamp)?;
    let valid_from_utc = if entry.valid_from.trim().is_empty() {
        timestamp_utc.clone()
    } else {
        normalize_utc_iso(&entry.valid_from)?
    };
    let valid_until_utc = entry
        .valid_until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_utc_iso)
        .transpose()?;
    let last_access_utc = entry
        .last_access
        .as_deref()
        .map(normalize_utc_iso)
        .transpose()?;
    let write_time_utc = now_utc_iso();

    let mut metadata = entry.metadata.clone();
    let path = crate::types::apply_location_relocation(&path, &entry.location, &mut metadata);
    let metadata_json = serde_json::to_string(&metadata)?;
    let kws_json = serde_json::to_string(&entry.keywords)?;
    let e_json = canonical_entities_json(entry)?;

    // All writes for one upsert must be atomic across main table + FTS + vec.
    let tx = conn.transaction()?;

    // ── Write-time Jaccard deduplication (new entries only) ──────────────────
    // Only for net-new IDs; ON CONFLICT path below handles updates.
    let is_new: bool = tx
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = ?1",
            params![entry.id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 0;

    if is_new {
        // Run FTS search for potential overlapping entries
        let safe_query: String = entry
            .text
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" ");
        if !safe_query.is_empty() {
            let fts_candidates: Vec<(String, String)> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT m.id, m.text FROM memories_fts
                     JOIN memories m ON m.id = memories_fts.id
                     WHERE memories_fts MATCH simple_query(?1)
                       AND m.archived = 0 AND m.superseded_by IS NULL
                     LIMIT 5",
                    )
                    .ok();
                if let Some(ref mut s) = stmt {
                    s.query_map(params![safe_query], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default()
                } else {
                    vec![]
                }
            };
            for (cand_id, cand_text) in fts_candidates {
                if jaccard_similarity(&entry.text, &cand_text) > 0.9 {
                    // Merge: update candidate with max importance and merged tags
                    let merge_kws = {
                        let cand_kws_json: String = tx
                            .query_row(
                                "SELECT keywords FROM memories WHERE id = ?1",
                                params![cand_id],
                                |r| r.get(0),
                            )
                            .unwrap_or_else(|_| "[]".to_string());
                        let mut kws: Vec<String> =
                            serde_json::from_str(&cand_kws_json).unwrap_or_default();
                        for k in &entry.keywords {
                            if !kws.contains(k) {
                                kws.push(k.clone());
                            }
                        }
                        serde_json::to_string(&kws).unwrap_or_else(|_| "[]".to_string())
                    };
                    let merge_ents = {
                        let cand_ents_json: String = tx
                            .query_row(
                                "SELECT entities FROM memories WHERE id = ?1",
                                params![cand_id],
                                |r| r.get(0),
                            )
                            .unwrap_or_else(|_| "[]".to_string());
                        let mut ents: Vec<String> =
                            serde_json::from_str(&cand_ents_json).unwrap_or_default();
                        for e in &entry.entities {
                            if !ents.contains(e) {
                                ents.push(e.clone());
                            }
                        }
                        crate::types::fold_person_names_into_entities(
                            &mut ents,
                            entry.persons.clone(),
                        );
                        serde_json::to_string(&ents).unwrap_or_else(|_| "[]".to_string())
                    };
                    tx.execute(
                        "UPDATE memories SET keywords = ?1, entities = ?2,
                         importance = MAX(importance, ?3), updated_at = ?4
                         WHERE id = ?5",
                        params![merge_kws, merge_ents, importance, &write_time_utc, cand_id],
                    )?;
                    let (cand_path, cand_summary, cand_text) = tx.query_row(
                        "SELECT path, summary, text FROM memories WHERE id = ?1",
                        params![cand_id],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        },
                    )?;
                    let kws_joined: String = serde_json::from_str::<Vec<String>>(&merge_kws)
                        .unwrap_or_default()
                        .join(" ");
                    let ents_joined: String = serde_json::from_str::<Vec<String>>(&merge_ents)
                        .unwrap_or_default()
                        .join(" ");
                    sync_memories_fts(
                        &tx,
                        &cand_id,
                        &cand_path,
                        &cand_summary,
                        &cand_text,
                        &kws_joined,
                        &ents_joined,
                    )?;
                    // Write this entry as superseded by the candidate
                    tx.execute(
                        r#"INSERT INTO memories
                              (id, path, summary, text, importance,
                               timestamp, valid_from, valid_until, category, topic, keywords, entities,
                               source, scope, archived, created_at, updated_at,
                               access_count, last_access, revision, metadata,
                               retention_policy, domain, recall_count, query_diversity, tier,
                               superseded_by)
                           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
                           ON CONFLICT(id) DO NOTHING"#,
                        params![
                            entry.id, &path, &clean_summary, &clean_text, importance,
                            timestamp_utc, valid_from_utc, valid_until_utc, category, entry.topic,
                            kws_json, e_json, &source, scope,
                            entry.archived, &write_time_utc, &write_time_utc,
                            entry.access_count, last_access_utc, entry.revision.max(1),
                            metadata_json, &retention_policy, entry.domain,
                            0i64, 0i64, "raw",
                            cand_id,
                        ],
                    )?;
                    tx.commit()?;
                    return Ok(());
                }
            }
        }
    }

    // Write to main table
    tx.execute(
        r#"INSERT INTO memories
              (id, path, summary, text, importance,
               timestamp, valid_from, valid_until, category, topic, keywords, entities,
               source, scope, archived, created_at, updated_at,
               access_count, last_access, revision, metadata,
               retention_policy, domain, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26)
           ON CONFLICT(id) DO UPDATE SET
               path         = excluded.path,
               summary      = excluded.summary,
               text         = excluded.text,
               importance   = excluded.importance,
               timestamp    = excluded.timestamp,
               valid_from   = excluded.valid_from,
               valid_until  = excluded.valid_until,
               category     = excluded.category,
               topic        = excluded.topic,
               keywords     = excluded.keywords,
               entities     = excluded.entities,
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
               domain       = excluded.domain,
               tier         = CASE WHEN memories.tier IN ('consolidated','pattern') THEN memories.tier ELSE excluded.tier END"#,
        params![
            entry.id,
            &path,
            &clean_summary,
            &clean_text,
            importance,
            timestamp_utc,
            valid_from_utc,
            valid_until_utc,
            category,
            entry.topic,
            kws_json,
            e_json,
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
            entry.recall_count,
            entry.query_diversity,
            &entry.tier,
        ],
    )?;

    let kws = entry.keywords.join(" ");
    let mut ents_vec = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut ents_vec, entry.persons.clone());
    let ents = ents_vec.join(" ");
    sync_memories_fts(
        &tx,
        &entry.id,
        &path,
        &clean_summary,
        &clean_text,
        &kws,
        &ents,
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
                 '$.enrichment.last_failure_at'
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
    conn.execute(
        r#"UPDATE memories
           SET metadata = json_set(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.status', 'failed',
                 '$.enrichment.failed_stage', ?1,
                 '$.enrichment.last_error', ?2,
                 '$.enrichment.last_failure_at', ?3
               ),
               updated_at = ?3
           WHERE id = ?4"#,
        params![stage, error, &now, id],
    )?;
    Ok(())
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
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let blob = serialize_f32(query_vec);
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let mut stmt = conn.prepare(
        r#"SELECT v.id, v.distance
           FROM memories_vec v
           JOIN memories m ON m.id = v.id
           WHERE v.embedding MATCH ?1
              AND k = ?3
              AND (?2 = 1 OR m.archived = 0)
               AND (?4 = 1 OR m.superseded_by IS NULL)
               AND (?5 IS NULL OR m.path LIKE ?5)
               AND (?6 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?6 AND (m.valid_until IS NULL OR m.valid_until > ?6)))
             ORDER BY v.distance"#,
    )?;

    let rows = stmt.query_map(
        params![
            blob,
            include_archived as i64,
            top_k as i64,
            include_superseded as i64,
            path_like,
            as_of_utc.as_deref()
        ],
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
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
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
              AND (?4 = 1 OR m.superseded_by IS NULL)
              AND (?5 IS NULL OR m.path LIKE ?5)
              AND (?6 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?6 AND (m.valid_until IS NULL OR m.valid_until > ?6)))
             ORDER BY bm25(memories_fts)
            LIMIT ?3"#,
    )?;

    let rows = stmt.query_map(
        params![
            safe_query,
            include_archived as i64,
            limit as i64,
            include_superseded as i64,
            path_like,
            as_of_utc.as_deref()
        ],
        |row| {
            let id: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            Ok((id, score))
        },
    )?;

    let mut raw: Vec<(String, f64)> = rows.collect::<Result<_, _>>()?;
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

// ─── SYMBOLIC CANDIDATE SEARCH ───────────────────────────────────────────────

/// Pull a small lexical candidate set for exact IDs, path slugs, keywords, and
/// short technical terms that FTS tokenization may miss.
pub fn search_symbolic_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let mut terms: Vec<String> = crate::scorer::tokenize(query)
        .into_iter()
        .filter(|term| term.len() >= 3)
        .collect();
    terms.extend(
        query
            .split_whitespace()
            .map(|term| {
                term.trim_matches(|c: char| {
                    !c.is_alphanumeric() && !matches!(c, '-' | '_' | '/' | '.')
                })
                .to_ascii_lowercase()
            })
            .filter(|term| term.len() >= 3),
    );
    terms.sort();
    terms.dedup();
    terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    terms.truncate(12);

    if terms.is_empty() && path_prefix.is_none() {
        return Ok(Vec::new());
    }

    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let mut sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS} FROM memories
         WHERE (?1 = 1 OR archived = 0)
           AND (?2 = 1 OR superseded_by IS NULL)
           AND (?3 IS NULL OR path LIKE ?3)
           AND (?4 IS NULL OR (COALESCE(NULLIF(valid_from, ''), timestamp) <= ?4 AND (valid_until IS NULL OR valid_until > ?4)))"
    );

    let mut params: Vec<rusqlite::types::Value> = vec![
        (include_archived as i64).into(),
        (include_superseded as i64).into(),
        path_like.into(),
        as_of_utc.clone().into(),
    ];

    if !terms.is_empty() {
        let mut term_clauses = Vec::new();
        for term in &terms {
            let pattern = format!("%{}%", escape_like_pattern(term));
            params.push(pattern.into());
            let idx = params.len();
            term_clauses.push(format!(
                "(id LIKE ?{idx} ESCAPE '\\'
                  OR path LIKE ?{idx} ESCAPE '\\'
                  OR summary LIKE ?{idx} ESCAPE '\\'
                  OR text LIKE ?{idx} ESCAPE '\\'
                  OR keywords LIKE ?{idx} ESCAPE '\\'
                  OR entities LIKE ?{idx} ESCAPE '\\'
                  OR topic LIKE ?{idx} ESCAPE '\\')"
            ));
        }
        sql.push_str(" AND (");
        sql.push_str(&term_clauses.join(" OR "));
        sql.push(')');
    }

    params.push((limit.max(1) as i64).into());
    let limit_idx = params.len();
    sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT ?{limit_idx}"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
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
            "SELECT {MEMORY_SELECT_COLUMNS} FROM memories WHERE id IN ({})",
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
            if let Ok(vec_rows) =
                vec_stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
                    let id: String = row.get(0)?;
                    let blob: Vec<u8> = row.get(1)?;
                    Ok((id, blob))
                })
            {
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
        format!("SELECT {MEMORY_SELECT_COLUMNS} FROM memories ORDER BY timestamp DESC LIMIT ?")
    } else {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS} FROM memories WHERE archived = 0 ORDER BY timestamp DESC LIMIT ?"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
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
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE path = ?1 OR path LIKE ?2
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
        )
    } else {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE (path = ?1 OR path LIKE ?2) AND archived = 0
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![normalized, like_prefix, limit], row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Find the canonical active wiki row for a path/topic pair.
pub fn find_active_wiki_entry_by_path_or_topic(
    conn: &Connection,
    path: &str,
    topic: &str,
) -> Result<Option<MemoryEntry>, MemoryError> {
    let sql = format!(
        r#"SELECT {MEMORY_SELECT_COLUMNS}
           FROM memories
           WHERE archived = 0
             AND superseded_by IS NULL
             AND (path = ?1 OR (topic = ?2 AND path LIKE '/wiki/%'))
           ORDER BY CASE WHEN path = ?1 THEN 0 ELSE 1 END, timestamp DESC
           LIMIT 1"#
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![path, topic], row_to_entry)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// Bump access_count and last_access for a list of IDs (called after every search).
/// `fts_hits` are the IDs matched by the FTS channel (get recall_count incremented).
/// `query` is the raw query string; its FNV-1a hash is stored in access_history and
/// used to compute `query_diversity` (distinct queries that reached this memory).
/// Applies a promotion gate: tier → "consolidated" when recall_count ≥ 3,
/// query_diversity ≥ 3, and (importance ≥ 0.8 OR query_diversity ≥ 3).
pub fn record_access(
    conn: &Connection,
    ids: &[String],
    fts_hits: &[String],
    query: Option<&str>,
) -> Result<(), MemoryError> {
    if ids.is_empty() {
        return Ok(());
    }

    let now = now_utc_iso();
    let query_hash = query.map(fnv1a_hash).unwrap_or_default();
    let tx = conn.unchecked_transaction()?;

    // Build a set of FTS hit IDs for O(1) lookup
    let fts_set: std::collections::HashSet<&str> = fts_hits.iter().map(String::as_str).collect();

    for id in ids {
        // NOTE: do NOT bump `updated_at` or `revision` here — see original comment.
        tx.execute(
            "UPDATE memories
             SET access_count = access_count + 1, last_access = ?1
             WHERE id = ?2",
            params![&now, id],
        )?;
        // Record access timestamp + query hash for ACT-R / diversity tracking
        tx.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params![id, &now, &query_hash],
        )?;

        // Increment recall_count for FTS hits (exact term retrieval signal)
        if fts_set.contains(id.as_str()) {
            tx.execute(
                "UPDATE memories SET recall_count = recall_count + 1 WHERE id = ?1",
                params![id],
            )?;
        }

        // Increment diversity only when this access introduces a new query hash.
        if !query_hash.is_empty() {
            let hash_count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM access_history
                 WHERE memory_id = ?1 AND query_hash = ?2",
                    params![id, &query_hash],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if hash_count == 1 {
                tx.execute(
                    "UPDATE memories SET query_diversity = query_diversity + 1 WHERE id = ?1",
                    params![id],
                )?;
            }
        }

        // Promotion gate: raw → consolidated when recall_count ≥ 3 AND query_diversity ≥ 3
        tx.execute(
            "UPDATE memories SET tier = 'consolidated'
             WHERE id = ?1
               AND tier = 'raw'
               AND recall_count >= 3
               AND query_diversity >= 3",
            params![id],
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

/// Mark a memory as superseded by a newer/canonical memory. Superseded rows are
/// hidden from default search but remain available for audit/history.
pub fn supersede_memory(
    conn: &Connection,
    id: &str,
    superseded_by: &str,
) -> Result<bool, MemoryError> {
    if id == superseded_by {
        return Ok(false);
    }
    let now = now_utc_iso();
    conn.execute(
        "UPDATE memories
         SET superseded_by = ?1, updated_at = ?2, revision = revision + 1
         WHERE id = ?3 AND (superseded_by IS NULL OR superseded_by != ?1)",
        params![superseded_by, now, id],
    )?;
    Ok(conn.changes() > 0)
}
