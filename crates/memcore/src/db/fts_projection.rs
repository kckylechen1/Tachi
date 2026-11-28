use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension, Transaction};
use std::sync::{Mutex, MutexGuard};

use crate::error::MemoryError;

pub(crate) const FTS_PROJECTION_MAP_TABLE: &str = "memories_fts_projection_map";
pub(crate) const FTS_PROJECTION_MAP_DDL: &str = r#"CREATE TABLE IF NOT EXISTS memories_fts_projection_map (
    projection_key INTEGER PRIMARY KEY,
    memory_id TEXT NOT NULL UNIQUE
);"#;

/// Match CRUD's `Vec<String>::join(" ")` when projecting stored JSON arrays.
/// This query-only function adds no persistent schema dependency or authority.
pub(super) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "memcore_fts_terms",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let raw = context.get::<Option<String>>(0)?;
            Ok(raw.map(|raw| {
                serde_json::from_str::<Vec<String>>(&raw)
                    .map(|terms| terms.join(" "))
                    // Legacy malformed/non-string arrays remain readable.
                    // Preserve the old SQL replace/trim fallback, including
                    // SQL trim's ASCII-space-only behavior and NULL above.
                    .unwrap_or_else(|_| {
                        raw.replace(['[', ']', '"'], " ")
                            .trim_matches(' ')
                            .to_owned()
                    })
            }))
        },
    )
}

static FTS_SYNC_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn acquire_fts_sync_lock() -> MutexGuard<'static, ()> {
    FTS_SYNC_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn symbolic_fts_present(conn: &Connection) -> Result<bool, MemoryError> {
    Ok(crate::db::table_exists(conn, "memories_symbolic_fts")?)
}

fn projection_map_present(conn: &Connection) -> Result<bool, MemoryError> {
    Ok(crate::db::table_exists(conn, FTS_PROJECTION_MAP_TABLE)?)
}

fn legacy_sync_symbolic(conn: &Connection, id: &str) -> Result<(), MemoryError> {
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    insert_symbolic_legacy(conn, id)
}

fn insert_symbolic_legacy(conn: &Connection, id: &str) -> Result<(), MemoryError> {
    conn.execute(
        r#"INSERT INTO memories_symbolic_fts(id, path, summary, text, keywords, entities, topic)
           SELECT id, path, summary, text, keywords, entities, topic
           FROM memories WHERE id = ?1"#,
        params![id],
    )?;
    Ok(())
}

fn lookup_projection_key(conn: &Connection, id: &str) -> Result<Option<i64>, MemoryError> {
    Ok(conn
        .query_row(
            "SELECT projection_key FROM memories_fts_projection_map WHERE memory_id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?)
}

#[derive(Debug)]
struct ProjectionKeyState {
    key: i64,
    lexical_id: Option<String>,
    symbolic_id: Option<String>,
}

/// One indexed mapping lookup plus point-rowid occupant checks. An occupant
/// belonging to another ID is corruption, never a delete target.
fn existing_projection_key(
    conn: &Connection,
    id: &str,
    symbolic: bool,
) -> Result<ProjectionKeyState, MemoryError> {
    let state = if symbolic {
        conn.query_row(
            "SELECT p.projection_key,
                    (SELECT f.id FROM memories_fts f WHERE f.rowid = p.projection_key),
                    (SELECT s.id FROM memories_symbolic_fts s WHERE s.rowid = p.projection_key)
             FROM memories_fts_projection_map p
             WHERE p.memory_id = ?1",
            params![id],
            |row| {
                Ok(ProjectionKeyState {
                    key: row.get(0)?,
                    lexical_id: row.get(1)?,
                    symbolic_id: row.get(2)?,
                })
            },
        )
        .optional()?
    } else {
        conn.query_row(
            "SELECT p.projection_key,
                    (SELECT f.id FROM memories_fts f WHERE f.rowid = p.projection_key)
             FROM memories_fts_projection_map p
             WHERE p.memory_id = ?1",
            params![id],
            |row| {
                Ok(ProjectionKeyState {
                    key: row.get(0)?,
                    lexical_id: row.get(1)?,
                    symbolic_id: None,
                })
            },
        )
        .optional()?
    };
    let state = state.ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "memory '{id}' has no FTS projection mapping; run an explicit FTS repair"
        ))
    })?;
    if state.lexical_id.as_deref().is_some_and(|found| found != id)
        || state
            .symbolic_id
            .as_deref()
            .is_some_and(|found| found != id)
    {
        return Err(MemoryError::InvalidArg(format!(
            "memory '{id}' FTS projection key {} names a different row; run an explicit FTS repair",
            state.key
        )));
    }
    Ok(state)
}

/// Allocate above every existing mapping and FTS rowid. In particular, an
/// unmapped historical orphan can never be mistaken for the new row's projection.
fn next_projection_key(conn: &Connection, symbolic: bool) -> Result<i64, MemoryError> {
    Ok(if symbolic {
        conn.query_row(
            "SELECT MAX(
                 COALESCE((SELECT MAX(projection_key) FROM memories_fts_projection_map), 0),
                 COALESCE((SELECT MAX(rowid) FROM memories_fts), 0),
                 COALESCE((SELECT MAX(rowid) FROM memories_symbolic_fts), 0)
             ) + 1",
            [],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT MAX(
                 COALESCE((SELECT MAX(projection_key) FROM memories_fts_projection_map), 0),
                 COALESCE((SELECT MAX(rowid) FROM memories_fts), 0)
             ) + 1",
            [],
            |row| row.get(0),
        )?
    })
}

fn allocate_projection_key(
    conn: &Connection,
    id: &str,
    symbolic: bool,
) -> Result<i64, MemoryError> {
    let key = next_projection_key(conn, symbolic)?;
    conn.execute(
        "INSERT INTO memories_fts_projection_map(projection_key, memory_id) VALUES (?1, ?2)",
        params![key, id],
    )?;
    Ok(key)
}

pub(crate) const DELETE_LEXICAL_AT_KEY_SQL: &str = "DELETE FROM memories_fts WHERE rowid = ?1";
pub(crate) const DELETE_SYMBOLIC_AT_KEY_SQL: &str =
    "DELETE FROM memories_symbolic_fts WHERE rowid = ?1";

fn point_delete_at_key(conn: &Connection, key: i64, symbolic: bool) -> Result<(), MemoryError> {
    conn.execute(DELETE_LEXICAL_AT_KEY_SQL, [key])?;
    if symbolic {
        conn.execute(DELETE_SYMBOLIC_AT_KEY_SQL, [key])?;
    }
    Ok(())
}

fn insert_lexical_at_keyless(
    conn: &Connection,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, path, summary, text, keywords_joined, entities_joined],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_lexical_at_key(
    conn: &Connection,
    key: i64,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            key,
            id,
            path,
            summary,
            text,
            keywords_joined,
            entities_joined
        ],
    )?;
    Ok(())
}

fn insert_symbolic_at_key(conn: &Connection, key: i64, id: &str) -> Result<(), MemoryError> {
    let inserted = conn.execute(
        r#"INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
           SELECT ?1, id, path, summary, text, keywords, entities, topic
           FROM memories WHERE id = ?2"#,
        params![key, id],
    )?;
    if inserted != 1 {
        return Err(MemoryError::InvalidArg(format!(
            "memory '{id}' is missing while refreshing its FTS projection"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn synchronize_at_key(
    conn: &Connection,
    state: &ProjectionKeyState,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
    symbolic: bool,
    delete_existing: bool,
) -> Result<(), MemoryError> {
    if delete_existing {
        point_delete_at_key(conn, state.key, symbolic)?;
    } else if state.lexical_id.is_some() || (symbolic && state.symbolic_id.is_some()) {
        return Err(MemoryError::InvalidArg(format!(
            "memory '{id}' already has an FTS row at projection key {}",
            state.key
        )));
    }
    insert_lexical_at_key(
        conn,
        state.key,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
    )?;
    if symbolic {
        insert_symbolic_at_key(conn, state.key, id)?;
    }
    Ok(())
}

/// Synchronize an existing memory through its persistent projection key.
pub(crate) fn sync_memories_fts(
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
    if !projection_map_present(tx)? {
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        insert_lexical_at_keyless(
            tx,
            id,
            path,
            summary,
            text,
            keywords_joined,
            entities_joined,
        )?;
        if symbolic_fts_present(tx)? {
            legacy_sync_symbolic(tx, id)?;
        }
        return Ok(());
    }
    let symbolic = symbolic_fts_present(tx)?;
    let state = existing_projection_key(tx, id, symbolic)?;
    synchronize_at_key(
        tx,
        &state,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
        symbolic,
        true,
    )
}

/// Insert a known-new memory. Synchronizing mode first removes only legacy
/// rows with the same ID; insert-only snapshot mode performs no deletes.
pub(crate) fn insert_memories_fts(
    tx: &Transaction<'_>,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    insert_memories_fts_with_mode(
        tx,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_memories_fts_with_mode(
    tx: &Transaction<'_>,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
    delete_legacy_id_rows: bool,
) -> Result<(), MemoryError> {
    let _fts_guard = acquire_fts_sync_lock();
    if !projection_map_present(tx)? {
        if delete_legacy_id_rows {
            tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
            if symbolic_fts_present(tx)? {
                tx.execute(
                    "DELETE FROM memories_symbolic_fts WHERE id = ?1",
                    params![id],
                )?;
            }
        }
        insert_lexical_at_keyless(
            tx,
            id,
            path,
            summary,
            text,
            keywords_joined,
            entities_joined,
        )?;
        if symbolic_fts_present(tx)? {
            insert_symbolic_legacy(tx, id)?;
        }
        return Ok(());
    }
    let symbolic = symbolic_fts_present(tx)?;
    let key = match lookup_projection_key(tx, id)? {
        Some(key) => key,
        None => {
            if delete_legacy_id_rows {
                // A first live claim for this ID owns its historical rows.
                // Only this unmapped case performs an exact-ID cleanup; mapped
                // updates stay on the rowid point-lookup path.
                tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
                if symbolic {
                    tx.execute(
                        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
                        params![id],
                    )?;
                }
            }
            allocate_projection_key(tx, id, symbolic)?
        }
    };
    let state = existing_projection_key(tx, id, symbolic)?;
    debug_assert_eq!(state.key, key);
    synchronize_at_key(
        tx,
        &state,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
        symbolic,
        delete_legacy_id_rows,
    )
}

/// Refresh both projections for an existing memory from its authoritative row.
pub(crate) fn sync_memories_fts_from_memories(
    conn: &Connection,
    id: &str,
) -> Result<(), MemoryError> {
    let _fts_guard = acquire_fts_sync_lock();
    if !projection_map_present(conn)? {
        conn.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        conn.execute(
            r#"INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
               SELECT id, path, summary, text,
                      memcore_fts_terms(keywords), memcore_fts_terms(entities)
               FROM memories WHERE id = ?1"#,
            params![id],
        )?;
        if symbolic_fts_present(conn)? {
            legacy_sync_symbolic(conn, id)?;
        }
        return Ok(());
    }
    let symbolic = symbolic_fts_present(conn)?;
    let state = existing_projection_key(conn, id, symbolic)?;
    sync_from_memories_at_key(conn, id, &state, symbolic)
}

fn sync_from_memories_at_key(
    conn: &Connection,
    id: &str,
    state: &ProjectionKeyState,
    symbolic: bool,
) -> Result<(), MemoryError> {
    point_delete_at_key(conn, state.key, symbolic)?;
    let inserted = conn.execute(
        r#"INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
           SELECT ?1, id, path, summary, text,
                  memcore_fts_terms(keywords), memcore_fts_terms(entities)
           FROM memories WHERE id = ?2"#,
        params![state.key, id],
    )?;
    if inserted != 1 {
        return Err(MemoryError::InvalidArg(format!(
            "memory '{id}' is missing while refreshing its FTS projection"
        )));
    }
    if symbolic {
        insert_symbolic_at_key(conn, state.key, id)?;
    }
    Ok(())
}

/// Repair compatibility seam for external restore writers. The historical
/// public name is retained, but current stores converge BOTH projections and
/// the mapping so legacy lexical SQL around this call cannot strand rowids.
pub fn sync_memories_symbolic_fts(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    register(conn)?;
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        sync_memories_symbolic_fts_inner(&tx, id)?;
        tx.commit()?;
        return Ok(());
    }
    sync_memories_symbolic_fts_inner(conn, id)
}

fn sync_memories_symbolic_fts_inner(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    if !symbolic_fts_present(conn)? {
        return Ok(());
    }
    let _fts_guard = acquire_fts_sync_lock();
    if !projection_map_present(conn)? {
        return legacy_sync_symbolic(conn, id);
    }
    conn.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    if lookup_projection_key(conn, id)?.is_none() {
        allocate_projection_key(conn, id, true)?;
    }
    let state = existing_projection_key(conn, id, true)?;
    sync_from_memories_at_key(conn, id, &state, true)
}

/// Only snapshot import's transaction-proven empty target may omit DELETE.
/// Both projections still share CRUD's process-global tokenizer lock.
pub(crate) fn insert_snapshot_fts(
    tx: &Transaction<'_>,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    insert_memories_fts_with_mode(
        tx,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
        false,
    )
}

/// Delete both projections and the mapping for canonical CRUD deletion.
pub(crate) fn delete_memories_fts(conn: &Connection, id: &str) -> Result<(), MemoryError> {
    let _fts_guard = acquire_fts_sync_lock();
    if !projection_map_present(conn)? {
        conn.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        if symbolic_fts_present(conn)? {
            conn.execute(
                "DELETE FROM memories_symbolic_fts WHERE id = ?1",
                params![id],
            )?;
        }
        return Ok(());
    }
    let symbolic = symbolic_fts_present(conn)?;
    if lookup_projection_key(conn, id)?.is_none() {
        // Anchor and legacy manually-created rows need not already have a
        // projection mapping. Deletion is still exact-ID cleanup, not a
        // corruption report or a reason to leave stale projections behind.
        conn.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        if symbolic {
            conn.execute(
                "DELETE FROM memories_symbolic_fts WHERE id = ?1",
                params![id],
            )?;
        }
        return Ok(());
    }
    let state = existing_projection_key(conn, id, symbolic)?;
    point_delete_at_key(conn, state.key, symbolic)?;
    conn.execute(
        "DELETE FROM memories_fts_projection_map WHERE memory_id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Repair compatibility seam for external purge/delete writers. Exact-ID
/// cleanup handles pre-mapping rows; a conflicting rowid occupant still fails.
pub fn delete_memories_symbolic_fts(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    register(conn)?;
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        delete_memories_symbolic_fts_inner(&tx, id)?;
        tx.commit()?;
        return Ok(());
    }
    delete_memories_symbolic_fts_inner(conn, id)
}

fn delete_memories_symbolic_fts_inner(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    if !symbolic_fts_present(conn)? {
        return Ok(());
    }
    let _fts_guard = acquire_fts_sync_lock();
    if !projection_map_present(conn)? {
        conn.execute(
            "DELETE FROM memories_symbolic_fts WHERE id = ?1",
            params![id],
        )?;
        return Ok(());
    }
    conn.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    if lookup_projection_key(conn, id)?.is_some() {
        let state = existing_projection_key(conn, id, true)?;
        point_delete_at_key(conn, state.key, true)?;
        conn.execute(
            "DELETE FROM memories_fts_projection_map WHERE memory_id = ?1",
            params![id],
        )?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct FtsRepairOutcome {
    pub lexical_inserted: usize,
    pub symbolic_inserted: usize,
    pub changed: bool,
}

/// Move a live mapping off an orphan/NULL-id occupant without pruning that
/// occupant. Insert-only maintenance preserves the orphan; open prunes it first.
fn remap_orphan_occupied_projection_keys(
    conn: &Connection,
    symbolic: bool,
) -> Result<(), MemoryError> {
    loop {
        let occupied = if symbolic {
            conn.query_row(
                "SELECT projection_key FROM (
                     SELECT p.projection_key
                     FROM memories_fts_projection_map p
                     JOIN memories m ON m.id = p.memory_id
                     JOIN memories_fts f ON f.rowid = p.projection_key
                     LEFT JOIN memories owner ON owner.id = f.id
                     WHERE f.id IS NULL OR owner.id IS NULL
                     UNION
                     SELECT p.projection_key
                     FROM memories_fts_projection_map p
                     JOIN memories m ON m.id = p.memory_id
                     JOIN memories_symbolic_fts f ON f.rowid = p.projection_key
                     LEFT JOIN memories owner ON owner.id = f.id
                     WHERE f.id IS NULL OR owner.id IS NULL
                 ) LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        } else {
            conn.query_row(
                "SELECT p.projection_key
                 FROM memories_fts_projection_map p
                 JOIN memories m ON m.id = p.memory_id
                 JOIN memories_fts f ON f.rowid = p.projection_key
                 LEFT JOIN memories owner ON owner.id = f.id
                 WHERE f.id IS NULL OR owner.id IS NULL
                 LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        };
        let Some(old_key) = occupied else {
            return Ok(());
        };
        let memory_id: String = conn.query_row(
            "SELECT memory_id FROM memories_fts_projection_map WHERE projection_key = ?1",
            [old_key],
            |row| row.get(0),
        )?;
        let new_key = next_projection_key(conn, symbolic)?;
        // The other projection may hold the matching live row at the old key.
        // Move that row with the shared mapping while leaving orphan occupants
        // untouched, then let the ordinary missing-row pass fill any vacancy.
        conn.execute(
            "INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
             SELECT ?1, id, path, summary, text, keywords, entities
             FROM memories_fts WHERE rowid = ?2 AND id = ?3",
            params![new_key, old_key, memory_id],
        )?;
        conn.execute(
            "DELETE FROM memories_fts WHERE rowid = ?1 AND id = ?2",
            params![old_key, memory_id],
        )?;
        if symbolic {
            conn.execute(
                "INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
                 SELECT ?1, id, path, summary, text, keywords, entities, topic
                 FROM memories_symbolic_fts WHERE rowid = ?2 AND id = ?3",
                params![new_key, old_key, memory_id],
            )?;
            conn.execute(
                "DELETE FROM memories_symbolic_fts WHERE rowid = ?1 AND id = ?2",
                params![old_key, memory_id],
            )?;
        }
        conn.execute(
            "UPDATE memories_fts_projection_map SET projection_key = ?1 WHERE projection_key = ?2",
            params![new_key, old_key],
        )?;
    }
}

/// Incremental explicit repair. Existing aligned rows are adopted, missing
/// mappings/rows are filled, and any misalignment triggers a full authoritative
/// rebuild before a later point-rowid writer can address a neighbor.
pub(crate) fn repair_memories_fts(
    conn: &Connection,
    prune_orphans: bool,
) -> Result<FtsRepairOutcome, MemoryError> {
    register(conn)?;
    let _fts_guard = acquire_fts_sync_lock();
    repair_memories_fts_locked(conn, prune_orphans)
}

fn repair_memories_fts_locked(
    conn: &Connection,
    prune_orphans: bool,
) -> Result<FtsRepairOutcome, MemoryError> {
    let symbolic = symbolic_fts_present(conn)?;
    let adopt_sql = if symbolic {
        r#"WITH lexical(id, rowid) AS (
               SELECT id, MIN(rowid) FROM memories_fts
               WHERE id IS NOT NULL GROUP BY id HAVING COUNT(*) = 1
           ), symbolic(id, rowid) AS (
               SELECT id, MIN(rowid) FROM memories_symbolic_fts
               WHERE id IS NOT NULL GROUP BY id HAVING COUNT(*) = 1
           )
           INSERT OR IGNORE INTO memories_fts_projection_map(projection_key, memory_id)
           SELECT lexical.rowid, memories.id
           FROM memories
           JOIN lexical ON lexical.id = memories.id
           JOIN symbolic ON symbolic.id = memories.id AND symbolic.rowid = lexical.rowid"#
    } else {
        r#"WITH lexical(id, rowid) AS (
               SELECT id, MIN(rowid) FROM memories_fts
               WHERE id IS NOT NULL GROUP BY id HAVING COUNT(*) = 1
           )
           INSERT OR IGNORE INTO memories_fts_projection_map(projection_key, memory_id)
           SELECT lexical.rowid, memories.id
           FROM memories JOIN lexical ON lexical.id = memories.id"#
    };
    conn.execute(adopt_sql, [])?;

    let allocate_sql = if symbolic {
        r#"WITH missing(id, sequence) AS (
               SELECT m.id, ROW_NUMBER() OVER (ORDER BY m.rowid)
               FROM memories m
               WHERE m.id IS NOT NULL
                 AND m.id NOT IN (SELECT memory_id FROM memories_fts_projection_map)
           ), bounds(base) AS (
               SELECT MAX(
                   COALESCE((SELECT MAX(projection_key) FROM memories_fts_projection_map), 0),
                   COALESCE((SELECT MAX(rowid) FROM memories_fts), 0),
                   COALESCE((SELECT MAX(rowid) FROM memories_symbolic_fts), 0)
               )
           )
           INSERT INTO memories_fts_projection_map(projection_key, memory_id)
           SELECT base + sequence, missing.id FROM missing, bounds"#
    } else {
        r#"WITH missing(id, sequence) AS (
               SELECT m.id, ROW_NUMBER() OVER (ORDER BY m.rowid)
               FROM memories m
               WHERE m.id IS NOT NULL
                 AND m.id NOT IN (SELECT memory_id FROM memories_fts_projection_map)
           ), bounds(base) AS (
               SELECT MAX(
                   COALESCE((SELECT MAX(projection_key) FROM memories_fts_projection_map), 0),
                   COALESCE((SELECT MAX(rowid) FROM memories_fts), 0)
               )
           )
           INSERT INTO memories_fts_projection_map(projection_key, memory_id)
           SELECT base + sequence, missing.id FROM missing, bounds"#
    };
    conn.execute(allocate_sql, [])?;

    let mut outcome = FtsRepairOutcome::default();
    if prune_orphans {
        let mut changes = conn.execute(
            "DELETE FROM memories_fts
             WHERE id IS NULL OR id NOT IN (SELECT id FROM memories WHERE id IS NOT NULL)",
            [],
        )?;
        if symbolic {
            changes += conn.execute(
                "DELETE FROM memories_symbolic_fts
                 WHERE id IS NULL OR id NOT IN (SELECT id FROM memories WHERE id IS NOT NULL)",
                [],
            )?;
        }
        conn.execute(
            "DELETE FROM memories_fts_projection_map
             WHERE memory_id NOT IN (SELECT id FROM memories WHERE id IS NOT NULL)",
            [],
        )?;
        outcome.changed = changes > 0;
    }

    // Move live mappings away from orphan occupants before selecting drift
    // deletions, so preservation never turns into a post-delete rescue.
    remap_orphan_occupied_projection_keys(conn, symbolic)?;

    let lexical_drift: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM memories_fts_projection_map p
             JOIN memories_fts f ON f.id = p.memory_id AND f.rowid != p.projection_key
             UNION ALL
             SELECT 1 FROM memories_fts_projection_map p
             JOIN memories_fts f ON f.rowid = p.projection_key AND f.id IS NOT p.memory_id
             JOIN memories other ON other.id = f.id
         )",
        [],
        |row| row.get(0),
    )?;
    let symbolic_drift = if symbolic {
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM memories_fts_projection_map p
                 JOIN memories_symbolic_fts f ON f.id = p.memory_id AND f.rowid != p.projection_key
                 UNION ALL
                 SELECT 1 FROM memories_fts_projection_map p
                 JOIN memories_symbolic_fts f ON f.rowid = p.projection_key AND f.id IS NOT p.memory_id
                 JOIN memories other ON other.id = f.id
             )",
            [],
            |row| row.get(0),
        )?
    } else {
        false
    };
    if lexical_drift || symbolic_drift {
        if prune_orphans {
            let inserted = rebuild_memories_fts_from_authority_locked(conn)?;
            return Ok(FtsRepairOutcome {
                lexical_inserted: inserted,
                symbolic_inserted: inserted,
                changed: true,
            });
        }
        // Insert-missing maintenance repairs only mapped drift. Orphans and
        // unrelated historical rows stay in place, and the return count below
        // still means rows actually inserted, never a disguised full rebuild.
        let changed = conn.execute(
            "DELETE FROM memories_fts WHERE rowid IN (
                 SELECT f.rowid FROM memories_fts_projection_map p
                 JOIN memories_fts f ON f.id = p.memory_id AND f.rowid != p.projection_key
                 JOIN memories owner ON owner.id = f.id
                 UNION ALL
                 SELECT f.rowid FROM memories_fts_projection_map p
                 JOIN memories_fts f ON f.rowid = p.projection_key AND f.id IS NOT p.memory_id
                 JOIN memories owner ON owner.id = f.id
             )",
            [],
        )?;
        outcome.changed = changed > 0;
        if symbolic {
            let changed = conn.execute(
                "DELETE FROM memories_symbolic_fts WHERE rowid IN (
                     SELECT f.rowid FROM memories_fts_projection_map p
                     JOIN memories_symbolic_fts f ON f.id = p.memory_id AND f.rowid != p.projection_key
                     JOIN memories owner ON owner.id = f.id
                     UNION ALL
                     SELECT f.rowid FROM memories_fts_projection_map p
                     JOIN memories_symbolic_fts f ON f.rowid = p.projection_key AND f.id IS NOT p.memory_id
                     JOIN memories owner ON owner.id = f.id
                 )",
                [],
            )?;
            outcome.changed |= changed > 0;
        }
    }

    outcome.lexical_inserted = conn.execute(
        r#"INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
           SELECT p.projection_key, m.id, m.path, m.summary, m.text,
                  memcore_fts_terms(m.keywords), memcore_fts_terms(m.entities)
           FROM memories_fts_projection_map p
           JOIN memories m ON m.id = p.memory_id
           WHERE p.projection_key NOT IN (SELECT rowid FROM memories_fts)"#,
        [],
    )?;
    if symbolic {
        outcome.symbolic_inserted = conn.execute(
            r#"INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
               SELECT p.projection_key, m.id, m.path, m.summary, m.text,
                      m.keywords, m.entities, m.topic
               FROM memories_fts_projection_map p
               JOIN memories m ON m.id = p.memory_id
               WHERE p.projection_key NOT IN (SELECT rowid FROM memories_symbolic_fts)"#,
            [],
        )?;
    }
    outcome.changed |= outcome.lexical_inserted + outcome.symbolic_inserted > 0;
    Ok(outcome)
}

/// Rebuild the mapping and every projection from authoritative memories. Any
/// historical FTS row, aligned or orphaned, is discarded inside the caller's tx.
pub(crate) fn rebuild_memories_fts_from_authority(conn: &Connection) -> Result<usize, MemoryError> {
    register(conn)?;
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let _fts_guard = acquire_fts_sync_lock();
        let inserted = rebuild_memories_fts_from_authority_locked(&tx)?;
        tx.commit()?;
        return Ok(inserted);
    }
    let _fts_guard = acquire_fts_sync_lock();
    rebuild_memories_fts_from_authority_locked(conn)
}

pub(crate) fn rebuild_memories_fts_from_authority_locked(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    let symbolic = symbolic_fts_present(conn)?;
    conn.execute("DELETE FROM memories_fts", [])?;
    if symbolic {
        conn.execute("DELETE FROM memories_symbolic_fts", [])?;
    }
    conn.execute("DELETE FROM memories_fts_projection_map", [])?;
    conn.execute(
        "INSERT INTO memories_fts_projection_map(memory_id)
         SELECT id FROM memories WHERE id IS NOT NULL",
        [],
    )?;
    let inserted = conn.execute(
        r#"INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
           SELECT p.projection_key, m.id, m.path, m.summary, m.text,
                  memcore_fts_terms(m.keywords), memcore_fts_terms(m.entities)
           FROM memories_fts_projection_map p
           JOIN memories m ON m.id = p.memory_id"#,
        [],
    )?;
    if symbolic {
        conn.execute(
            r#"INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
               SELECT p.projection_key, m.id, m.path, m.summary, m.text,
                      m.keywords, m.entities, m.topic
               FROM memories_fts_projection_map p
               JOIN memories m ON m.id = p.memory_id"#,
            [],
        )?;
    }
    Ok(inserted)
}
