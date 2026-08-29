//! v34: attached-session receipt spine for #1678 — append-only authoritative
//! session events, the materialized canonical state projection, typed
//! intervention request/result receipts, and host-owned capability
//! advertisements.
//!
//! Every table is additive and keyed to an existing v33 attachment row. The
//! spine stays receipts-only: no table here can spawn, signal, or reap a
//! host-owned session, and no column stores a transcript.

use rusqlite::{params, Connection};

use crate::error::MemoryError;

pub(super) fn migrate_v34_harness_session_spine(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::install_harness_session_spine_schema(conn)?;
    crate::db::schema::validate_harness_session_spine_schema(conn)?;
    // Five tables plus two indexes; the receipt count stays stable on replay
    // because CREATE IF NOT EXISTS is idempotent and the sentinel gate runs
    // before this function.
    Ok(7)
}

/// Upgrade the shipped v34 intervention and capability-advertisement tables
/// without rewriting the v34 sentinel's meaning. The rebuild preserves row
/// identities and AUTOINCREMENT high-water marks, marks unrecoverable v34
/// capability provenance as unknown, and canonicalizes the previously open
/// JSON payload into the closed eight-boolean vocabulary.
pub(super) fn migrate_v35_harness_session_spine_receipts(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    let has_capability_source: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('harness_session_interventions')
            WHERE name = 'capability_source'
         )",
        [],
        |row| row.get(0),
    )?;
    if has_capability_source {
        crate::db::schema::validate_harness_session_spine_schema(conn)?;
        return Ok(0);
    }

    let intervention_sequence: i64 = conn.query_row(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'harness_session_interventions'), 0)",
        [],
        |row| row.get(0),
    )?;
    let advertisement_sequence: i64 = conn.query_row(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'harness_session_capability_advertisements'), 0)",
        [],
        |row| row.get(0),
    )?;

    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_harness_session_interventions_attachment;
         ALTER TABLE harness_session_interventions
             RENAME TO harness_session_interventions_v34;
         ALTER TABLE harness_session_capability_advertisements
             RENAME TO harness_session_capability_advertisements_v34;",
    )?;
    crate::db::schema::install_harness_session_spine_schema(conn)?;
    conn.execute_batch(
        "INSERT INTO harness_session_interventions (
            intervention_row_id, attachment_id, request_id, kind, reason,
            expected_session_revision, capability_source, requested_by, requested_at
         )
         SELECT intervention_row_id, attachment_id, request_id, kind, reason,
                expected_session_revision,
                'legacy_unknown',
                requested_by, requested_at
         FROM harness_session_interventions_v34 AS intervention;

         INSERT INTO harness_session_capability_advertisements (
            advertisement_row_id, attachment_id, advertisement_seq,
            capabilities_json, source_host_identity, advertised_at
         )
         SELECT advertisement_row_id, attachment_id, advertisement_seq,
                '{\"observe\":' || CASE WHEN json_type(capabilities_json, '$.observe') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"wait\":' || CASE WHEN json_type(capabilities_json, '$.wait') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"prompt\":' || CASE WHEN json_type(capabilities_json, '$.prompt') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"cancel\":' || CASE WHEN json_type(capabilities_json, '$.cancel') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"resume\":' || CASE WHEN json_type(capabilities_json, '$.resume') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"load\":' || CASE WHEN json_type(capabilities_json, '$.load') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"events\":' || CASE WHEN json_type(capabilities_json, '$.events') = 'true' THEN 'true' ELSE 'false' END ||
                ',\"artifacts\":' || CASE WHEN json_type(capabilities_json, '$.artifacts') = 'true' THEN 'true' ELSE 'false' END || '}',
                source_host_identity, advertised_at
         FROM harness_session_capability_advertisements_v34;

         DROP TABLE harness_session_interventions_v34;
         DROP TABLE harness_session_capability_advertisements_v34;",
    )?;
    let intervention_max: i64 = conn.query_row(
        "SELECT COALESCE(MAX(intervention_row_id), 0) FROM harness_session_interventions",
        [],
        |row| row.get(0),
    )?;
    let advertisement_max: i64 = conn.query_row(
        "SELECT COALESCE(MAX(advertisement_row_id), 0) FROM harness_session_capability_advertisements",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "DELETE FROM sqlite_sequence WHERE name = 'harness_session_interventions'",
        [],
    )?;
    let intervention_high_water = intervention_sequence.max(intervention_max);
    if intervention_high_water > 0 {
        conn.execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES ('harness_session_interventions', ?1)",
            params![intervention_high_water],
        )?;
    }
    conn.execute(
        "DELETE FROM sqlite_sequence WHERE name = 'harness_session_capability_advertisements'",
        [],
    )?;
    let advertisement_high_water = advertisement_sequence.max(advertisement_max);
    if advertisement_high_water > 0 {
        conn.execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES ('harness_session_capability_advertisements', ?1)",
            params![advertisement_high_water],
        )?;
    }
    crate::db::schema::validate_harness_session_spine_schema(conn)?;
    Ok(2)
}

#[cfg(test)]
pub(super) fn install_shipped_v34_receipt_tables_for_test(conn: &Connection) {
    conn.execute_batch(
        "DROP INDEX idx_harness_session_interventions_attachment;
         DROP TABLE harness_session_interventions;
         DROP TABLE harness_session_capability_advertisements;
         CREATE TABLE harness_session_interventions (
            intervention_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            request_id TEXT NOT NULL CHECK (length(request_id) <= 128 AND length(trim(request_id)) > 0 AND instr(CAST(request_id AS BLOB), CAST(x'00' AS BLOB)) = 0),
            kind TEXT NOT NULL CHECK (kind IN ('request_status', 'prompt_or_correct', 'request_pause', 'request_cancel', 'request_resume')),
            reason TEXT NOT NULL CHECK (length(reason) > 0 AND length(reason) <= 1000 AND instr(CAST(reason AS BLOB), CAST(x'00' AS BLOB)) = 0),
            expected_session_revision INTEGER NOT NULL CHECK (expected_session_revision >= 0),
            requested_by TEXT NOT NULL CHECK (length(trim(requested_by)) > 0),
            requested_at TEXT NOT NULL,
            UNIQUE (attachment_id, request_id)
         );
         CREATE INDEX idx_harness_session_interventions_attachment
            ON harness_session_interventions(attachment_id, requested_at);
         CREATE TABLE harness_session_capability_advertisements (
            advertisement_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            advertisement_seq INTEGER NOT NULL CHECK (advertisement_seq >= 1),
            capabilities_json TEXT NOT NULL CHECK (json_valid(capabilities_json)),
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            advertised_at TEXT NOT NULL,
            UNIQUE (attachment_id, advertisement_seq)
         );",
    )
    .unwrap();
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
    fn v35_rebuilds_shipped_v34_receipts_and_preserves_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        migrate_v34_harness_session_spine(&conn).unwrap();
        install_shipped_v34_receipt_tables_for_test(&conn);
        conn.execute_batch(
            "INSERT INTO harness_session_capability_advertisements
                (advertisement_row_id, attachment_id, advertisement_seq, capabilities_json, source_host_identity, advertised_at)
             VALUES (11, 'attachment-old', 1, '{\"observe\":true,\"foreign\":\"discard\"}', 'host-old', '2026-08-29T00:00:00Z');
             INSERT INTO harness_session_interventions
                (intervention_row_id, attachment_id, request_id, kind, reason, expected_session_revision, requested_by, requested_at)
             VALUES (12, 'attachment-old', 'request-old', 'request_status', 'observe', 3, 'host-old', '2026-08-29T00:00:01Z');
             UPDATE sqlite_sequence SET seq = 120 WHERE name = 'harness_session_interventions';
             UPDATE sqlite_sequence SET seq = 110 WHERE name = 'harness_session_capability_advertisements';",
        )
        .unwrap();

        assert_eq!(
            migrate_v35_harness_session_spine_receipts(&conn).unwrap(),
            2
        );
        crate::db::schema::validate_harness_session_spine_schema(&conn).unwrap();
        let capability_source: String = conn
            .query_row(
                "SELECT capability_source FROM harness_session_interventions WHERE intervention_row_id = 12",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(capability_source, "legacy_unknown");
        let intervention_sequence: i64 = conn
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'harness_session_interventions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let advertisement_sequence: i64 = conn
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'harness_session_capability_advertisements'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(intervention_sequence, 120);
        assert_eq!(advertisement_sequence, 110);
        conn.execute(
            "INSERT INTO harness_session_interventions
             (attachment_id, request_id, kind, reason, expected_session_revision,
              capability_source, requested_by, requested_at)
             VALUES ('attachment-old', 'request-new', 'request_status', 'observe', 4,
                     'declared', 'host-old', '2026-08-29T00:00:02Z')",
            [],
        )
        .unwrap();
        assert_eq!(conn.last_insert_rowid(), 121);
        conn.execute(
            "INSERT INTO harness_session_capability_advertisements
             (attachment_id, advertisement_seq, capabilities_json, source_host_identity, advertised_at)
             VALUES ('attachment-old', 2,
                     '{\"observe\":true,\"wait\":false,\"prompt\":false,\"cancel\":false,\"resume\":false,\"load\":false,\"events\":false,\"artifacts\":false}',
                     'host-old', '2026-08-29T00:00:02Z')",
            [],
        )
        .unwrap();
        assert_eq!(conn.last_insert_rowid(), 111);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let capabilities_json: String = conn
            .query_row(
                "SELECT capabilities_json FROM harness_session_capability_advertisements WHERE advertisement_row_id = 11",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            capabilities_json,
            r#"{"observe":true,"wait":false,"prompt":false,"cancel":false,"resume":false,"load":false,"events":false,"artifacts":false}"#
        );
        assert_eq!(
            migrate_v35_harness_session_spine_receipts(&conn).unwrap(),
            0
        );
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
                "harness_session_interventions",
                "capability_source IN ('declared', 'advertised', 'legacy_unknown')",
            ),
            (
                "harness_session_intervention_results",
                "length(detail) > 0 AND ",
            ),
            (
                "harness_session_capability_advertisements",
                "length(CAST(capabilities_json AS BLOB)) <= 256",
            ),
            (
                "harness_session_capability_advertisements",
                "json_remove(capabilities_json, '$.observe', '$.wait', '$.prompt', '$.cancel', '$.resume', '$.load', '$.events', '$.artifacts') = '{}'",
            ),
            (
                "harness_session_capability_advertisements",
                "COALESCE(json_type(capabilities_json, '$.observe'), '') IN ('true', 'false')",
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
