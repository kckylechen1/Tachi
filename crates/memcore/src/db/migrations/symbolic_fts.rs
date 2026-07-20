//! v22: `memories_symbolic_fts` trigram projection for symbolic candidate
//! retrieval (#1331 / #1335 oracle fix-round).
//!
//! Creates the FTS5 trigram virtual table (idempotent) and performs a full
//! rebuild from live `memories` rows. A full rebuild — not an insert-missing
//! backfill — is required because earlier sentinel migrations in the same
//! upgrade (v1/v3/v4/v9 path rewrites, etc.) can change indexed columns after
//! `ensure_fts_backfilled` has already copied pre-migration values into the
//! projection.

use rusqlite::Connection;

use crate::error::MemoryError;

/// DDL matching `schema/ddl.rs` / `store/enrichment.rs` so fresh CREATE and
/// the versioned migration stay byte-compatible.
pub(super) const MEMORIES_SYMBOLIC_FTS_DDL: &str = r#"CREATE VIRTUAL TABLE IF NOT EXISTS memories_symbolic_fts USING fts5(
            id,
            path,
            summary,
            text,
            keywords,
            entities,
            topic,
            tokenize = 'trigram case_sensitive 0'
        );"#;

pub(super) fn migrate_v22_memories_symbolic_fts(conn: &Connection) -> Result<usize, MemoryError> {
    conn.execute_batch(MEMORIES_SYMBOLIC_FTS_DDL)?;
    rebuild_memories_symbolic_fts(conn)
}

/// Drop every symbolic-FTS row and re-insert from live `memories`. No-op when
/// the virtual table is absent (should not happen after the CREATE above).
pub(super) fn rebuild_memories_symbolic_fts(conn: &Connection) -> Result<usize, MemoryError> {
    let present: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !present {
        return Ok(0);
    }
    conn.execute("DELETE FROM memories_symbolic_fts", [])?;
    let inserted = conn.execute(
        r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
           SELECT id, path, summary, text, keywords, entities, topic FROM memories"#,
        [],
    )?;
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn open_minimal() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '',
                summary TEXT NOT NULL DEFAULT '',
                text TEXT NOT NULL DEFAULT '',
                keywords TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]',
                topic TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn v22_creates_table_and_full_backfills() {
        let conn = open_minimal();
        conn.execute(
            "INSERT INTO memories (id, path, summary, text, keywords, entities, topic)
             VALUES ('a', '/handoff/unknown', 's', '记 body', '[]', '[]', 't')",
            [],
        )
        .unwrap();
        assert_eq!(migrate_v22_memories_symbolic_fts(&conn).unwrap(), 1);
        let path: String = conn
            .query_row(
                "SELECT path FROM memories_symbolic_fts WHERE id = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(path, "/handoff/unknown");
        // Idempotent CREATE + rebuild.
        assert_eq!(migrate_v22_memories_symbolic_fts(&conn).unwrap(), 1);
    }

    #[test]
    fn rebuild_refreshes_stale_path_after_legacy_rewrite() {
        let conn = open_minimal();
        migrate_v22_memories_symbolic_fts(&conn).unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, summary, text, keywords, entities, topic)
             VALUES ('h', '/handoff', 's', 'handoffuniqueterm body', '[]', '[]', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
             VALUES ('h', '/handoff', 's', 'handoffuniqueterm body', '[]', '[]', 't')",
            [],
        )
        .unwrap();

        // Simulate v3 handoff standardize mutating memories only.
        conn.execute(
            "UPDATE memories SET path = '/handoff/unknown' WHERE id = 'h'",
            [],
        )
        .unwrap();
        let stale: String = conn
            .query_row(
                "SELECT path FROM memories_symbolic_fts WHERE id = 'h'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, "/handoff");

        rebuild_memories_symbolic_fts(&conn).unwrap();
        let fresh: String = conn
            .query_row(
                "SELECT path FROM memories_symbolic_fts WHERE id = 'h'",
                params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fresh, "/handoff/unknown");
    }
}
