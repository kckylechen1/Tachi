use rusqlite::{functions::FunctionFlags, params, Connection, Transaction};
use std::sync::{Mutex, MutexGuard};

use crate::error::MemoryError;

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
    tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
    insert_lexical_fts(
        tx,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
    )?;
    // Symbolic trigram index stores raw memories column bytes (JSON tags
    // included) so LIKE eligibility matches a table scan (#1331).
    sync_memories_symbolic_fts(tx, id)?;
    Ok(())
}

/// Refresh one row in `memories_symbolic_fts` from the live `memories` row.
/// No-op when the virtual table is absent (legacy fixtures / mid-migration).
///
/// Public so repair writers (`tachi repair` quarantine restore/purge/junk)
/// share the same tx-scoped projection as normal CRUD (#1335 oracle).
pub fn sync_memories_symbolic_fts(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    insert_symbolic_fts(conn, id)?;
    Ok(())
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
    let _fts_guard = acquire_fts_sync_lock();
    insert_lexical_fts(
        tx,
        id,
        path,
        summary,
        text,
        keywords_joined,
        entities_joined,
    )?;
    insert_symbolic_fts(tx, id)
}

fn insert_lexical_fts(
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

fn insert_symbolic_fts(conn: &Connection, id: &str) -> Result<(), MemoryError> {
    conn.execute(
        r#"INSERT INTO memories_symbolic_fts(id, path, summary, text, keywords, entities, topic)
           SELECT id, path, summary, text, keywords, entities, topic
           FROM memories WHERE id = ?1"#,
        params![id],
    )?;
    Ok(())
}

/// Drop one row from `memories_symbolic_fts`. No-op when the table is absent.
pub fn delete_memories_symbolic_fts(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}
