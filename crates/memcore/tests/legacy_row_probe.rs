//! Isolated SQL tests for a proposed initializer fast probe.
//!
//! The candidate is compiled here but is NOT wired into schema initialization.
//! These tests do not certify open-time admission, full validity normalization,
//! FTS repair, or an end-to-end performance improvement.

use memcore::MemoryError;
use rusqlite::Connection;

#[path = "../src/db/schema/legacy_rows.rs"]
mod candidate;

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (
            id INTEGER PRIMARY KEY, timestamp TEXT, created_at TEXT,
            updated_at TEXT, revision INTEGER, valid_from TEXT
         );
         CREATE TABLE observed_updates (id INTEGER, field TEXT, value TEXT);
         CREATE TRIGGER created_observer AFTER UPDATE OF created_at ON memories
         BEGIN INSERT INTO observed_updates VALUES (NEW.id, 'created', NEW.created_at); END;
         CREATE TRIGGER updated_observer AFTER UPDATE OF updated_at ON memories
         BEGIN INSERT INTO observed_updates VALUES (NEW.id, 'updated', NEW.updated_at); END;
         CREATE TRIGGER revision_observer AFTER UPDATE OF revision ON memories
         BEGIN INSERT INTO observed_updates VALUES (NEW.id, 'revision', NEW.revision); END;",
    )
    .unwrap();
    conn
}

fn original_prefix(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET created_at = timestamp WHERE created_at IS NULL OR created_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET updated_at = created_at WHERE updated_at IS NULL OR updated_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET revision = 1 WHERE revision IS NULL OR revision <= 0",
        [],
    )?;
    Ok(())
}

type Row = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

fn rows(conn: &Connection) -> Vec<Row> {
    conn.prepare(
        "SELECT id,timestamp,created_at,updated_at,revision,valid_from FROM memories ORDER BY id",
    )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn updates(conn: &Connection) -> Vec<(i64, String, Option<String>)> {
    conn.prepare("SELECT id,field,value FROM observed_updates ORDER BY rowid")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn healthy_rows_skip_the_legacy_mutator() {
    let conn = fixture();
    conn.execute_batch(
        "INSERT INTO memories VALUES (1,'2026-01-01T00:00:00Z','created','updated',3,'valid');",
    )
    .unwrap();
    let before = rows(&conn);
    candidate::normalize_legacy_memory_rows(&conn, |_| {
        panic!("healthy rows must not invoke the validity repair")
    })
    .unwrap();
    assert_eq!(rows(&conn), before);
    assert!(updates(&conn).is_empty());
}

#[test]
fn empty_store_skips_the_legacy_mutator() {
    let conn = fixture();
    candidate::normalize_legacy_memory_rows(&conn, |_| panic!("empty store")).unwrap();
    assert!(rows(&conn).is_empty());
}

#[test]
fn whitespace_is_not_reclassified_as_empty() {
    let conn = fixture();
    conn.execute_batch("INSERT INTO memories VALUES (1,'timestamp',' ',' ',1,' ');")
        .unwrap();
    candidate::normalize_legacy_memory_rows(&conn, |_| panic!("whitespace is not empty"))
        .unwrap();
    assert!(updates(&conn).is_empty());
}

#[test]
fn dirty_prefix_preserves_values_and_trigger_order() {
    let old = fixture();
    let new = fixture();
    let values = [None, Some(""), Some(" "), Some("value")];
    let revisions = [None, Some(-1_i64), Some(0), Some(3)];
    for id in 0..256_i64 {
        let created = values[(id as usize) % 4];
        let updated = values[(id as usize / 4) % 4];
        let valid = values[(id as usize / 16) % 4];
        let revision = revisions[(id as usize / 64) % 4];
        for conn in [&old, &new] {
            conn.execute(
                "INSERT INTO memories VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![id, "2026-01-01T00:00:00Z", created, updated, revision, valid],
            )
            .unwrap();
        }
    }
    original_prefix(&old).unwrap();
    let mut called = 0;
    candidate::normalize_legacy_memory_rows(&new, |_| {
        called += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(called, 1, "retain the existing validity mutator on dirty data");
    assert_eq!(rows(&new), rows(&old));
    assert_eq!(updates(&new), updates(&old));
}

#[test]
fn validity_only_damage_reaches_the_existing_mutator() {
    let conn = fixture();
    conn.execute_batch("INSERT INTO memories VALUES (1,'timestamp','created','updated',1,NULL);")
        .unwrap();
    let mut called = false;
    candidate::normalize_legacy_memory_rows(&conn, |_| {
        called = true;
        Ok(())
    })
    .unwrap();
    assert!(called);
    assert!(
        updates(&conn).is_empty(),
        "unrelated prefix updates remain no-ops"
    );
}

#[test]
fn validity_error_rolls_back_with_the_callers_transaction() {
    let mut conn = fixture();
    conn.execute_batch("INSERT INTO memories VALUES (1,'timestamp',NULL,'',0,NULL);")
        .unwrap();
    let before = rows(&conn);
    {
        let tx = conn.transaction().unwrap();
        let result = candidate::normalize_legacy_memory_rows(&tx, |_| {
            Err(MemoryError::InvalidArg(
                "injected validity failure".to_string(),
            ))
        });
        assert!(result.is_err());
        // The initializer owns this transaction; error drops it uncommitted.
    }
    assert_eq!(rows(&conn), before);
    assert!(updates(&conn).is_empty());
}

#[test]
fn missing_required_columns_are_not_silently_accepted() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE memories(id INTEGER);")
        .unwrap();
    let mut called = false;
    let result = candidate::normalize_legacy_memory_rows(&conn, |_| {
        called = true;
        Ok(())
    });
    assert!(result.is_err());
    assert!(!called);
}
