//! v16 (#1065) — add the immutable dispatch identity receipt column.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

use super::dispatch_outcomes_reported::table_exists;
use super::legacy_columns::table_has_column;

pub(super) fn migrate_v16_dispatch_outcomes_identity_receipt(
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
            migrate_v16_dispatch_outcomes_identity_receipt(&conn, StoreProfile::TachiFull).unwrap(),
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
            migrate_v16_dispatch_outcomes_identity_receipt(&conn, StoreProfile::TachiFull).unwrap(),
            0
        );
    }
}
