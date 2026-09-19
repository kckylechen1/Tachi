//! v37: install CurrentTruth's existing schema through the canonical product
//! migration boundary before the production adapter opens its typed handle.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v37_current_truth(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    if !profile.includes_product() {
        return Ok(0);
    }
    crate::db::schema::install_current_truth_schema(conn)?;
    crate::db::schema::validate_current_truth_schema(conn)?;
    // Three tables plus two explicit indexes.
    Ok(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v37_installs_current_truth_only_for_product_stores() {
        let product = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v37_current_truth(&product, StoreProfile::TachiFull).unwrap(),
            5
        );
        crate::db::schema::validate_current_truth_schema(&product).unwrap();

        let portable = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v37_current_truth(&portable, StoreProfile::PortableKernel).unwrap(),
            0
        );
        let count: i64 = portable
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'current_truth_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn v37_validator_refuses_missing_ingestion_uniqueness() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v37_current_truth(&conn, StoreProfile::TachiFull).unwrap();
        conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
        conn.execute(
            "UPDATE sqlite_schema
             SET sql = replace(sql, ?1, '')
             WHERE type = 'table' AND name = 'current_truth_assertions'",
            ["UNIQUE (subject_repo, subject_kind, subject_id, predicate,\n                    authority, issuer, source_id, source_revision)"],
        )
        .unwrap();
        conn.execute_batch("PRAGMA writable_schema = OFF;").unwrap();
        crate::db::schema::validate_current_truth_schema(&conn)
            .expect_err("drifted immutable ingestion key must fail closed");
    }
}
