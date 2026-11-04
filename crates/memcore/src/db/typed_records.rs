//! Durable schema for host-declared records. Revisions and lineage are
//! independent append-only rows in the existing database.

use crate::MemoryError;
use rusqlite::Connection;

pub(crate) const TABLES_SQL: &str = r#"
CREATE TABLE typed_records (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    kind_version INTEGER NOT NULL CHECK(kind_version > 0),
    keys_json TEXT NOT NULL CHECK(json_valid(keys_json) AND json_type(keys_json) = 'object'),
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    links_json TEXT NOT NULL CHECK(json_valid(links_json) AND json_type(links_json) = 'array'),
    revision INTEGER NOT NULL CHECK(revision > 0),
    content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64 AND content_sha256 NOT GLOB '*[^0-9a-f]*'),
    writer TEXT NOT NULL,
    created_at TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE typed_record_supersessions (
    source_id TEXT PRIMARY KEY NOT NULL REFERENCES typed_records(id),
    target_id TEXT NOT NULL UNIQUE REFERENCES typed_records(id),
    receipt_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    CHECK(source_id != target_id)
) WITHOUT ROWID;
"#;

const RECORD_INSERT: &str = r#"
CREATE TRIGGER typed_records_insert_guard
BEFORE INSERT ON typed_records
WHEN tachi_reserved_reference_write_enabled() = 0
 OR EXISTS(SELECT 1 FROM typed_records WHERE id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'typed records require an authorized append; replacement is forbidden');
END
"#;
const RECORD_UPDATE: &str = r#"
CREATE TRIGGER typed_records_no_update
BEFORE UPDATE ON typed_records
BEGIN
    SELECT RAISE(ABORT, 'typed record revisions are immutable');
END
"#;
const RECORD_DELETE: &str = r#"
CREATE TRIGGER typed_records_no_delete
BEFORE DELETE ON typed_records
BEGIN
    SELECT RAISE(ABORT, 'typed record revisions are immutable');
END
"#;
const LINEAGE_INSERT: &str = r#"
CREATE TRIGGER typed_record_supersessions_insert_guard
BEFORE INSERT ON typed_record_supersessions
WHEN tachi_reserved_reference_write_enabled() = 0
 OR EXISTS(SELECT 1 FROM typed_record_supersessions WHERE source_id = NEW.source_id
           OR target_id = NEW.target_id OR receipt_id = NEW.receipt_id)
BEGIN
    SELECT RAISE(ABORT, 'typed lineage requires an authorized append; replacement is forbidden');
END
"#;
const LINEAGE_UPDATE: &str = r#"
CREATE TRIGGER typed_record_supersessions_no_update
BEFORE UPDATE ON typed_record_supersessions
BEGIN
    SELECT RAISE(ABORT, 'typed record lineage is immutable');
END
"#;
const LINEAGE_DELETE: &str = r#"
CREATE TRIGGER typed_record_supersessions_no_delete
BEFORE DELETE ON typed_record_supersessions
BEGIN
    SELECT RAISE(ABORT, 'typed record lineage is immutable');
END
"#;
const KIND_INSERT: &str = r#"
CREATE TRIGGER typed_record_kinds_insert_guard
BEFORE INSERT ON hard_state
WHEN (NEW.namespace = 'typed_record_kinds'
      AND tachi_reserved_reference_write_enabled() = 0)
 OR EXISTS(SELECT 1 FROM hard_state WHERE namespace = 'typed_record_kinds'
           AND (rowid = NEW.rowid OR (namespace = NEW.namespace AND key = NEW.key)))
BEGIN
    SELECT RAISE(ABORT, 'typed record definitions require an authorized append');
END
"#;
const KIND_UPDATE: &str = r#"
CREATE TRIGGER typed_record_kinds_no_update
BEFORE UPDATE ON hard_state
WHEN OLD.namespace = 'typed_record_kinds' OR NEW.namespace = 'typed_record_kinds'
 OR EXISTS(SELECT 1 FROM hard_state WHERE namespace = 'typed_record_kinds'
           AND rowid != OLD.rowid
           AND (rowid = NEW.rowid OR (namespace = NEW.namespace AND key = NEW.key)))
BEGIN
    SELECT RAISE(ABORT, 'typed record definitions are immutable');
END
"#;
const KIND_DELETE: &str = r#"
CREATE TRIGGER typed_record_kinds_no_delete
BEFORE DELETE ON hard_state
WHEN OLD.namespace = 'typed_record_kinds'
BEGIN
    SELECT RAISE(ABORT, 'typed record definitions are immutable');
END
"#;

pub(crate) const TRIGGERS: &[(&str, &str, &str)] = &[
    ("typed_records_insert_guard", "typed_records", RECORD_INSERT),
    ("typed_records_no_update", "typed_records", RECORD_UPDATE),
    ("typed_records_no_delete", "typed_records", RECORD_DELETE),
    (
        "typed_record_supersessions_insert_guard",
        "typed_record_supersessions",
        LINEAGE_INSERT,
    ),
    (
        "typed_record_supersessions_no_update",
        "typed_record_supersessions",
        LINEAGE_UPDATE,
    ),
    (
        "typed_record_supersessions_no_delete",
        "typed_record_supersessions",
        LINEAGE_DELETE,
    ),
    ("typed_record_kinds_insert_guard", "hard_state", KIND_INSERT),
    ("typed_record_kinds_no_update", "hard_state", KIND_UPDATE),
    ("typed_record_kinds_no_delete", "hard_state", KIND_DELETE),
];

pub(crate) fn expected_trigger(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    TRIGGERS
        .iter()
        .copied()
        .find(|(expected, _, _)| name.eq_ignore_ascii_case(expected))
}

pub(crate) fn install(conn: &Connection) -> Result<(), MemoryError> {
    validate_kind_rowid_layout(conn)?;
    conn.execute_batch(TABLES_SQL)?;
    for (_, _, sql) in TRIGGERS {
        conn.execute_batch(sql)?;
    }
    Ok(())
}

/// The namespace guards must address the physical rowid, never an alias
/// column (including a generated column hidden from table_info).
pub(crate) fn validate_kind_rowid_layout(conn: &Connection) -> Result<(), MemoryError> {
    let canonical: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main'
                       AND name='hard_state' AND type='table' AND wr=0)
         AND NOT EXISTS(SELECT 1 FROM pragma_table_xinfo('hard_state','main')
                        WHERE name COLLATE NOCASE IN ('rowid','oid','_rowid_'))",
        [],
        |row| row.get(0),
    )?;
    if !canonical {
        return Err(MemoryError::InvalidArg(
            "typed kind definitions require the canonical hard_state rowid layout".into(),
        ));
    }
    Ok(())
}
