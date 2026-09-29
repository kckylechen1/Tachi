//! Deterministic read-open contention at the production entry.
//!
//! `sqlite3_open_v2` runs every registered auto-extension (libsimple's
//! tokenizer, sqlite-vec) while opening a connection, and those inits need a
//! schema read of the database file. That read happens BEFORE
//! `configure_connection` can install the busy timeout, so a file lock that
//! excludes readers — another connection's `BEGIN EXCLUSIVE` on a
//! rollback-journal database, the same boundary `doctor_probe_ops.rs`
//! documents — fails the OPEN itself with the connection error
//! `automatic extension loading failed: ...` (rc SQLITE_BUSY). This is the
//! observed live shape `open <project> read store: SQLite error: automatic
//! extension loading failed:` from the runtime's named read cache
//! (`open_read_store` → `MemoryStore::open_read_only_with_label`).
//!
//! The fixtures here seed a real store through the production write path, so
//! the read open exercises the exact same schema/identity/trigger validation
//! the daemon performs — not a helper-only shortcut.

use super::*;
use crate::error::MemoryError;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::tempdir;

/// Seed a current-schema store through the production write open, then leave
/// the file in rollback-journal (`DELETE`) mode.
///
/// Production stores run WAL, where a plain writer transaction does not
/// exclude readers; the deterministic reader-excluding lock is `BEGIN
/// EXCLUSIVE` on a rollback journal. The journal flip is fixture-only: the
/// file keeps its production schema, stamps, and identity, so the read-only
/// open below runs the full production validation chain.
fn seed_production_shaped_store(db_path: &std::path::Path, db_label: &str) {
    let db_str = db_path.to_string_lossy().to_string();
    {
        let store = crate::MemoryStore::open_with_label(&db_str, db_label)
            .expect("seed store through the production write open");
        drop(store);
    }
    let normalizer = Connection::open(db_path).expect("open journal normalizer");
    let mode: String = normalizer
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .expect("normalize fixture journal to DELETE");
    assert_eq!(mode.to_ascii_uppercase(), "DELETE");
}

/// A short-lived exclusive file lock at the read-open boundary must not fail
/// the production read-only open: the bounded typed retry outlives the
/// contention window without weakening the busy-timeout policy, erroring on
/// any non-BUSY failure class, or replaying anything (an open performs no
/// writes).
#[test]
fn read_store_open_survives_a_transient_exclusive_lock() {
    crate::db::enable_simple_auto_extension().unwrap();
    register_sqlite_vec();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("contended-read-open.db");
    seed_production_shaped_store(&db_path, "contention-probe");
    let db_str = db_path.to_string_lossy().to_string();

    let locker = open_raw(&db_path).expect("open contender connection");
    locker
        .execute_batch("BEGIN EXCLUSIVE;")
        .expect("take the reader-excluding lock");

    let (released_tx, released_rx) = mpsc::channel::<()>();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        locker
            .execute_batch("ROLLBACK;")
            .expect("release the exclusive lock");
        let _ = released_tx.send(());
    });

    let opened = crate::MemoryStore::open_read_only_with_label(&db_str, "contention-probe");
    released_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("contender must release its lock");
    releaser.join().expect("releaser thread");

    let store = opened.expect(
        "the production read-only open must outlive a short reader-excluding lock instead of \
         failing at the auto-extension boundary before its busy timeout exists",
    );
    // The handle must be a genuinely usable read projection of the same file.
    let memories: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .expect("the recovered read handle must serve queries");
    assert_eq!(memories, 0, "freshly seeded fixture store has no rows");
}

/// Mechanism pin: while the reader-excluding lock is STILL held, the open
/// failure that reaches the caller is the typed BUSY/LOCKED class (the same
/// rc classification `retry_memory_locked` keys on) carrying SQLite's
/// auto-extension message — not a corruption or permission error.
#[test]
fn read_store_open_failure_under_a_held_lock_is_typed_busy() {
    crate::db::enable_simple_auto_extension().unwrap();
    register_sqlite_vec();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("held-lock-read-open.db");
    seed_production_shaped_store(&db_path, "contention-probe");
    let db_str = db_path.to_string_lossy().to_string();

    let locker = open_raw(&db_path).expect("open contender connection");
    locker
        .execute_batch("BEGIN EXCLUSIVE;")
        .expect("hold the reader-excluding lock for the whole open");

    let error = match crate::MemoryStore::open_read_only_with_label(&db_str, "contention-probe") {
        Ok(_) => panic!("an indefinitely held exclusive lock must still fail the open"),
        Err(error) => error,
    };
    locker
        .execute_batch("ROLLBACK;")
        .expect("release the lock again");

    let MemoryError::Sqlite(sqlite_error) = error else {
        panic!("open failure under contention must be a SQLite error, got: {error:?}");
    };
    assert!(
        crate::db::sqlite_error_is_locked(&sqlite_error),
        "the contended open failure must stay in the typed BUSY/LOCKED class so the \
         bounded retry classification remains rc-based: {sqlite_error:?}"
    );
}

/// Negative control: corruption and a missing database are real open failures
/// and must surface promptly as errors — never be retried into a false
/// success or slowed by the lock-retry budget.
#[test]
fn read_store_open_does_not_mask_corrupt_or_missing_databases() {
    crate::db::enable_simple_auto_extension().unwrap();
    register_sqlite_vec();

    let dir = tempdir().unwrap();
    let corrupt_path = dir.path().join("corrupt.db");
    std::fs::write(&corrupt_path, b"this is definitely not a sqlite database\n")
        .expect("write corrupt fixture");
    let corrupt_str = corrupt_path.to_string_lossy().to_string();

    let started = std::time::Instant::now();
    let corrupt =
        match crate::MemoryStore::open_read_only_with_label(&corrupt_str, "contention-probe") {
            Ok(_) => panic!("a corrupt database must fail the read-only open"),
            Err(error) => error,
        };
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a non-BUSY failure class must not burn the lock-retry budget: {:?}",
        started.elapsed()
    );
    assert!(
        !matches!(
            &corrupt,
            MemoryError::Sqlite(rusqlite::Error::SqliteFailure(err, _))
                if rusqlite::ErrorCode::DatabaseBusy == err.code
                || rusqlite::ErrorCode::DatabaseLocked == err.code
        ),
        "corruption must surface as its own error class, not as contention: {corrupt:?}"
    );

    let missing_str = dir.path().join("absent.db").to_string_lossy().to_string();
    let missing =
        match crate::MemoryStore::open_read_only_with_label(&missing_str, "contention-probe") {
            Ok(_) => panic!("a missing database must fail the read-only open"),
            Err(error) => error,
        };
    assert!(
        !matches!(
            &missing,
            MemoryError::Sqlite(rusqlite::Error::SqliteFailure(err, _))
                if rusqlite::ErrorCode::DatabaseBusy == err.code
                || rusqlite::ErrorCode::DatabaseLocked == err.code
        ),
        "a missing file is not contention: {missing:?}"
    );
}
