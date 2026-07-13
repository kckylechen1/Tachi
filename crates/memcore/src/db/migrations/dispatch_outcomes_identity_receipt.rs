//! v16 (#1065) — add the immutable dispatch identity receipt column.

use rusqlite::Connection;

use crate::error::MemoryError;

use super::dispatch_outcomes_reported::table_exists;
use super::legacy_columns::table_has_column;

pub(super) fn migrate_v16_dispatch_outcomes_identity_receipt(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    if !table_exists(conn, "dispatch_outcomes")?
        || table_has_column(conn, "dispatch_outcomes", "identity_receipt")?
    {
        return Ok(0);
    }
    conn.execute(
        "ALTER TABLE dispatch_outcomes ADD COLUMN identity_receipt TEXT",
        [],
    )?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v16_adds_receipt_column_idempotently_and_preserves_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE dispatch_outcomes (
                 outcome_id TEXT PRIMARY KEY,
                 dispatch_id TEXT NOT NULL,
                 execution_outcome TEXT NOT NULL,
                 idempotency_key TEXT NOT NULL
             );
             INSERT INTO dispatch_outcomes
                 (outcome_id, dispatch_id, execution_outcome, idempotency_key)
             VALUES ('o-legacy', 'd-legacy', 'completed', 'd-legacy::_untyped');",
        )
        .unwrap();

        assert_eq!(
            migrate_v16_dispatch_outcomes_identity_receipt(&conn).unwrap(),
            1
        );
        assert!(table_has_column(&conn, "dispatch_outcomes", "identity_receipt").unwrap());
        let legacy_receipt: Option<String> = conn
            .query_row(
                "SELECT identity_receipt FROM dispatch_outcomes WHERE outcome_id = 'o-legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_receipt, None);
        assert_eq!(
            migrate_v16_dispatch_outcomes_identity_receipt(&conn).unwrap(),
            0
        );
    }
}
