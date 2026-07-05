//! Read-only diagnostic probes over an arbitrary (possibly foreign, possibly
//! corrupt) sqlite file, used by `tachi doctor`.
//!
//! `doctor` deliberately does *not* go through `MemoryStore`: it scans
//! filesystem paths that may be legacy schemas, placeholders, or corrupt
//! files, and needs read-only/immutable opens without triggering schema
//! migration. These functions take an already-open `&Connection` (or a raw
//! path/URI to open) rather than living on `MemoryStore` — only the raw SQL
//! and connection-opening mechanics are centralized here; classification
//! decisions stay in memory-server.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Open a database read-only, disabling WAL writes, via the `immutable=1` URI
/// flag. `uri` must already be the fully-formed `file:...?mode=ro&immutable=1`
/// string — percent-encoding the path is the caller's concern.
pub fn open_immutable_readonly(uri: &str) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

/// `pragma schema_version` — the cheapest possible "is this actually a
/// SQLite file" probe; fails immediately on garbage bytes.
pub fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("pragma schema_version", [], |r| r.get(0))
}

/// Whether a table or view named `name` exists in the schema.
pub fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "select 1 from sqlite_master where type in ('table','view') and name = ?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn count_scalar(conn: &Connection, sql: &str) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(n.max(0) as usize)
}

pub fn count_memories_rows(conn: &Connection) -> rusqlite::Result<usize> {
    count_scalar(conn, "select count(*) from memories")
}

pub fn count_chunks_rows(conn: &Connection) -> rusqlite::Result<usize> {
    count_scalar(conn, "select count(*) from chunks")
}

pub fn count_memories_vec_rows(conn: &Connection) -> rusqlite::Result<usize> {
    count_scalar(conn, "select count(*) from memories_vec")
}

pub fn count_memories_missing_domain(conn: &Connection) -> rusqlite::Result<usize> {
    count_scalar(
        conn,
        "select count(*) from memories where domain is null or domain=''",
    )
}

/// Best-effort breakdown of `foundry_jobs` rows by (lowercased) status.
/// All-zero when the table doesn't exist (foreign/legacy DBs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FoundryJobStatusCounts {
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pending: usize,
}

pub fn foundry_job_status_counts(conn: &Connection) -> FoundryJobStatusCounts {
    let mut counts = FoundryJobStatusCounts::default();
    if !table_exists(conn, "foundry_jobs") {
        return counts;
    }
    counts.total = count_scalar(conn, "select count(*) from foundry_jobs").unwrap_or(0);
    let by_status = |status: &str| -> usize {
        conn.query_row(
            "select count(*) from foundry_jobs where lower(status) = ?1",
            [status],
            |row| row.get::<_, i64>(0).map(|n| n.max(0) as usize),
        )
        .unwrap_or(0)
    };
    counts.completed = by_status("completed");
    counts.skipped = by_status("skipped");
    counts.failed = by_status("failed");
    counts.pending = by_status("pending");
    counts
}

/// Open a connection for a checkpoint-copy step: read-write, best-effort 5s
/// busy timeout (matches the prior inline behavior — a timeout failure is
/// not fatal, the checkpoint attempt still proceeds).
pub fn open_for_wal_checkpoint(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    let _ = conn.busy_timeout(Duration::from_millis(5_000));
    Ok(conn)
}

/// `PRAGMA wal_checkpoint(TRUNCATE);` — run only against a throwaway copy,
/// never the live daemon-owned file (the caller enforces that liveness
/// guard before ever reaching this call).
pub fn checkpoint_wal_truncate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
}

/// Open a raw read-write connection for building ad hoc (possibly
/// non-canonical) sqlite fixtures, e.g. in `doctor` tests that simulate
/// legacy/partial schemas `MemoryStore::open` would never produce on its
/// own (it always runs the full schema migration).
pub fn open_raw(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open(path)
}
