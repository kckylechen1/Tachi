//! v15 (#894 S2c) — add the `env_class` column to `exec_envs` on legacy DBs.
//!
//! Fresh DBs already get the column from `ddl.rs`'s `CREATE TABLE` (so this
//! migration no-ops on them via the `table_has_column` guard); DBs whose
//! `exec_envs` was created before S2c need the `ALTER TABLE ... ADD COLUMN`.
//! Idempotent on both the sentinel gate (`was_run`) and the column-presence
//! check — safe to re-run.
//!
//! Legacy rows back-fill to `'edit-only'` (the column default). That is a
//! statement about *provisioning policy*, not a claim that those worktrees have
//! no target dir on disk: a pre-S2c worktree was opened with a shared
//! `.cargo/config.toml` and never had a resource binding at all (the S2a
//! resource ledger is newer than every row here). `edit-only` is the fail-safe
//! value to land on — it claims no `build_target` resource, so nothing in the
//! reclaim path can conclude a legacy lease owns bytes it does not own.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

use super::legacy_columns::table_has_column;

/// Back-fill the `env_class` column onto an existing `exec_envs` table. Returns
/// `1` if the column was added, `0` if it was already present (fresh DB or a
/// re-run).
pub(super) fn migrate_v15_exec_envs_env_class(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    // #1585 D3: product-scoped migration. A PortableKernel store never
    // created the table(s) this touches, so the work is vacuously done.
    // Returning Ok here (rather than skipping the call) is deliberate:
    // `apply_versioned_migration` still marks the sentinel, so a portable
    // database is a COMPLETE stamped-28 database by every existing gate's
    // definition (`validate_current_schema_integrity`,
    // `MIGRATION_SENTINEL_KEYS`) — the sentinel set is profile-invariant.
    if !profile.includes_product() {
        return Ok(0);
    }
    // Guard: a DB that never created the table has nothing to alter — the DDL
    // will create it with the column.
    if !table_exists(conn, "exec_envs")? {
        return Ok(0);
    }
    if table_has_column(conn, "exec_envs", "env_class")? {
        return Ok(0);
    }
    conn.execute(
        "ALTER TABLE exec_envs ADD COLUMN env_class TEXT NOT NULL DEFAULT 'edit-only'",
        [],
    )?;
    Ok(1)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, MemoryError> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [table],
            |_| Ok(()),
        )
        .is_ok();
    Ok(exists)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `exec_envs` table shaped like a pre-S2c DB: no `env_class` column.
    fn legacy_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE exec_envs (
                 env_id    TEXT PRIMARY KEY,
                 kind      TEXT NOT NULL DEFAULT 'worktree',
                 path      TEXT NOT NULL,
                 state     TEXT NOT NULL DEFAULT 'active'
             );
             INSERT INTO exec_envs (env_id, path) VALUES ('env-legacy', '/wt/legacy');",
        )
        .unwrap();
    }

    #[test]
    fn v15_adds_column_idempotently_and_defaults_legacy_rows_to_edit_only() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_table(&conn);
        assert!(!table_has_column(&conn, "exec_envs", "env_class").unwrap());

        let added = migrate_v15_exec_envs_env_class(&conn, StoreProfile::TachiFull).unwrap();
        assert_eq!(added, 1);
        assert!(table_has_column(&conn, "exec_envs", "env_class").unwrap());

        // The legacy row survives and reads back the fail-safe class (claims no
        // build_target resource).
        let class: String = conn
            .query_row(
                "SELECT env_class FROM exec_envs WHERE env_id = 'env-legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(class, "edit-only");

        // Second run is a no-op (idempotent add-column).
        assert_eq!(
            migrate_v15_exec_envs_env_class(&conn, StoreProfile::TachiFull).unwrap(),
            0
        );
    }

    #[test]
    fn v15_noops_when_table_absent() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v15_exec_envs_env_class(&conn, StoreProfile::TachiFull).unwrap(),
            0
        );
    }
}
