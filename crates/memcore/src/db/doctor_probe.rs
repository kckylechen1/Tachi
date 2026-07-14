//! Read-only diagnostic probes over an arbitrary (possibly foreign, possibly
//! corrupt) sqlite file, used by `tachi doctor`.
//!
//! `doctor` deliberately does *not* go through `MemoryStore`: it scans
//! filesystem paths that may be legacy schemas, placeholders, or corrupt
//! files, and needs read-only/immutable opens without triggering schema
//! migration. These functions take an already-open `&Connection` (or a raw
//! path/URI to open) rather than living on `MemoryStore` — only the raw SQL
//! and connection-opening mechanics are centralized here; classification
//! decisions stay in tachi-server.

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
///
/// #1041 B6 (codex round-4): returns the query's own `Result` rather than
/// folding EVERY error into `false`. Before this fix, "genuinely no such
/// table" (`QueryReturnedNoRows` — the expected, common case) and "couldn't
/// even check `sqlite_master`" (a real I/O or corruption error, still
/// possible past `schema_version`'s much cheaper header-only probe)
/// collapsed into the identical `false`. `probe_keyword_suspects` then read
/// that `false` as "nothing to scan" and returned `Ok(default)` — exactly
/// the false-clean `Some(0)` ("evaluated, clean") this module's own F7 fix
/// (see that function's doc) was written to eliminate for the case where
/// the LATER `memories` query fails; a failure at THIS earlier existence
/// check fell through the same hole. Each caller now decides for itself
/// whether a real error here should propagate (`probe_keyword_suspects`
/// does, since it feeds `cross_domain_suspect_count`'s `None`-vs-`Some(0)`
/// distinction) or collapse to a best-effort `false`
/// (`foundry_job_status_counts` does, matching its own documented
/// all-zero-on-anything-uncertain contract) — that choice belongs at each
/// call site, not baked into this shared primitive for everyone.
pub fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    match conn.query_row(
        "select 1 from sqlite_master where type in ('table','view') and name = ?1",
        [name],
        |_| Ok(()),
    ) {
        Ok(()) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(err) => Err(err),
    }
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

/// Result of a best-effort keyword-heuristic cross-domain scan (#1041 S4):
/// how many rows matched at least one of the caller's keyword substrings,
/// plus a small sample of matching ids so an operator can spot-check hits.
/// Informational only — `tachi doctor` never blocks or auto-fixes on this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeywordSuspectProbe {
    pub count: usize,
    pub sample_ids: Vec<String>,
}

/// Scan `memories` for rows whose `text`/`summary`/`path` contain any of
/// `keywords` (case-insensitive substring via SQL `LIKE`, which is already
/// ASCII-case-insensitive in sqlite; CJK keywords have no case to fold).
/// Degrades gracefully: DBs with a foreign/legacy `memories` schema missing
/// `summary`/`path` fall back to a `text`-only scan. A `memories` table that
/// doesn't exist at all yields an all-zero probe (genuinely "nothing to
/// scan", not a failure). #1041 F7: a `memories` table that DOES exist but
/// still can't be queried even in the narrowest (`text`-only) fallback — a
/// truly foreign/corrupt schema — now propagates that error instead of
/// silently folding it into `Ok(default)`. That fold was a false-clean
/// diagnostic: `DoctorFinding::cross_domain_suspect_count` is documented as
/// `None` when "the probe itself errored", but the error never reached the
/// caller to produce that `None` — every failure surfaced as `Some(0)`,
/// indistinguishable from "checked, and clean". Callers that want the old
/// best-effort collapse (e.g. informational-only tripwires) still get it —
/// they call this via `.ok()`, which now correctly yields `None` on a real
/// failure instead of never seeing one.
pub fn probe_keyword_suspects(
    conn: &Connection,
    keywords: &[&str],
    sample_limit: usize,
) -> rusqlite::Result<KeywordSuspectProbe> {
    if keywords.is_empty() {
        return Ok(KeywordSuspectProbe::default());
    }
    // #1041 B6: propagate a real `sqlite_master` query error instead of
    // collapsing it into "table absent" — this is exactly the signal
    // `classify`'s `cross_domain_suspect_count` needs to tell "not
    // evaluated" (`None`) apart from "evaluated, clean" (`Some(0)`).
    if !table_exists(conn, "memories")? {
        return Ok(KeywordSuspectProbe::default());
    }
    match probe_keyword_suspects_over_columns(
        conn,
        keywords,
        sample_limit,
        &["text", "summary", "path"],
    ) {
        Ok(probe) => Ok(probe),
        Err(_) => probe_keyword_suspects_over_columns(conn, keywords, sample_limit, &["text"]),
    }
}

fn probe_keyword_suspects_over_columns(
    conn: &Connection,
    keywords: &[&str],
    sample_limit: usize,
    columns: &[&str],
) -> rusqlite::Result<KeywordSuspectProbe> {
    let per_keyword_predicate = columns
        .iter()
        .map(|c| format!("{c} LIKE ?"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let predicate = keywords
        .iter()
        .map(|_| format!("({per_keyword_predicate})"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let like_values: Vec<String> = keywords
        .iter()
        .flat_map(|k| std::iter::repeat_n(format!("%{k}%"), columns.len()))
        .collect();
    let bind_params: Vec<&dyn rusqlite::types::ToSql> = like_values
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect();

    let count_sql = format!("select count(*) from memories where {predicate}");
    let count: i64 = conn.query_row(&count_sql, bind_params.as_slice(), |row| row.get(0))?;

    let sample_ids = if sample_limit == 0 {
        Vec::new()
    } else {
        let sample_sql =
            format!("select id from memories where {predicate} order by id limit {sample_limit}");
        let mut stmt = conn.prepare(&sample_sql)?;
        let rows = stmt.query_map(bind_params.as_slice(), |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };

    Ok(KeywordSuspectProbe {
        count: count.max(0) as usize,
        sample_ids,
    })
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
    // #1041 B6: this function's own contract is best-effort/all-zero-on-
    // uncertainty (see doc above) — that collapse is an explicit, visible
    // choice made HERE, not silently baked into `table_exists` itself.
    if !table_exists(conn, "foundry_jobs").unwrap_or(false) {
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
