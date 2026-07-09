use rusqlite::Connection;

use crate::error::MemoryError;

/// Drop the retired `packs` and `agent_projections` tables left over from the
/// skill-pack system. Both were originally created with
/// `CREATE TABLE IF NOT EXISTS`, so long-lived DBs may still hold now-unused
/// copies after the pack subsystem is removed from the code.
///
/// Idempotent: a no-op on DBs that never created the tables, and skipped
/// entirely once the `v10_drop_pack_tables` sentinel is set.
///
/// Returns the number of tables actually dropped (0, 1, or 2).
pub(super) fn migrate_v10_drop_pack_tables(conn: &Connection) -> Result<usize, MemoryError> {
    fn exists(conn: &Connection, name: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .unwrap_or(0);
        n > 0
    }

    let mut dropped = 0usize;
    if exists(conn, "packs") {
        conn.execute_batch("DROP TABLE packs")?;
        dropped += 1;
    }
    if exists(conn, "agent_projections") {
        conn.execute_batch("DROP TABLE agent_projections")?;
        dropped += 1;
    }
    Ok(dropped)
}
