//! v19 (#1115) — reserve a DB-level identity only for new id-less saves.
//!
//! Existing rows intentionally remain `NULL`: legacy duplicate cleanup is an
//! owner-approved data operation, never an automatic schema-migration side
//! effect. The partial index therefore protects future active rows without
//! deleting, rewriting, or making a legacy database fail to migrate.

use rusqlite::Connection;

use crate::error::MemoryError;

use super::table_has_column;

pub(super) fn migrate_v19_add_idless_memory_identity(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    if !table_has_column(conn, "memories", "idless_identity")? {
        conn.execute("ALTER TABLE memories ADD COLUMN idless_identity TEXT", [])?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_idless_identity_active \
         ON memories(idless_identity) \
         WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL;",
    )?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v19_leaves_legacy_duplicates_unmodified_and_indexes_new_identities() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                archived INTEGER NOT NULL DEFAULT 0,
                superseded_by TEXT
            );
            INSERT INTO memories (id) VALUES ('legacy-a'), ('legacy-b');",
        )
        .unwrap();

        migrate_v19_add_idless_memory_identity(&conn).unwrap();

        let legacy_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE idless_identity IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            legacy_rows, 2,
            "migration must not rewrite legacy duplicate rows"
        );

        conn.execute(
            "INSERT INTO memories (id, idless_identity) VALUES ('modern-a', 'identity')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO memories (id, idless_identity) VALUES ('modern-b', 'identity')",
                [],
            )
            .is_err(),
            "new active id-less identities must be unique"
        );
    }
}
