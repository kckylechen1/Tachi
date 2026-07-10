use rusqlite::Connection;

use crate::error::MemoryError;

/// Drop the retired `domains` registry table left over from the domain
/// registry (`register_domain`/`get_domain`/`list_domains`/`delete_domain`)
/// removal (#757). It was originally created with
/// `CREATE TABLE IF NOT EXISTS`, so long-lived DBs may still hold a now-unused
/// copy after the registry subsystem is removed from the code.
///
/// This does NOT touch the free-text `memories.domain` column, which is a
/// separate, still-live field.
///
/// Idempotent: a no-op on DBs that never created the table, and skipped
/// entirely once the `v11_drop_domains_table` sentinel is set.
///
/// Returns the number of tables actually dropped (0 or 1).
pub(super) fn migrate_v11_drop_domains_table(conn: &Connection) -> Result<usize, MemoryError> {
    // `unwrap_or(0)` treats a failed existence-check (lock/I/O/authorizer
    // error) as "table absent", so the sentinel below still gets written and
    // the migration is not retried. This mirrors the same trade-off in v10's
    // `exists()` (pack_retire.rs) — an established framework pattern here,
    // not something specific to this migration.
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

    if exists(conn, "domains") {
        conn.execute_batch("DROP TABLE domains")?;
        return Ok(1);
    }
    Ok(0)
}
