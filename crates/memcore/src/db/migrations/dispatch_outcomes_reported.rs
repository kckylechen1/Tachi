//! v14 (#773 Layer-2 ②) — add the `reported_outcome` column to
//! `dispatch_outcomes` on legacy DBs.
//!
//! Fresh DBs already get the column from `ddl.rs`'s `CREATE TABLE` (so this
//! migration no-ops on them via the `table_has_column` guard); existing DBs
//! whose `dispatch_outcomes` was created before this column need the
//! `ALTER TABLE ... ADD COLUMN`. Idempotent on both the sentinel gate
//! (`was_run`) and the column-presence check — safe to re-run.

use rusqlite::Connection;

use crate::error::MemoryError;

use super::legacy_columns::table_has_column;

/// Back-fill the `reported_outcome` column onto an existing `dispatch_outcomes`
/// table. Returns `1` if the column was added, `0` if it was already present
/// (fresh DB or a re-run). No data back-fill is needed: legacy rows have no
/// recoverable self-report, so the new column is legitimately NULL for them.
pub(super) fn migrate_v14_dispatch_outcomes_reported_outcome(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    // Guard: if the table doesn't exist yet (a DB that never created it),
    // there is nothing to alter — the DDL will create it with the column.
    if !table_exists(conn, "dispatch_outcomes")? {
        return Ok(0);
    }
    if table_has_column(conn, "dispatch_outcomes", "reported_outcome")? {
        return Ok(0);
    }
    conn.execute(
        "ALTER TABLE dispatch_outcomes ADD COLUMN reported_outcome TEXT",
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

    /// A `dispatch_outcomes` table shaped like a pre-#773 (v12) DB: no
    /// `reported_outcome` column. Only the columns the migration cares about
    /// are needed.
    fn legacy_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE dispatch_outcomes (
                 outcome_id        TEXT PRIMARY KEY,
                 dispatch_id       TEXT NOT NULL DEFAULT '',
                 execution_outcome TEXT NOT NULL,
                 idempotency_key   TEXT NOT NULL
             );
             INSERT INTO dispatch_outcomes
                 (outcome_id, dispatch_id, execution_outcome, idempotency_key)
             VALUES ('o-legacy', 'd-legacy', 'success', 'd-legacy::_untyped');",
        )
        .unwrap();
    }

    #[test]
    fn v14_adds_column_idempotently_and_preserves_rows() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_table(&conn);
        assert!(!table_has_column(&conn, "dispatch_outcomes", "reported_outcome").unwrap());

        // First run adds the column and reports one alteration; the legacy row
        // survives with the new column reading NULL (no recoverable report).
        let added = migrate_v14_dispatch_outcomes_reported_outcome(&conn).unwrap();
        assert_eq!(added, 1);
        assert!(table_has_column(&conn, "dispatch_outcomes", "reported_outcome").unwrap());
        let reported: Option<String> = conn
            .query_row(
                "SELECT reported_outcome FROM dispatch_outcomes WHERE outcome_id = 'o-legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reported, None, "legacy row keeps a NULL self-report");

        // Second run is a no-op (idempotent add-column): column already present.
        let again = migrate_v14_dispatch_outcomes_reported_outcome(&conn).unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn v14_noops_when_table_absent() {
        let conn = Connection::open_in_memory().unwrap();
        // No dispatch_outcomes table at all — nothing to alter.
        assert_eq!(
            migrate_v14_dispatch_outcomes_reported_outcome(&conn).unwrap(),
            0
        );
    }
}
