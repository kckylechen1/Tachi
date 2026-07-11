use rusqlite::Connection;

use crate::error::MemoryError;

/// Drop the retired `domains` registry table left over from the domain
/// registry (`register_domain`/`get_domain`/`list_domains`/`delete_domain`)
/// removal (#757). It was originally created with
/// `CREATE TABLE IF NOT EXISTS`, so long-lived DBs may still hold a now-unused
/// copy after the registry subsystem is removed from the code.
///
/// This does NOT touch the free-text `memories.domain` column, which is a
/// separate, still-live field.
///
/// Idempotent: a no-op on DBs that never created the table, and skipped
/// entirely once the `v11_drop_domains_table` sentinel is set.
///
/// Returns the number of tables actually dropped (0 or 1).
pub(super) fn migrate_v11_drop_domains_table(conn: &Connection) -> Result<usize, MemoryError> {
    migrate_v11_inner(conn, real_exists_query)
}

/// The real `sqlite_master` lookup used by [`exists_with_query`] in
/// production. Isolated as its own function so tests can substitute a
/// failing query while `exists_with_query`'s propagate-vs-swallow decision
/// stays the actual code under test (#978).
fn real_exists_query(conn: &Connection, name: &str) -> Result<i64, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )
}

/// Existence-check: does a table named `name` exist in `conn`? Parameterized
/// over the row-count query itself (not the whole existence decision) so
/// tests can inject a failing query and exercise the REAL `?`-propagation
/// here — the code path that decides "error propagates" vs. "collapses to
/// absent via `unwrap_or(0)`" (#978). If someone regresses this to
/// `query(conn, name).unwrap_or(0) > 0`, a test driving a failing query
/// through this function goes from `Err` to `Ok(false)`.
fn exists_with_query(
    conn: &Connection,
    name: &str,
    query: impl Fn(&Connection, &str) -> Result<i64, rusqlite::Error>,
) -> Result<bool, MemoryError> {
    let n = query(conn, name)?;
    Ok(n > 0)
}

/// Body of [`migrate_v11_drop_domains_table`], parameterized over the
/// existence-check so tests can inject a deterministic failure without
/// inducing a real SQLite lock/corruption condition (#978). Production
/// behavior and the public signature above are unchanged; this is purely an
/// internal DI seam.
///
/// Propagates existence-check errors (lock/I/O/authorizer) instead of
/// collapsing them to "table absent" — a swallowed error here would let
/// `run_data_migrations` mark this migration's sentinel as run even though
/// the DROP never ran, permanently skipping the retry.
pub(super) fn migrate_v11_inner(
    conn: &Connection,
    exists_query: impl Fn(&Connection, &str) -> Result<i64, rusqlite::Error>,
) -> Result<usize, MemoryError> {
    if exists_with_query(conn, "domains", exists_query)? {
        conn.execute_batch("DROP TABLE domains")?;
        return Ok(1);
    }
    Ok(0)
}
