use rusqlite::Connection;

use crate::error::MemoryError;

/// Drop the retired `packs` and `agent_projections` tables left over from the
/// skill-pack system. Both were originally created with
/// `CREATE TABLE IF NOT EXISTS`, so long-lived DBs may still hold now-unused
/// copies after the pack subsystem is removed from the code.
///
/// Idempotent: a no-op on DBs that never created the tables, and skipped
/// entirely once the `v10_drop_pack_tables` sentinel is set.
///
/// Returns the number of tables actually dropped (0, 1, or 2).
pub(super) fn migrate_v10_drop_pack_tables(conn: &Connection) -> Result<usize, MemoryError> {
    migrate_v10_inner(conn, real_exists_query)
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

/// Body of [`migrate_v10_drop_pack_tables`], parameterized over the
/// existence-check so tests can inject a deterministic failure without
/// inducing a real SQLite lock/corruption condition (#978). Production
/// behavior and the public signature above are unchanged; this is purely an
/// internal DI seam.
///
/// Propagates existence-check errors (lock/I/O/authorizer) instead of
/// collapsing them to "table absent" — a swallowed error here would let
/// `run_data_migrations` mark this migration's sentinel as run even though
/// the DROP never ran, permanently skipping the retry.
pub(super) fn migrate_v10_inner(
    conn: &Connection,
    exists_query: impl Fn(&Connection, &str) -> Result<i64, rusqlite::Error> + Copy,
) -> Result<usize, MemoryError> {
    let mut dropped = 0usize;
    if exists_with_query(conn, "packs", exists_query)? {
        conn.execute_batch("DROP TABLE packs")?;
        dropped += 1;
    }
    if exists_with_query(conn, "agent_projections", exists_query)? {
        conn.execute_batch("DROP TABLE agent_projections")?;
        dropped += 1;
    }
    Ok(dropped)
}
