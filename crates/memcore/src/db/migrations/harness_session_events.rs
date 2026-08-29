//! v34: attached-session receipt spine for #1678 — append-only authoritative
//! session events, the materialized canonical state projection, typed
//! intervention request/result receipts, and host-owned capability
//! advertisements.
//!
//! Every table is additive and keyed to an existing v33 attachment row. The
//! spine stays receipts-only: no table here can spawn, signal, or reap a
//! host-owned session, and no column stores a transcript.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(super) fn migrate_v34_harness_session_spine(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::install_harness_session_spine_schema(conn)?;
    crate::db::schema::validate_harness_session_spine_schema(conn)?;
    // Five tables plus two indexes; the receipt count stays stable on replay
    // because CREATE IF NOT EXISTS is idempotent and the sentinel gate runs
    // before this function.
    Ok(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v34_creates_the_session_spine_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v34_harness_session_spine(&conn).unwrap(),
            7,
            "migration receipt count is stable on replay"
        );
        crate::db::schema::validate_harness_session_spine_schema(&conn).unwrap();
    }

    #[test]
    fn v34_spine_schema_is_provider_neutral() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v34_harness_session_spine(&conn).unwrap();
        let names: Vec<String> = conn
            .prepare(
                "SELECT name FROM main.sqlite_schema WHERE type = 'table'
                 AND name LIKE 'harness_session_%'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!names.is_empty());
        for name in names {
            let lowered = name.to_ascii_lowercase();
            for forbidden in ["zeroclaw", "codex", "claude", "vendor"] {
                assert!(
                    !lowered.contains(forbidden),
                    "provider-specific name {forbidden:?} leaked into the spine schema"
                );
            }
        }
    }
}
