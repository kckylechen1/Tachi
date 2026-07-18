//! v21: additive AgentIdentity / WorkClaim holder-evidence spine (#1253).
//!
//! Existing rows intentionally keep a NULL `agent_identity_id`: deriving one
//! from a session client or dispatch would turn an absence of proof into a
//! false identity assertion.

use rusqlite::{params, Connection};

use crate::error::MemoryError;

pub(super) fn migrate_v21_identity_workclaim_spine(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_identities (
            agent_identity_id TEXT PRIMARY KEY,
            display_name TEXT, seat TEXT, capability_json TEXT,
            created_at TEXT NOT NULL DEFAULT ''
          );
          CREATE TABLE IF NOT EXISTS identity_admissions (
            admission_id TEXT PRIMARY KEY,
            agent_identity_id TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('self_asserted', 'verified', 'rejected', 'unavailable')),
            created_at TEXT NOT NULL DEFAULT '',
            UNIQUE(agent_identity_id, connection_id)
          );
          CREATE INDEX IF NOT EXISTS idx_identity_admissions_connection ON identity_admissions(connection_id);",
    )?;

    let mut added = 0;
    for (table, column, ddl) in [
        ("session_claims", "agent_identity_id", "TEXT"),
        ("session_claims", "worktree_path", "TEXT"),
        ("session_claims", "role", "TEXT"),
        ("session_claims", "mode", "TEXT"),
        ("session_claims", "expected_head", "TEXT"),
        ("session_claims", "lease_expires_at", "TEXT"),
        (
            "session_claims",
            "transition_version",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        ("session_claims", "exec_env_id", "TEXT"),
        ("session_claims", "orphaned_at", "TEXT"),
        ("exec_envs", "agent_identity_id", "TEXT"),
        ("exec_envs", "claim_id", "TEXT"),
    ] {
        if table_exists(conn, table)? && !column_exists(conn, table, column)? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"))?;
            added += 1;
        }
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_exec_envs_claim ON exec_envs(claim_id);")?;
    let has_legacy_identity_key = if table_exists(conn, "session_claims")? {
        ["session_client", "issue_ref", "flow_id"]
            .iter()
            .try_fold(true, |_, column| {
                column_exists(conn, "session_claims", column)
            })?
    } else {
        false
    };
    if has_legacy_identity_key {
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_session_claims_identity_active;
             CREATE UNIQUE INDEX idx_session_claims_identity_active
             ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
             WHERE state = 'active' AND mode IS NULL;",
        )?;
    }
    Ok(added)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, MemoryError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        params![table],
        |row| row.get(0),
    )?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v21_is_additive_and_preserves_legacy_rows_without_fabricating_identity() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_claims (claim_id TEXT PRIMARY KEY, state TEXT NOT NULL DEFAULT 'active');
             CREATE TABLE exec_envs (env_id TEXT PRIMARY KEY, state TEXT NOT NULL DEFAULT 'active');
             INSERT INTO session_claims (claim_id, state) VALUES ('legacy-claim', 'active');
             INSERT INTO exec_envs (env_id, state) VALUES ('legacy-env', 'active');",
        )
        .unwrap();

        assert_eq!(migrate_v21_identity_workclaim_spine(&conn).unwrap(), 11);
        assert_eq!(
            conn.query_row(
                "SELECT agent_identity_id FROM session_claims WHERE claim_id='legacy-claim'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap(),
            None,
            "migration must not infer an identity from a legacy claim"
        );
        assert_eq!(
            conn.query_row(
                "SELECT claim_id FROM exec_envs WHERE env_id='legacy-env'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap(),
            None,
            "migration must leave legacy ExecEnv holder evidence absent"
        );
        assert_eq!(migrate_v21_identity_workclaim_spine(&conn).unwrap(), 0);
    }
}
