//! v18 (#1035) — append-only adjudication and signature-event tables.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(super) fn migrate_v18_dispatch_adjudications(conn: &Connection) -> Result<usize, MemoryError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dispatch_adjudications (
            adjudication_id TEXT PRIMARY KEY,
            outcome_id TEXT NOT NULL,
            event_key TEXT NOT NULL UNIQUE,
            verdict TEXT,
            not_required_reason TEXT,
            actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            evidence_ref TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at TEXT NOT NULL DEFAULT '',
            insertion_seq INTEGER NOT NULL,
            CHECK ((verdict IS NOT NULL AND length(trim(verdict)) > 0 AND not_required_reason IS NULL)
                   OR (verdict IS NULL AND not_required_reason IS NOT NULL AND length(trim(not_required_reason)) > 0))
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_adjudications_outcome
            ON dispatch_adjudications(outcome_id, created_at);
        CREATE TABLE IF NOT EXISTS dispatch_adjudication_signatures (
            adjudication_id TEXT NOT NULL,
            signature_id TEXT NOT NULL,
            evidence_ref TEXT,
            resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
            PRIMARY KEY (adjudication_id, signature_id)
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_adjudication_signatures_signature
            ON dispatch_adjudication_signatures(signature_id);",
    )?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v18_creates_adjudication_tables_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(migrate_v18_dispatch_adjudications(&conn).unwrap(), 1);
        assert_eq!(migrate_v18_dispatch_adjudications(&conn).unwrap(), 1);
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'dispatch_adjudications')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }
}
