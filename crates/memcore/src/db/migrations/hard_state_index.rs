use rusqlite::Connection;

use crate::error::MemoryError;

/// Add `idx_hard_state_ns_updated` on `hard_state(namespace, updated_at DESC)`.
///
/// `list_state` (see `db/state.rs`) queries `WHERE namespace = ? ORDER BY
/// updated_at DESC, key ASC`. Without this index, SQLite can only use the
/// existing `(namespace, key)` primary-key index to satisfy the `WHERE`,
/// then falls back to a full temp B-tree sort for `ORDER BY updated_at DESC`
/// (see `EXPLAIN QUERY PLAN` in the perf-pack evidence). With this index in
/// place the same query plan collapses to "USE INDEX ... ORDER BY" with the
/// `updated_at` term satisfied directly off the index — no in-memory sort —
/// for the common single-namespace case (`ORDER BY updated_at DESC` is the
/// index's natural order; the secondary `key ASC` tiebreak still needs a
/// small sort only among rows sharing the same `updated_at`, which in
/// practice is rare).
///
/// `CREATE INDEX` is DDL and (unlike `journal_mode`) fine to run inside a
/// transaction, so — unlike the read-only-connection PRAGMAs in
/// `db/open.rs` — this runs as a normal sentinel-gated data migration next
/// to v10/v11, not as a connection-level PRAGMA.
///
/// Idempotent: `CREATE INDEX IF NOT EXISTS` is a no-op if the index already
/// exists (e.g. a fresh DB created after this migration was added, where
/// `ddl.rs`'s `MIGRATED_INDEXES_SQL`/base schema might already carry it),
/// and this migration is additionally skipped entirely once the
/// `v12_hard_state_ns_updated_index` sentinel is set.
pub(super) fn migrate_v12_add_hard_state_index(conn: &Connection) -> Result<usize, MemoryError> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_hard_state_ns_updated \
         ON hard_state(namespace, updated_at DESC)",
    )?;
    Ok(1)
}
