//! v19: atomic identity reservations for id-less memory saves (#1115).
//!
//! Existing `memories` rows are intentionally not backfilled. A legacy DB can
//! contain historical exact duplicates, and deciding which one to retain is a
//! separate owner-approved data operation. This migration therefore creates an
//! empty reservation table: legacy rows are explicitly exempt, while every new
//! id-less save is guarded by the transaction that writes its memory row.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(super) fn migrate_v19_idless_save_identity(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS idless_save_identities (
             identity TEXT PRIMARY KEY,
             path TEXT NOT NULL,
             text TEXT NOT NULL,
             memory_id TEXT NOT NULL,
             created_at TEXT NOT NULL DEFAULT ''
         );",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec};

    #[test]
    fn exempts_legacy_duplicate_rows_without_failing_migration() {
        libsimple::enable_auto_extension().expect("enable simple tokenizer");
        register_sqlite_vec();
        let conn = Connection::open_in_memory().expect("open legacy fixture");
        let _ = try_load_sqlite_vec(&conn);
        init_schema(&conn).expect("initialize fixture");
        conn.execute_batch("DROP TABLE idless_save_identities;")
            .expect("remove new table to simulate pre-v19 schema");
        conn.execute(
            "INSERT INTO memories(id, path, text, timestamp) VALUES (?1, ?2, ?3, ?4)",
            (
                "legacy-duplicate-a",
                "/legacy/duplicate",
                "same text",
                "2026-07-16T00:00:00Z",
            ),
        )
        .expect("insert first legacy duplicate");
        conn.execute(
            "INSERT INTO memories(id, path, text, timestamp) VALUES (?1, ?2, ?3, ?4)",
            (
                "legacy-duplicate-b",
                "/legacy/duplicate",
                "same text",
                "2026-07-16T00:00:01Z",
            ),
        )
        .expect("insert second legacy duplicate");

        migrate_v19_idless_save_identity(&conn).expect("migration must exempt legacy rows");

        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .expect("count legacy rows");
        let reservations: i64 = conn
            .query_row("SELECT COUNT(*) FROM idless_save_identities", [], |row| {
                row.get(0)
            })
            .expect("count reservations");
        assert_eq!(row_count, 2, "legacy data must remain untouched");
        assert_eq!(reservations, 0, "legacy rows are deliberately exempt");
    }
}
