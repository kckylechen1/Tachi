use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::path_router;

pub(super) fn migrate_v1_path_normalize(conn: &mut Connection) -> Result<usize, MemoryError> {
    let tx = conn.transaction()?;
    let mut count = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_id_path_batch(&tx, &after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        after_id = rows.last().map(|(id, _)| id.clone()).unwrap_or(after_id);
        for (id, path) in rows {
            let normalized = path_router::normalize_path(&path);
            if normalized != path {
                tx.execute(
                    "UPDATE memories SET path = ?1 WHERE id = ?2",
                    params![normalized, id],
                )?;
                count += 1;
            }
        }
    }
    tx.commit()?;
    Ok(count)
}

fn fetch_id_path_batch(
    conn: &Connection,
    after_id: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path FROM memories
         WHERE id > ?1
         ORDER BY id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![after_id, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn migrate_v2_scope_normalize(conn: &mut Connection) -> Result<usize, MemoryError> {
    // PR-1 already added a CHECK constraint that prevents non-canonical scope
    // values. Per-project DBs that pre-existed PR-1 should also have been
    // normalized by PR-1's migration when init_schema runs. This is a
    // defensive sweep: count rows that LOOK wrong and normalize.
    let count = conn.execute(
        "UPDATE memories SET scope = 'general'
         WHERE scope IS NULL OR scope NOT IN ('user','project','general')",
        [],
    )?;
    Ok(count)
}

pub(super) fn migrate_v3_handoff_standardize(conn: &mut Connection) -> Result<usize, MemoryError> {
    // Bare "/handoff" -> "/handoff/unknown".
    let count = conn.execute(
        "UPDATE memories SET path = '/handoff/unknown' WHERE path = '/handoff'",
        [],
    )?;
    Ok(count)
}
