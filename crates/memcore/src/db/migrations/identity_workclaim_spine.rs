//! v40: WorkClaim project binding (`session_claims.canonical_project`).
//!
//! New claims may carry a server-admitted project. Historical rows remain
//! unbound: neither a worktree path nor an issue ref establishes ownership.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v40_work_claim_project_binding(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    if !profile.includes_product() || column_exists(conn, "session_claims", "canonical_project")? {
        return Ok(0);
    }
    conn.execute_batch("ALTER TABLE session_claims ADD COLUMN canonical_project TEXT")?;
    Ok(1)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let found = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    Ok(found)
}

#[cfg(all(test, feature = "admin"))]
mod tests {
    use super::*;

    #[test]
    fn v40_is_product_scoped_and_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_claims (claim_id TEXT PRIMARY KEY, state TEXT NOT NULL DEFAULT 'active');
             INSERT INTO session_claims (claim_id, state) VALUES ('legacy-claim', 'active');",
        )
        .unwrap();

        assert_eq!(
            migrate_v40_work_claim_project_binding(&conn, StoreProfile::PortableKernel).unwrap(),
            0
        );
        assert!(!column_exists(&conn, "session_claims", "canonical_project").unwrap());

        assert_eq!(
            migrate_v40_work_claim_project_binding(&conn, StoreProfile::TachiFull).unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT canonical_project FROM session_claims WHERE claim_id='legacy-claim'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap(),
            None,
            "migration must leave historical claim rows unbound"
        );
        assert_eq!(
            migrate_v40_work_claim_project_binding(&conn, StoreProfile::TachiFull).unwrap(),
            0
        );
    }
}
