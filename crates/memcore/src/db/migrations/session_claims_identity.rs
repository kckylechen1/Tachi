//! v12: DB-level uniqueness for the `session_claims` identity triple
//! (#1001 round 2, item 2).
//!
//! `upsert_or_heartbeat_claim` (`session_claims.rs`) was read-then-write: it
//! `SELECT`s for an existing active claim matching
//! `(session_client, issue_ref, flow_id)`, then either `UPDATE`s or `INSERT`s
//! outside of any constraint that would catch two concurrent callers racing
//! the same identity into two rows. This migration adds a partial UNIQUE
//! index that makes the same guarantee at the DB level, so a race (or a
//! future caller that bypasses `upsert_or_heartbeat_claim`) fails loudly
//! instead of silently duplicating the row.
//!
//! ## Scope: `state = 'active'` rows only
//!
//! The index is `WHERE state = 'active'` (a SQLite partial index) rather than
//! table-wide, because a released historical row for an identity must never
//! block a fresh active claim for that same identity — `release_claim`
//! deliberately keeps released rows for audit (module doc in
//! `session_claims.rs`), and `upsert_or_heartbeat_claim`'s own `WHERE state =
//! 'active'` clause already scopes its lookup the same way.
//!
//! ## The NULL≠NULL trap
//!
//! `session_client`, `issue_ref`, and `flow_id` are all nullable, and SQLite's
//! default UNIQUE-constraint semantics treat NULL as unequal to any other
//! NULL — so two rows that are both NULL in the same column would NOT
//! collide under a naive `UNIQUE(session_client, issue_ref, flow_id)`, even
//! though `upsert_or_heartbeat_claim`'s `IS ?` lookup (`IS` is SQL's
//! NULL-safe equality) treats them as the same identity and would coalesce
//! them into one heartbeat. To close that gap, the index is built on
//! `COALESCE(col, '')` for each nullable column instead of the raw column,
//! so two same-identity rows with a NULL in the same slot collide exactly
//! the way the application-level upsert already treats them as one identity.
//! (Verified directly against sqlite3 3.51 during implementation: two
//! `flow_id IS NULL` active rows with the same session_client/issue_ref do
//! collide under the COALESCE index and do NOT collide under a raw one.)
//!
//! ## Pre-migration dedupe
//!
//! Before creating the index, any existing duplicate active rows for the
//! same identity (which could exist on a DB written by the pre-#1001-round-2
//! kernel, since the read-then-write upsert never had a DB constraint to
//! prevent them) are collapsed to the single most-recently-heartbeated row;
//! the older duplicates are released (not deleted — same audit-retention
//! discipline as `release_claim`) with `release_reason =
//! 'superseded-by-unique-identity-migration'` so the `CREATE UNIQUE INDEX`
//! below never fails on legacy duplicate data.

use rusqlite::{params, Connection};

use crate::error::MemoryError;

/// Idempotent: dedupes any pre-existing duplicate active claims for the same
/// `(session_client, issue_ref, flow_id)` identity (keeping the newest
/// heartbeat, releasing the rest), then creates the partial UNIQUE index.
/// Returns the number of duplicate rows released.
pub(super) fn migrate_v12_session_claims_unique_identity(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    if !table_exists(conn, "session_claims")? {
        // Nothing to migrate — a DB that has never run the #1001 schema yet
        // will get the index for free when `session_claims` is first created
        // (the DDL in `ddl.rs` already includes it going forward).
        return Ok(0);
    }

    let released = dedupe_duplicate_active_claims(conn)?;

    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_session_claims_identity_active \
         ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), \
         COALESCE(flow_id, '')) WHERE state = 'active';",
    )?;

    Ok(released)
}

/// Guarded, `mode IS NULL`-scoped dedup entry point shared with
/// `init_schema_inner` (#1289).
///
/// `init_schema_inner`'s `MIGRATED_INDEXES_SQL` builds the partial UNIQUE
/// index `idx_session_claims_identity_active` (`WHERE state = 'active' AND
/// mode IS NULL`) on EVERY open, and it runs BEFORE the v12/v21 sentinel
/// migrations. On a legacy DB written by the pre-#1001-round-2 kernel that
/// still carries duplicate active rows for one identity, that `CREATE UNIQUE
/// INDEX` would crash init unless the duplicates are collapsed first — so
/// `init_schema_inner` calls this right before the index build.
///
/// Scoped to `mode IS NULL` (via [`dedupe_duplicate_active_claims`], which
/// applies the predicate because `init_schema_inner` ensures the `mode` column
/// before calling here) to match the index predicate EXACTLY: v21 WorkClaims
/// carry a non-null `mode` and are deliberately allowed to share an identity
/// triple (they use transactional collision semantics, not this legacy presence
/// index — see `identity_workclaim_spine.rs`), so a mode-agnostic dedup running
/// on every startup would wrongly release live v21 claims. Table-exists guarded, so a
/// DB that has never built `session_claims` is a clean no-op; and idempotent
/// on every subsequent startup because once the index exists no second active
/// modeless row for an identity can be inserted, leaving nothing to collapse.
pub(in crate::db) fn dedupe_session_claims_identity_conflicts(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    if !table_exists(conn, "session_claims")? {
        return Ok(0);
    }
    dedupe_duplicate_active_claims(conn)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, MemoryError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Find every group of active rows sharing the same
/// `(session_client, issue_ref, flow_id)` identity (NULL-coalesced the same
/// way the unique index will treat them) that has more than one row, keep
/// the row with the latest `heartbeat_at` (ties broken by `claim_id` so the
/// choice is deterministic), and release every other row in the group with a
/// dedicated `release_reason`. Returns the count of rows released.
///
/// ## Conditional `mode IS NULL` scope (#1289)
///
/// The `mode` predicate is applied ONLY when the `session_claims.mode` column
/// actually exists, because this one function is the single source shared by
/// two callers with different column guarantees:
///
/// - **`init_schema_inner`** (via `dedupe_session_claims_identity_conflicts`)
///   runs its `ensure_column(session_claims, mode)` FIRST, so the column is
///   present. There we scope to `mode IS NULL` to match the partial UNIQUE
///   index predicate (`WHERE state = 'active' AND mode IS NULL`) exactly: v21
///   WorkClaims carry a non-null `mode` and may legitimately share an identity
///   triple (transactional collision semantics, see
///   `identity_workclaim_spine.rs`), so a mode-agnostic dedup would wrongly
///   release live v21 claims.
/// - **standalone `run_data_migrations`** runs v12 (this migration) BEFORE v21
///   adds the `mode` column. On a pre-v21 legacy DB the column is absent, and
///   an unconditional `AND mode IS NULL` would `no such column: mode`-crash the
///   whole migration. There, mode-set rows cannot exist yet (WorkClaims are a
///   v21 concept), so a mode-agnostic dedup of every active duplicate is both
///   safe and correct — and once v21 adds the column + rebuilds the index the
///   `mode IS NULL` predicate takes over on every subsequent startup.
///
/// Building the scope from `column_exists` keeps ONE dedup implementation
/// rather than forking a mode-aware and a mode-blind copy.
fn dedupe_duplicate_active_claims(conn: &Connection) -> Result<usize, MemoryError> {
    let mode_scoped = column_exists(conn, "session_claims", "mode")?;
    let s1_mode = if mode_scoped { " AND mode IS NULL" } else { "" };
    let s2_mode = if mode_scoped {
        " AND s2.mode IS NULL"
    } else {
        ""
    };

    let select_sql = format!(
        "SELECT claim_id FROM session_claims s1
         WHERE state = 'active'{s1_mode}
           AND EXISTS (
             SELECT 1 FROM session_claims s2
             WHERE s2.state = 'active'{s2_mode}
               AND COALESCE(s2.session_client, '') = COALESCE(s1.session_client, '')
               AND COALESCE(s2.issue_ref, '') = COALESCE(s1.issue_ref, '')
               AND COALESCE(s2.flow_id, '') = COALESCE(s1.flow_id, '')
               AND (
                 s2.heartbeat_at > s1.heartbeat_at
                 OR (s2.heartbeat_at = s1.heartbeat_at AND s2.claim_id > s1.claim_id)
               )
           )"
    );
    let mut stmt = conn.prepare(&select_sql)?;
    let stale_claim_ids: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let now = super::super::common::now_utc_iso();
    let update_sql = format!(
        "UPDATE session_claims SET state = 'released', released_at = ?2, \
         release_reason = 'superseded-by-unique-identity-migration' \
         WHERE claim_id = ?1 AND state = 'active'{s1_mode}"
    );
    let mut released = 0usize;
    for claim_id in stale_claim_ids {
        conn.execute(&update_sql, params![claim_id, now])?;
        released += 1;
    }
    Ok(released)
}

/// Does `table.column` exist? Used to make [`dedupe_duplicate_active_claims`]'s
/// `mode` scope conditional so the same code runs on a pre-v21 legacy DB (no
/// `mode` column, standalone `run_data_migrations` path) and a mode-carrying DB
/// (`init_schema_inner` path) without crashing on the former (#1289).
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let found = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec};

    fn open_test_db() -> Connection {
        let _ = crate::db::enable_simple_auto_extension();
        register_sqlite_vec();
        let conn = Connection::open_in_memory().expect("open");
        let _ = try_load_sqlite_vec(&conn);
        init_schema(&conn).expect("init_schema");
        conn
    }

    /// A DB with `session_claims` already present (via the real `init_schema`
    /// DDL, so column shapes stay in lockstep with production) but WITHOUT
    /// the new unique index yet — simulating a pre-existing DB that predates
    /// this migration, the only state in which pre-existing duplicate active
    /// rows for the same identity could actually exist on disk. Tests that
    /// need to seed a duplicate BEFORE the migration creates the index must
    /// use this fixture, not [`open_test_db`] (which already carries the
    /// index via the up-to-date DDL and would reject the duplicate insert
    /// itself, before the migration under test ever runs).
    fn open_test_db_pre_index() -> Connection {
        let conn = open_test_db();
        conn.execute_batch("DROP INDEX IF EXISTS idx_session_claims_identity_active;")
            .expect("drop index to simulate pre-migration DB");
        conn
    }

    /// A pre-v21 `session_claims` shape: the legacy identity/audit columns the
    /// dedup reads and writes, but WITHOUT the v21 `mode` column. This is the
    /// exact on-disk state a standalone `run_data_migrations` presents to v12,
    /// which runs BEFORE v21's `ALTER TABLE ... ADD COLUMN mode`. Built by hand
    /// (not via `init_schema`, whose forward DDL already carries `mode`) so the
    /// column really is absent.
    fn open_pre_v21_session_claims() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE session_claims (
                claim_id             TEXT PRIMARY KEY,
                session_client       TEXT,
                issue_ref            TEXT,
                flow_id              TEXT,
                branch               TEXT NOT NULL DEFAULT '',
                state                TEXT NOT NULL DEFAULT 'active',
                release_reason       TEXT,
                created_at           TEXT NOT NULL DEFAULT '',
                heartbeat_at         TEXT NOT NULL DEFAULT '',
                released_at          TEXT
            );",
        )
        .expect("build pre-v21 session_claims fixture");
        conn
    }

    fn insert_claim(
        conn: &Connection,
        claim_id: &str,
        session_client: Option<&str>,
        issue_ref: Option<&str>,
        flow_id: Option<&str>,
        state: &str,
        heartbeat_at: &str,
    ) {
        conn.execute(
            "INSERT INTO session_claims
             (claim_id, session_client, issue_ref, flow_id, branch, state, created_at, heartbeat_at)
             VALUES (?1, ?2, ?3, ?4, 'feat/x', ?5, ?6, ?6)",
            params![
                claim_id,
                session_client,
                issue_ref,
                flow_id,
                state,
                heartbeat_at
            ],
        )
        .unwrap();
    }

    #[test]
    fn creates_unique_index_on_fresh_schema_noop_dedupe() {
        let conn = open_test_db();
        let released = migrate_v12_session_claims_unique_identity(&conn).unwrap();
        assert_eq!(released, 0, "fresh schema has no duplicates to dedupe");

        // Index must now reject a duplicate active insert for the same
        // identity triple.
        insert_claim(
            &conn,
            "dup-1",
            Some("claude-code"),
            Some("org/repo#1"),
            Some("flow-1"),
            "active",
            "2026-07-12T00:00:00Z",
        );
        let err = conn.execute(
            "INSERT INTO session_claims
             (claim_id, session_client, issue_ref, flow_id, branch, state, created_at, heartbeat_at)
             VALUES ('dup-2', 'claude-code', 'org/repo#1', 'flow-1', 'feat/x', 'active', '2026-07-12T00:01:00Z', '2026-07-12T00:01:00Z')",
            [],
        );
        assert!(
            err.is_err(),
            "duplicate active claim for the same identity must be rejected by the index"
        );
    }

    #[test]
    fn null_flow_id_collides_same_as_a_concrete_value() {
        let conn = open_test_db();
        migrate_v12_session_claims_unique_identity(&conn).unwrap();

        insert_claim(
            &conn,
            "null-1",
            Some("claude-code"),
            Some("org/repo#2"),
            None,
            "active",
            "2026-07-12T00:00:00Z",
        );
        let err = conn.execute(
            "INSERT INTO session_claims
             (claim_id, session_client, issue_ref, flow_id, branch, state, created_at, heartbeat_at)
             VALUES ('null-2', 'claude-code', 'org/repo#2', NULL, 'feat/x', 'active', '2026-07-12T00:01:00Z', '2026-07-12T00:01:00Z')",
            [],
        );
        assert!(
            err.is_err(),
            "two active claims with NULL flow_id but matching session_client+issue_ref must collide (NULL-coalesced identity)"
        );
    }

    #[test]
    fn released_duplicate_does_not_block_fresh_active_claim() {
        let conn = open_test_db();
        insert_claim(
            &conn,
            "old-released",
            Some("claude-code"),
            Some("org/repo#3"),
            Some("flow-3"),
            "released",
            "2026-07-11T00:00:00Z",
        );
        migrate_v12_session_claims_unique_identity(&conn).unwrap();

        conn.execute(
            "INSERT INTO session_claims
             (claim_id, session_client, issue_ref, flow_id, branch, state, created_at, heartbeat_at)
             VALUES ('new-active', 'claude-code', 'org/repo#3', 'flow-3', 'feat/x', 'active', '2026-07-12T00:00:00Z', '2026-07-12T00:00:00Z')",
            [],
        )
        .expect("a released row for the same identity must not block a new active claim");
    }

    #[test]
    fn pre_existing_duplicates_are_deduped_keeping_newest_heartbeat() {
        let conn = open_test_db_pre_index();
        insert_claim(
            &conn,
            "old-dup",
            Some("claude-code"),
            Some("org/repo#4"),
            Some("flow-4"),
            "active",
            "2026-07-11T00:00:00Z",
        );
        insert_claim(
            &conn,
            "new-dup",
            Some("claude-code"),
            Some("org/repo#4"),
            Some("flow-4"),
            "active",
            "2026-07-11T00:10:00Z",
        );

        let released = migrate_v12_session_claims_unique_identity(&conn).unwrap();
        assert_eq!(released, 1, "exactly the older duplicate must be released");

        let old_state: String = conn
            .query_row(
                "SELECT state FROM session_claims WHERE claim_id = 'old-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_state, "released");
        let old_reason: String = conn
            .query_row(
                "SELECT release_reason FROM session_claims WHERE claim_id = 'old-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_reason, "superseded-by-unique-identity-migration");

        let new_state: String = conn
            .query_row(
                "SELECT state FROM session_claims WHERE claim_id = 'new-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_state, "active", "newest heartbeat survives as active");
    }

    #[test]
    fn distinct_identities_are_untouched() {
        let conn = open_test_db();
        insert_claim(
            &conn,
            "a",
            Some("claude-code"),
            Some("org/repo#5"),
            Some("flow-a"),
            "active",
            "2026-07-11T00:00:00Z",
        );
        insert_claim(
            &conn,
            "b",
            Some("codex"),
            Some("org/repo#5"),
            Some("flow-a"),
            "active",
            "2026-07-11T00:00:00Z",
        );

        let released = migrate_v12_session_claims_unique_identity(&conn).unwrap();
        assert_eq!(
            released, 0,
            "different session_client is a different identity"
        );

        let both_active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_claims WHERE state = 'active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(both_active, 2);
    }

    #[test]
    fn v12_dedupes_pre_v21_db_missing_mode_column() {
        // #1289 Claim5: in the standalone `run_data_migrations` path v12 runs
        // BEFORE v21 adds `session_claims.mode`. On a legacy DB carrying
        // duplicate active claims, v12's dedup must NOT reference `mode` or it
        // crashes with `no such column: mode`, aborting the migration. Before
        // the fix, `dedupe_duplicate_active_claims`'s unconditional
        // `AND mode IS NULL` made this RED; after, the scope is conditional on
        // the column existing, so a mode-less DB is deduped mode-agnostically
        // (GREEN) — correct because WorkClaims (the only non-null-mode rows)
        // are a v21 concept and cannot exist yet.
        let conn = open_pre_v21_session_claims();
        insert_claim(
            &conn,
            "old-dup",
            Some("claude-code"),
            Some("org/repo#1289"),
            Some("flow-1"),
            "active",
            "2026-07-11T00:00:00Z",
        );
        insert_claim(
            &conn,
            "new-dup",
            Some("claude-code"),
            Some("org/repo#1289"),
            Some("flow-1"),
            "active",
            "2026-07-11T00:10:00Z",
        );

        let released = migrate_v12_session_claims_unique_identity(&conn).expect(
            "v12 must not crash on a pre-v21 DB missing the mode column (#1289 Claim5)",
        );
        assert_eq!(released, 1, "the older modeless duplicate must be released");

        let old_state: String = conn
            .query_row(
                "SELECT state FROM session_claims WHERE claim_id = 'old-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_state, "released");
        let new_state: String = conn
            .query_row(
                "SELECT state FROM session_claims WHERE claim_id = 'new-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_state, "active", "newest heartbeat survives as active");

        // v12's own index (predicate `WHERE state = 'active'`, no mode ref) must
        // still build on the mode-less table; v21 later rebuilds it with the
        // `mode IS NULL` predicate.
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_session_claims_identity_active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1, "v12 must build the partial unique index");
    }

    #[test]
    fn is_idempotent_on_second_run() {
        let conn = open_test_db_pre_index();
        insert_claim(
            &conn,
            "old",
            Some("claude-code"),
            Some("org/repo#6"),
            Some("flow-6"),
            "active",
            "2026-07-11T00:00:00Z",
        );
        insert_claim(
            &conn,
            "new",
            Some("claude-code"),
            Some("org/repo#6"),
            Some("flow-6"),
            "active",
            "2026-07-11T00:10:00Z",
        );

        let first = migrate_v12_session_claims_unique_identity(&conn).unwrap();
        assert_eq!(first, 1);
        let second = migrate_v12_session_claims_unique_identity(&conn).unwrap();
        assert_eq!(
            second, 0,
            "nothing left to dedupe; index already exists (IF NOT EXISTS)"
        );
    }
}
