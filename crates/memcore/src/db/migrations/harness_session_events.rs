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

    #[test]
    fn v34_validator_refuses_missing_confirmation_reference_bounds() {
        for (table, removed_clause) in [
            (
                "harness_session_events",
                "AND instr(CAST(authority_confirmation_ref AS BLOB), CAST(x'00' AS BLOB)) = 0",
            ),
            (
                "harness_session_intervention_results",
                "length(authority_confirmation_ref) <= 128 AND ",
            ),
            (
                "harness_session_events",
                "length(payload_digest) <= 128 AND ",
            ),
            ("harness_session_interventions", "length(reason) > 0 AND "),
            (
                "harness_session_intervention_results",
                "length(detail) > 0 AND ",
            ),
        ] {
            let conn = Connection::open_in_memory().unwrap();
            migrate_v34_harness_session_spine(&conn).unwrap();
            conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
            let changed = conn
                .execute(
                    "UPDATE sqlite_schema
                     SET sql = replace(sql, ?1, '')
                     WHERE type = 'table' AND name = ?2",
                    rusqlite::params![removed_clause, table],
                )
                .unwrap();
            assert_eq!(changed, 1, "must mutate the {table} fixture");
            conn.execute_batch("PRAGMA writable_schema = OFF;").unwrap();

            let error = crate::db::schema::validate_harness_session_spine_schema(&conn)
                .expect_err("drifted confirmation-reference constraint must fail closed");
            assert!(error.to_string().contains(table), "{table}: {error}");
        }
    }

    #[test]
    fn v34_validator_refuses_state_table_without_primary_key() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v34_harness_session_spine(&conn).unwrap();
        conn.execute_batch(
            "DROP TABLE harness_session_state;
             CREATE TABLE harness_session_state (
                 attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
                 canonical_state TEXT NOT NULL,
                 canonical_revision INTEGER NOT NULL,
                 terminal_digest TEXT,
                 conflicting_terminal_digest TEXT,
                 cleanup_recorded INTEGER NOT NULL DEFAULT 0,
                 last_event_id TEXT,
                 pre_disconnect_rank INTEGER NOT NULL DEFAULT -1,
                 updated_at TEXT NOT NULL
             );",
        )
        .unwrap();
        let error = crate::db::schema::validate_harness_session_spine_schema(&conn)
            .expect_err("state table without its primary key must fail closed");
        assert!(
            error.to_string().contains("harness_session_state"),
            "{error}"
        );
    }
}
