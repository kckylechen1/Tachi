use rusqlite::{params, Connection, OptionalExtension};
use std::sync::{Mutex, MutexGuard};

use crate::error::MemoryError;
use crate::types::{default_retention_for, MemoryCategory, MemoryEntry, MemoryScope, MemorySource};

use super::common::{normalize_utc_iso, now_utc_iso, row_to_entry};
use super::sqlite_vec::serialize_f32;

mod access;
mod read;
mod search;
mod update;

pub use access::get_access_times;
pub(crate) use access::record_access_with_updates;
#[cfg(test)]
pub(crate) use access::{record_access, AccessUpdate};
pub use read::{
    fetch_by_ids, find_active_wiki_entry_by_path_or_topic, find_exact_path_text_id, get_all,
    list_by_path, list_by_path_recent, list_wiki_duplicate_candidates,
};
pub(crate) use search::search_fts_raw_match;
pub use search::{search_fts, search_symbolic_candidates, search_vec};
pub use update::{
    record_enrichment_failure, release_event_claim, set_keyword_enrichment_pending_if_unset,
    set_keyword_enrichment_status, try_claim_event, update_enrichment_fields, update_with_revision,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

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

fn simple_query_input(query: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;

    for ch in query.chars() {
        if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        } else if ch.is_whitespace() || !out.is_empty() {
            pending_space = true;
        }
    }

    out
}

pub(crate) const MEMORY_SELECT_COLUMNS: &str = "id,path,summary,text,importance,timestamp,valid_from,valid_until,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain,recall_count,query_diversity,tier";
const MEMORY_SELECT_COLUMNS_QUALIFIED: &str = "m.id,m.path,m.summary,m.text,m.importance,m.timestamp,m.valid_from,m.valid_until,m.category,m.topic,m.keywords,'[]' AS persons,m.entities,'' AS location,m.source,m.scope,m.archived,m.access_count,m.last_access,m.revision,m.metadata,m.retention_policy,m.domain,m.recall_count,m.query_diversity,m.tier";

static FTS_SYNC_LOCK: Mutex<()> = Mutex::new(());

fn acquire_fts_sync_lock() -> MutexGuard<'static, ()> {
    FTS_SYNC_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sync_memories_fts(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    // The wangfenjin/simple tokenizer has process-global Pinyin state. Keep
    // in-process FTS syncs single-file so parallel test/server writes do not
    // stampede that tokenizer while preserving the same SQL effects.
    let _fts_guard = acquire_fts_sync_lock();
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

/// Outcome of an id-less write protected by the modern identity constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdlessUpsertResult {
    Saved,
    Duplicate { id: String },
}

/// Insert or update a memory entry (and its embedding vector if provided).
pub fn upsert(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
) -> Result<(), MemoryError> {
    upsert_with_idless_identity(conn, entry, vec_available, None).map(|_| ())
}

/// Insert an id-less entry once. A unique modern identity chooses one winner
/// without rewriting legacy rows that predate the constraint.
pub fn upsert_idless(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    identity: &str,
) -> Result<IdlessUpsertResult, MemoryError> {
    let identity = identity.trim();
    if identity.is_empty() {
        return Err(MemoryError::InvalidArg(
            "id-less identity must be non-empty".to_string(),
        ));
    }
    upsert_with_idless_identity(conn, entry, vec_available, Some(identity))
}

fn upsert_with_idless_identity(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
) -> Result<IdlessUpsertResult, MemoryError> {
    if entry.id.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "entry.id must be provided by caller".to_string(),
        ));
    }
    // tachi#773 item 4 guard (c): the `anchor:` id namespace is reserved for
    // `ensure_anchor` (memcore::db::anchor). Ordinary upserts must never
    // create or silently overwrite an anchor row — `ensure_anchor` uses its
    // own dedicated INSERT OR IGNORE, not this function, so any caller
    // reaching here with an `anchor:`-prefixed id is a bug (or a hostile
    // write), not a legitimate anchor creation.
    if entry.id.starts_with("anchor:") {
        return Err(MemoryError::InvalidArg(format!(
            "id '{}' is in the reserved 'anchor:' namespace; use ensure_anchor, not upsert",
            entry.id
        )));
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
    let is_new: bool = tx.query_row(
        "SELECT COUNT(*) FROM memories WHERE id = ?1",
        params![entry.id],
        |r| r.get::<_, i64>(0),
    )? == 0;

    if is_new && idless_identity.is_none() {
        // Run FTS search for potential overlapping entries
        let safe_query = simple_query_input(
            &entry
                .text
                .split_whitespace()
                .take(12)
                .collect::<Vec<_>>()
                .join(" "),
        );
        if !safe_query.is_empty() {
            let fts_candidates: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT m.id, m.text FROM memories_fts
                     JOIN memories m ON m.id = memories_fts.id
                     WHERE memories_fts MATCH simple_query(?1)
                       AND m.archived = 0 AND m.superseded_by IS NULL
                     LIMIT 5",
                )?;
                let rows = stmt.query_map(params![safe_query], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                rows.collect::<Result<_, _>>()?
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
                    return Ok(IdlessUpsertResult::Saved);
                }
            }
        }
    }

    // Write to main table
    let rows_written = tx.execute(
        r#"INSERT INTO memories
              (id, path, summary, text, importance,
               timestamp, valid_from, valid_until, category, topic, keywords, entities,
               source, scope, archived, created_at, updated_at,
               access_count, last_access, revision, metadata,
               retention_policy, domain, idless_identity, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
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
               idless_identity = excluded.idless_identity,
               tier         = CASE WHEN memories.tier IN ('consolidated','pattern') THEN memories.tier ELSE excluded.tier END
           ON CONFLICT(idless_identity)
             WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL
             DO NOTHING"#,
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
            idless_identity,
            entry.recall_count,
            entry.query_diversity,
            &entry.tier,
        ],
    )?;

    if rows_written == 0 {
        let identity = idless_identity.ok_or_else(|| {
            MemoryError::Internal("ordinary upsert unexpectedly wrote zero rows".to_string())
        })?;
        let winner_id = tx
            .query_row(
                "SELECT id FROM memories
                 WHERE idless_identity = ?1 AND archived = 0 AND superseded_by IS NULL",
                params![identity],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                MemoryError::Internal(format!(
                    "id-less identity conflict without an active winner: {identity}"
                ))
            })?;
        tx.commit()?;
        return Ok(IdlessUpsertResult::Duplicate { id: winner_id });
    }

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
    Ok(IdlessUpsertResult::Saved)
}

#[cfg(test)]
mod idless_upsert_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/notes/atomic-idless".to_string(),
            summary: "atomic id-less save".to_string(),
            text: "identical concurrent id-less save payload".to_string(),
            importance: 0.7,
            timestamp: "2026-07-16T00:00:00Z".to_string(),
            valid_from: "2026-07-16T00:00:00Z".to_string(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "mcp".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn two_concurrent_same_identity_idless_saves_choose_one_winner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("memory.db")
            .to_string_lossy()
            .to_string();
        {
            let store = crate::MemoryStore::open(&path).unwrap();
            let has_identity_column: bool = store
                .connection()
                .query_row(
                    "SELECT EXISTS (\
                         SELECT 1 FROM pragma_table_info('memories') \
                         WHERE name = 'idless_identity'\
                     )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let has_identity_index: bool = store
                .connection()
                .query_row(
                    "SELECT EXISTS (\
                         SELECT 1 FROM sqlite_master \
                         WHERE type = 'index' \
                           AND name = 'idx_memories_idless_identity_active'\
                     )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(has_identity_column);
            assert!(has_identity_index);
        }
        let barrier = Arc::new(Barrier::new(2));
        let identity = "idless:concurrent-fixture".to_string();

        let workers = ["candidate-a", "candidate-b"].map(|id| {
            let path = path.clone();
            let identity = identity.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = crate::MemoryStore::open(&path).unwrap();
                barrier.wait();
                match store.upsert_idless(&entry(id), &identity).unwrap() {
                    IdlessUpsertResult::Saved => id.to_string(),
                    IdlessUpsertResult::Duplicate { id } => id,
                }
            })
        });
        let winner_ids = workers.map(|worker| worker.join().unwrap());

        assert_eq!(
            winner_ids[0], winner_ids[1],
            "both concurrent writers must resolve the same persisted id"
        );
        let store = crate::MemoryStore::open(&path).unwrap();
        let active_rows: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE idless_identity = ?1 AND archived = 0 AND superseded_by IS NULL",
                params![identity],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_rows, 1,
            "the identity constraint must leave one active row"
        );
    }

    #[test]
    fn explicit_id_update_clears_stale_idless_identity() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        assert_eq!(
            store
                .upsert_idless(&entry("shared-id"), "old-identity")
                .unwrap(),
            IdlessUpsertResult::Saved
        );

        let mut replacement = entry("shared-id");
        replacement.path = "/notes/replaced".to_string();
        replacement.text = "replacement explicit-id payload".to_string();
        store.upsert(&replacement).unwrap();

        assert_eq!(
            store
                .upsert_idless(&entry("new-id"), "old-identity")
                .unwrap(),
            IdlessUpsertResult::Saved,
            "an explicit-ID rewrite must release the obsolete id-less identity"
        );
        let old_identity: Option<String> = store
            .connection()
            .query_row(
                "SELECT idless_identity FROM memories WHERE id = 'shared-id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_identity, None);
    }
}

/// Maximum IDs per batch for IN clause queries (SQLite has a 999 parameter limit).
const IN_BATCH_SIZE: usize = 900;

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

        if vec_available {
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![trimmed])?;
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
    // Closing valid_until at supersession time turns the superseded row into a
    // point-in-time-recoverable version: `as_of` before `now` still returns it,
    // `as_of` after `now` correctly prefers the superseding row. COALESCE keeps
    // an explicitly-set validity window intact. NOTE: this invalidation is
    // specific to supersession; archive_memory (stale/dedup eviction) must NOT
    // close valid_until, since a GC'd fact may still have been true.
    conn.execute(
        "UPDATE memories
         SET superseded_by = ?1, updated_at = ?2, revision = revision + 1,
             valid_until = COALESCE(valid_until, ?2)
         WHERE id = ?3 AND (superseded_by IS NULL OR superseded_by != ?1)",
        params![superseded_by, now, id],
    )?;
    Ok(conn.changes() > 0)
}

#[cfg(test)]
mod tests {
    use super::simple_query_input;

    #[test]
    fn simple_query_input_treats_fts_punctuation_as_separators() {
        let cases = [
            (")))", ""),
            ("!!!", ""),
            ("  foo  ", "foo"),
            ("foo_bar-baz.qux", "foo_bar-baz.qux"),
            ("don't \"panic\"", "don t panic"),
            ("foo)))bar", "foo bar"),
            ("spaced\twords\nok", "spaced words ok"),
            ("模型（搜索）", "模型 搜索"),
        ];

        for (input, expected) in cases {
            assert_eq!(simple_query_input(input), expected, "input={input:?}");
        }
    }
}
