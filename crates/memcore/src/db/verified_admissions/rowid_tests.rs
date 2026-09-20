//! Physical-rowid schema admission and real replacement discriminators.

use super::*;
use crate::db::{DbOpenContext, MigrationAuthority, OpenIntent, StoreProfile};
use crate::MemoryStore;

const ALIASES: &[&str] = &["rowid", "ROWID", "oid", "OiD", "_rowid_", "_RoWiD_"];
const DECLARATIONS: &[&str] = &[
    "TEXT",
    "INTEGER GENERATED ALWAYS AS (length(admission_id)) VIRTUAL",
    "INTEGER GENERATED ALWAYS AS (length(admission_id)) STORED",
];

fn legacy_conn(extra: &str, recursive: bool, without_rowid: bool) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.pragma_update(None, "recursive_triggers", recursive)
        .unwrap();
    let suffix = if without_rowid { "WITHOUT ROWID" } else { "" };
    conn.execute_batch(&format!(
        "CREATE TABLE agent_identities (
             agent_identity_id TEXT PRIMARY KEY,
             created_at TEXT NOT NULL
         );
         CREATE TABLE identity_admissions (
             {extra}
             admission_id TEXT PRIMARY KEY,
             agent_identity_id TEXT,
             connection_id TEXT NOT NULL,
             state TEXT NOT NULL,
             created_at TEXT NOT NULL,
             UNIQUE(agent_identity_id, connection_id)
         ) {suffix};"
    ))
    .unwrap();
    conn
}

fn schema_snapshot(conn: &Connection) -> Vec<(String, String)> {
    let mut statement = conn
        .prepare("SELECT name, COALESCE(sql, '') FROM main.sqlite_schema ORDER BY name")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

// Reproduce already-installed/offline-drifted databases without admitting them
// through the repaired installer. The canonical DDL and triggers stay unchanged.
fn install_existing_objects_unchecked(conn: &Connection) {
    for (_, _, _, sql) in CANONICAL_OBJECTS {
        conn.execute_batch(sql).unwrap();
    }
    for (_, _, sql) in CANONICAL_TRIGGERS {
        conn.execute_batch(sql).unwrap();
    }
}

fn seed_admissions(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO agent_identities (agent_identity_id, created_at)
         VALUES ('agent-victim', '2026-09-20T00:00:00Z'),
                ('agent-source', '2026-09-20T00:00:00Z');
         INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
         VALUES ('victim', 'agent-victim', 'connection-victim', 'verified',
                 '2026-09-20T00:00:00Z'),
                ('source', 'agent-source', 'connection-source', 'self_asserted',
                 '2026-09-20T00:00:00Z');",
    )
    .unwrap();
}

#[test]
fn installer_refuses_each_plain_and_generated_alias_before_any_ddl() {
    for recursive in [false, true] {
        for alias in ALIASES {
            for declaration in DECLARATIONS {
                let conn = legacy_conn(&format!("\"{alias}\" {declaration},"), recursive, false);
                let before = schema_snapshot(&conn);
                let error = install_verified_admission_schema(&conn)
                    .expect_err("rowid aliases must refuse before schema installation");
                assert!(error.to_string().contains("non-canonical rowid layout"));
                assert_eq!(schema_snapshot(&conn), before);
            }
        }
    }
}

#[test]
fn validator_refuses_each_preexisting_plain_and_generated_alias_without_repair() {
    for recursive in [false, true] {
        for alias in ALIASES {
            for declaration in DECLARATIONS {
                let conn = legacy_conn(&format!("\"{alias}\" {declaration},"), recursive, false);
                install_existing_objects_unchecked(&conn);
                let before = schema_snapshot(&conn);
                let error = validate_verified_admission_schema(&conn)
                    .expect_err("existing canonical triggers do not attest a shadowed rowid");
                assert!(error.to_string().contains("non-canonical rowid layout"));
                assert_eq!(schema_snapshot(&conn), before);
            }
        }
    }
}

#[test]
fn canonical_layout_keeps_allowed_progress_and_append_only_protection() {
    for recursive in [false, true] {
        let conn = legacy_conn("operator_note TEXT,", recursive, false);
        install_verified_admission_schema(&conn).unwrap();
        validate_verified_admission_schema(&conn).unwrap();
        seed_admissions(&conn);
        assert_eq!(
            conn.execute(
                "UPDATE identity_admissions SET operator_note='ordinary metadata'
                 WHERE admission_id='source'",
                [],
            )
            .unwrap(),
            1
        );
        let error = conn
            .execute(
                "UPDATE OR REPLACE identity_admissions
                 SET _rowid_=(SELECT _rowid_ FROM identity_admissions WHERE admission_id='victim')
                 WHERE admission_id='source'",
                [],
            )
            .expect_err("the physical-rowid victim must survive");
        assert!(error.to_string().contains("append-only"));
        let state: String = conn
            .query_row(
                "SELECT state FROM identity_admissions WHERE admission_id='victim'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "verified");
    }
}

#[test]
fn shadowed_rowid_is_a_real_recursive_off_replacement_bypass() {
    let conn = legacy_conn("rowid TEXT,", false, false);
    install_existing_objects_unchecked(&conn);
    seed_admissions(&conn);
    assert_eq!(
        conn.execute(
            "UPDATE OR REPLACE identity_admissions
             SET _rowid_=(SELECT _rowid_ FROM identity_admissions WHERE admission_id='victim')
             WHERE admission_id='source'",
            [],
        )
        .unwrap(),
        1
    );
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT admission_id, state FROM identity_admissions ORDER BY admission_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows, vec![("source".to_string(), "self_asserted".to_string())]);
    assert!(validate_verified_admission_schema(&conn).is_err());
}

#[test]
fn main_layout_cannot_be_certified_by_a_temp_namesake() {
    let conn = legacy_conn("rowid TEXT,", false, false);
    conn.execute_batch("CREATE TEMP TABLE identity_admissions (admission_id TEXT PRIMARY KEY)")
        .unwrap();
    assert!(validate_identity_admission_rowid_layout(&conn).is_err());

    let missing = Connection::open_in_memory().unwrap();
    missing
        .execute_batch("CREATE TEMP TABLE identity_admissions (admission_id TEXT PRIMARY KEY)")
        .unwrap();
    let before = schema_snapshot(&missing);
    assert!(install_verified_admission_schema(&missing).is_err());
    assert_eq!(schema_snapshot(&missing), before);
}

#[test]
fn without_rowid_layout_refuses_before_installation() {
    let conn = legacy_conn("", false, true);
    let before = schema_snapshot(&conn);
    let error = install_verified_admission_schema(&conn)
        .expect_err("the admission triggers require a real rowid table");
    assert!(error.to_string().contains("non-canonical rowid layout"));
    assert_eq!(schema_snapshot(&conn), before);
}

#[test]
fn current_product_store_reopen_refuses_alias_drift_without_repair() {
    for declaration in [
        "rowid TEXT",
        "\"RoWiD\" INTEGER GENERATED ALWAYS AS (length(admission_id)) VIRTUAL",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.db");
        let context = DbOpenContext {
            intent: OpenIntent::OpenExisting,
            migration: MigrationAuthority::Deny,
            required_profile: StoreProfile::TachiFull,
        };
        drop(
            MemoryStore::open_with_context(path.to_str().unwrap(), &context)
                .expect("create the current product store"),
        );
        let before = {
            let raw = Connection::open(&path).unwrap();
            seed_admissions(&raw);
            raw.execute_batch(&format!(
                "ALTER TABLE identity_admissions ADD COLUMN {declaration}"
            ))
            .unwrap();
            schema_snapshot(&raw)
        };
        let error = MemoryStore::open_with_context(path.to_str().unwrap(), &context)
            .err()
            .expect("a current-version drifted store must fail closed on production reopen");
        assert!(error.to_string().contains("non-canonical rowid layout"));
        let raw = Connection::open(&path).unwrap();
        assert_eq!(schema_snapshot(&raw), before);
        let state: String = raw
            .query_row(
                "SELECT state FROM identity_admissions WHERE admission_id='victim'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "verified");
        let version: u32 = raw
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, crate::db::migrations::EXPECTED_SCHEMA_VERSION);
    }
}

#[test]
fn unicode_whitespace_cannot_mask_a_missing_consumer_column() {
    for whitespace in ['\u{00a0}', '\u{2003}', '\u{202f}'] {
        let conn = legacy_conn("", false, false);
        for (_, name, _, sql) in CANONICAL_OBJECTS {
            let ddl = if *name == "identity_admission_verification_receipts" {
                sql.replace("verified_at TEXT", &format!("verified_at{whitespace}TEXT"))
            } else {
                sql.to_string()
            };
            conn.execute_batch(&ddl).unwrap();
        }
        for (_, _, sql) in CANONICAL_TRIGGERS {
            conn.execute_batch(sql).unwrap();
        }
        assert!(conn
            .prepare("SELECT verified_at FROM identity_admission_verification_receipts")
            .is_err());
        let error = validate_verified_admission_schema(&conn)
            .expect_err("Unicode token drift must not normalize to canonical DDL");
        assert!(error.to_string().contains("missing or non-canonical"));
    }
}

#[test]
fn ascii_whitespace_remains_formatting_only() {
    assert_eq!(
        normalize_schema_sql(" SELECT\tvalue\r\nFROM\x0ctable_name ; \n"),
        "SELECT value FROM table_name"
    );
    assert_ne!(
        normalize_schema_sql("SELECT value\u{00a0}FROM table_name"),
        normalize_schema_sql("SELECT value FROM table_name")
    );
}
