//! v37: immutable verified AgentIdentity admission receipts (#1938).

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v37_verified_agent_admissions(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    if !profile.includes_product() {
        return Ok(0);
    }
    crate::db::verified_admissions::install_verified_admission_schema(conn)?;
    crate::db::verified_admissions::validate_verified_admission_schema(conn)?;
    // Two tables, three indexes, and nine canonical append-only triggers.
    Ok(14)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v37_is_product_scoped_and_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE agent_identities (
                 agent_identity_id TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE identity_admissions (
                 admission_id TEXT PRIMARY KEY,
                 agent_identity_id TEXT,
                 connection_id TEXT NOT NULL,
                 state TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(agent_identity_id, connection_id)
             );",
        )
        .unwrap();

        assert_eq!(
            migrate_v37_verified_agent_admissions(&conn, StoreProfile::PortableKernel).unwrap(),
            0
        );
        let absent: bool = conn
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='identity_admission_verification_receipts')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(absent);

        assert_eq!(
            migrate_v37_verified_agent_admissions(&conn, StoreProfile::TachiFull).unwrap(),
            14
        );
        assert_eq!(
            migrate_v37_verified_agent_admissions(&conn, StoreProfile::TachiFull).unwrap(),
            14,
            "DDL replay must preserve the canonical shape"
        );
        crate::db::verified_admissions::validate_verified_admission_schema(&conn)
            .expect("the complete migration fixture must satisfy current validation");
    }

    #[test]
    fn v37_refuses_reordered_identity_admission_conflict_target() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE agent_identities (
                 agent_identity_id TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE identity_admissions (
                 admission_id TEXT PRIMARY KEY,
                 agent_identity_id TEXT,
                 connection_id TEXT NOT NULL,
                 state TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(connection_id, agent_identity_id)
             );",
        )
        .unwrap();

        let error = migrate_v37_verified_agent_admissions(&conn, StoreProfile::TachiFull)
            .expect_err("migration must reject a reordered historical conflict target");
        assert!(
            error
                .to_string()
                .contains("unexpected unique conflict targets"),
            "{error}"
        );
    }
}
