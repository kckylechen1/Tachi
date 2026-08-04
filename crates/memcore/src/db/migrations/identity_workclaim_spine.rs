//! v21: additive AgentIdentity / WorkClaim holder-evidence spine (#1253).
//!
//! Existing rows intentionally keep a NULL `agent_identity_id`: deriving one
//! from a session client or dispatch would turn an absence of proof into a
//! false identity assertion.

use rusqlite::{params, Connection};

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v21_identity_workclaim_spine(
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
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_identities (
            agent_identity_id TEXT PRIMARY KEY,
            display_name TEXT, seat TEXT, capability_json TEXT,
            created_at TEXT NOT NULL DEFAULT ''
          );",
    )?;
    migrate_identity_admissions(conn)?;

    let mut added = 0;
    // `lease_expires_at` is retained as caller-declared lease metadata for the
    // v1 API and transition receipts. It is reserved for future direct-expiry
    // enforcement: v1 orphaning is driven by `heartbeat_at` plus the GC TTL.
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

fn migrate_identity_admissions(conn: &Connection) -> Result<(), MemoryError> {
    if !table_exists(conn, "identity_admissions")? {
        conn.execute_batch(
            "CREATE TABLE identity_admissions (
                admission_id TEXT PRIMARY KEY,
                agent_identity_id TEXT,
                connection_id TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('self_asserted', 'verified', 'rejected', 'unavailable')),
                rejection_evidence TEXT,
                created_at TEXT NOT NULL DEFAULT '',
                UNIQUE(agent_identity_id, connection_id)
            );
            CREATE INDEX idx_identity_admissions_connection ON identity_admissions(connection_id);",
        )?;
        return Ok(());
    }

    if column_exists(conn, "identity_admissions", "rejection_evidence")?
        && !column_is_not_null(conn, "identity_admissions", "agent_identity_id")?
    {
        return Ok(());
    }

    conn.execute_batch(
        "CREATE TABLE identity_admissions_v21 (
            admission_id TEXT PRIMARY KEY,
            agent_identity_id TEXT,
            connection_id TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('self_asserted', 'verified', 'rejected', 'unavailable')),
            rejection_evidence TEXT,
            created_at TEXT NOT NULL DEFAULT '',
            UNIQUE(agent_identity_id, connection_id)
        );
        INSERT INTO identity_admissions_v21
            (admission_id, agent_identity_id, connection_id, state, rejection_evidence, created_at)
            SELECT admission_id, agent_identity_id, connection_id, state, NULL, created_at
            FROM identity_admissions;
        DROP TABLE identity_admissions;
        ALTER TABLE identity_admissions_v21 RENAME TO identity_admissions;
        CREATE INDEX idx_identity_admissions_connection ON identity_admissions(connection_id);",
    )?;
    Ok(())
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

fn column_is_not_null(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(row.get::<_, i64>(3)? != 0);
        }
    }
    Ok(false)
}

// admin-gated: these tests exercise the Product-scoped v21 migration against
// `db::session_claims`, which does not exist in a portable build (where this
// migration marks its sentinel without executing).
#[cfg(all(test, feature = "admin"))]
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

        assert_eq!(
            migrate_v21_identity_workclaim_spine(&conn, StoreProfile::TachiFull).unwrap(),
            11
        );
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
        assert_eq!(
            migrate_v21_identity_workclaim_spine(&conn, StoreProfile::TachiFull).unwrap(),
            0
        );
    }

    #[test]
    fn v21_keeps_the_exact_legacy_active_claim_upsert_conflict_target() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_claims (
                claim_id TEXT PRIMARY KEY,
                session_client TEXT,
                issue_ref TEXT,
                flow_id TEXT,
                dispatch_id TEXT,
                branch TEXT NOT NULL DEFAULT '',
                declared_file_scope TEXT,
                state TEXT NOT NULL DEFAULT 'active',
                release_reason TEXT,
                created_at TEXT NOT NULL DEFAULT '',
                heartbeat_at TEXT NOT NULL DEFAULT '',
                released_at TEXT
            );
            CREATE TABLE exec_envs (env_id TEXT PRIMARY KEY);
            CREATE UNIQUE INDEX idx_session_claims_identity_active
            ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
            WHERE state = 'active';",
        )
        .unwrap();

        migrate_v21_identity_workclaim_spine(&conn, StoreProfile::TachiFull).unwrap();
        let first = crate::db::session_claims::upsert_or_heartbeat_claim(
            &mut conn,
            &crate::db::session_claims::NewSessionClaim {
                claim_id: "legacy-claim".into(),
                session_client: Some("legacy-client".into()),
                issue_ref: Some("org/repo#1253".into()),
                flow_id: Some("flow-1".into()),
                dispatch_id: None,
                branch: "lane/legacy".into(),
                declared_file_scope: Some("crates/memcore/src/db/**".into()),
                created_at: "2026-07-18T00:00:00Z".into(),
            },
        )
        .expect("the exact legacy upsert must still resolve after v21");
        assert_eq!(first, "legacy-claim");

        let second = crate::db::session_claims::upsert_or_heartbeat_claim(
            &mut conn,
            &crate::db::session_claims::NewSessionClaim {
                claim_id: "legacy-claim-reenter".into(),
                session_client: Some("legacy-client".into()),
                issue_ref: Some("org/repo#1253".into()),
                flow_id: Some("flow-1".into()),
                dispatch_id: None,
                branch: "lane/legacy-reenter".into(),
                declared_file_scope: Some("crates/memcore/src/db/**".into()),
                created_at: "2026-07-18T00:01:00Z".into(),
            },
        )
        .expect("a migrated DB must heartbeat the existing legacy claim");
        assert_eq!(second, "legacy-claim");
    }

    #[test]
    fn v21_rebuilds_admissions_without_losing_prior_identity_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_claims (claim_id TEXT PRIMARY KEY, state TEXT NOT NULL DEFAULT 'active');
             CREATE TABLE exec_envs (env_id TEXT PRIMARY KEY, state TEXT NOT NULL DEFAULT 'active');
             CREATE TABLE identity_admissions (
                admission_id TEXT PRIMARY KEY,
                agent_identity_id TEXT NOT NULL,
                connection_id TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT '',
                UNIQUE(agent_identity_id, connection_id)
             );
             INSERT INTO identity_admissions
                (admission_id, agent_identity_id, connection_id, state, created_at)
                VALUES ('legacy-admission', 'agent-a', 'connection-a', 'self_asserted', 'then');",
        )
        .unwrap();

        migrate_v21_identity_workclaim_spine(&conn, StoreProfile::TachiFull).unwrap();
        let row: (String, Option<String>) = conn
            .query_row(
                "SELECT connection_id, rejection_evidence FROM identity_admissions WHERE admission_id='legacy-admission'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, "connection-a");
        assert_eq!(row.1, None);
    }
}
