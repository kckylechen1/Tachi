//! One-shot, idempotent data migrations for legacy memory DBs.
//!
//! Each migration is gated by a sentinel row in `hard_state`
//! (namespace='migrations', key=<migration_id>). Once written, the migration
//! is skipped on subsequent runs.
//!
//! See docs/audit-2026-04-30.md (PR-3) for the bugs each migration fixes:
//! - v1: H1 path normalization
//! - v2: H4 scope normalization (defense-in-depth on top of PR-1's enum migration)
//! - v3: H6 handoff path standardization
//! - v4: B4/B11 cross-DB pollution quarantine
//! - v5: drop HyperTachi legacy columns (`indexed_tags`, `domain_key`) after bridge
//! - v6: fold non-empty `persons` JSON into `entities`, then clear `persons`
//! - v7: reconcile half-migrated DBs (re-bridge + drop `indexed_tags`/`domain_key`, ensure `location`)
//! - v8: drop the legacy physical `persons` column after folding it into `entities`
//! - v9: relocate non-empty `location` into `path` / metadata, then drop `location`
//! - v10: drop retired skill-pack tables (`packs`, `agent_projections`)
//! - v11: drop retired `domains` registry table (#757)

use std::path::Path;

use rusqlite::Connection;

use crate::error::MemoryError;

use super::common::now_utc_iso;

mod basic;
mod cross_db;
mod domain_retire;
mod legacy_columns;
mod pack_retire;
mod sentinel;

use basic::*;
use cross_db::*;
use domain_retire::*;
use legacy_columns::*;
use pack_retire::*;
pub use legacy_columns::{
    fold_and_drop_legacy_persons_column, migrate_v9_relocate_and_drop_location,
};
use sentinel::*;

const MIGRATION_NS: &str = "migrations";
const SANITY_QUARANTINE_FRACTION: f64 = 0.5;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MigrationReport {
    pub paths_normalized: usize,
    pub scopes_fixed: usize,
    pub handoff_paths_standardized: usize,
    pub quarantined: usize,
    pub quarantine_skipped_sanity_guard: bool,
    pub hypertachi_legacy_columns_dropped: usize,
    pub persons_folded_into_entities: usize,
    pub legacy_columns_reconciled: usize,
    pub persons_columns_dropped: usize,
    pub locations_relocated: usize,
    pub location_columns_dropped: usize,
    pub pack_tables_dropped: usize,
    pub domains_table_dropped: usize,
}

/// Run all data-fix migrations in order. Idempotent.
///
/// `db_label` is the manifest role/project label for this DB ("global",
/// "wiki", a project name, or "unknown"). `current_db_path` is the canonical
/// filesystem path to this DB file (used by v4 to detect rows whose
/// `metadata.provenance.db_path` points elsewhere).
pub fn run_data_migrations(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<MigrationReport, MemoryError> {
    let mut report = MigrationReport::default();

    if !was_run(conn, "v1_path_normalize_legacy")? {
        report.paths_normalized = migrate_v1_path_normalize(conn)?;
        mark_run(conn, "v1_path_normalize_legacy")?;
    }

    if !was_run(conn, "v2_scope_self_normalize")? {
        report.scopes_fixed = migrate_v2_scope_normalize(conn)?;
        mark_run(conn, "v2_scope_self_normalize")?;
    }

    if !was_run(conn, "v3_handoff_path_standardize")? {
        report.handoff_paths_standardized = migrate_v3_handoff_standardize(conn)?;
        mark_run(conn, "v3_handoff_path_standardize")?;
    }

    if !was_run(conn, "v4_quarantine_cross_db_rows")? {
        let (quarantined, skipped) =
            migrate_v4_quarantine_cross_db(conn, db_label, current_db_path)?;
        report.quarantined = quarantined;
        report.quarantine_skipped_sanity_guard = skipped;
        // Even on sanity-guard skip, mark run so we don't loop on every startup.
        mark_run(conn, "v4_quarantine_cross_db_rows")?;
    }

    if !was_run(conn, "v5_drop_hypertachi_legacy_columns")? {
        report.hypertachi_legacy_columns_dropped = migrate_v5_drop_hypertachi_legacy_columns(conn)?;
        mark_run(conn, "v5_drop_hypertachi_legacy_columns")?;
    }

    if !was_run(conn, "v6_fold_persons_into_entities")? {
        report.persons_folded_into_entities = migrate_v6_fold_persons_into_entities(conn)?;
        mark_run(conn, "v6_fold_persons_into_entities")?;
    }

    if !was_run(conn, "v7_reconcile_legacy_memory_columns")? {
        report.legacy_columns_reconciled = migrate_v7_reconcile_legacy_memory_columns(conn)?;
        mark_run(conn, "v7_reconcile_legacy_memory_columns")?;
    }

    if !was_run(conn, "v8_drop_legacy_persons_column")? {
        report.persons_columns_dropped = fold_and_drop_legacy_persons_column(conn)?;
        mark_run(conn, "v8_drop_legacy_persons_column")?;
    }

    if !was_run(conn, "v9_relocate_and_drop_location")? {
        let (relocated, dropped) = migrate_v9_relocate_and_drop_location(conn)?;
        report.locations_relocated = relocated;
        report.location_columns_dropped = dropped;
        mark_run(conn, "v9_relocate_and_drop_location")?;
    }

    if !was_run(conn, "v10_drop_pack_tables")? {
        report.pack_tables_dropped = migrate_v10_drop_pack_tables(conn)?;
        mark_run(conn, "v10_drop_pack_tables")?;
    }

    if !was_run(conn, "v11_drop_domains_table")? {
        report.domains_table_dropped = migrate_v11_drop_domains_table(conn)?;
        mark_run(conn, "v11_drop_domains_table")?;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec};
    use rusqlite::{params, Connection};
    use serde_json::json;

    fn open_test_db() -> (Connection, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let _ = libsimple::enable_auto_extension();
        register_sqlite_vec();
        let conn = Connection::open(tmp.path()).expect("open");
        let _ = try_load_sqlite_vec(&conn);
        init_schema(&conn).expect("init_schema");
        (conn, tmp)
    }

    #[test]
    fn migration_table_has_column_rejects_dynamic_sql_identifiers() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute("CREATE TABLE memories (id TEXT PRIMARY KEY)", [])
            .expect("create minimal table");

        let err = table_has_column(&conn, "memories'); DROP TABLE memories; --", "id")
            .expect_err("dynamic table identifier should be rejected");

        assert!(err.to_string().contains("invalid SQL identifier"));
    }

    fn insert_row(conn: &Connection, id: &str, path: &str, scope: &str, metadata: &str) {
        if table_has_column(conn, "memories", "location").unwrap() {
            conn.execute(
                "INSERT INTO memories
              (id, path, summary, text, importance, timestamp, category, topic,
               keywords, entities, location, source, scope, archived,
               created_at, updated_at, access_count, last_access, revision,
               metadata, retention_policy, domain)
             VALUES (?1, ?2, '', '', 0.5, '2026-01-01T00:00:00Z', 'fact', '',
                     '[]', '[]', '', 'manual', ?3, 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, 1,
                     ?4, NULL, NULL)",
                params![id, path, scope, metadata],
            )
            .unwrap();
        } else {
            conn.execute(
                "INSERT INTO memories
              (id, path, summary, text, importance, timestamp, category, topic,
               keywords, entities, source, scope, archived,
               created_at, updated_at, access_count, last_access, revision,
               metadata, retention_policy, domain)
             VALUES (?1, ?2, '', '', 0.5, '2026-01-01T00:00:00Z', 'fact', '',
                     '[]', '[]', 'manual', ?3, 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, 1,
                     ?4, NULL, NULL)",
                params![id, path, scope, metadata],
            )
            .unwrap();
        }
    }

    #[test]
    fn was_run_mark_run_roundtrip() {
        let (conn, _tmp) = open_test_db();
        assert!(!was_run(&conn, "v1_test").unwrap());
        mark_run(&conn, "v1_test").unwrap();
        assert!(was_run(&conn, "v1_test").unwrap());
        // Idempotent re-mark.
        mark_run(&conn, "v1_test").unwrap();
        assert!(was_run(&conn, "v1_test").unwrap());
    }

    #[test]
    fn v1_normalizes_legacy_paths() {
        let (mut conn, tmp) = open_test_db();
        insert_row(&conn, "a", "/Wiki//foo//", "general", "{}");
        insert_row(&conn, "b", "wiki/Bar", "general", "{}");
        insert_row(&conn, "c", "/handoff/agent", "general", "{}");

        let report = run_data_migrations(&mut conn, "wiki", tmp.path()).unwrap();
        assert_eq!(report.paths_normalized, 2);

        let pa: String = conn
            .query_row("SELECT path FROM memories WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pa, "/wiki/foo");
        let pb: String = conn
            .query_row("SELECT path FROM memories WHERE id='b'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pb, "/wiki/Bar");

        // Idempotent on second run.
        let report2 = run_data_migrations(&mut conn, "wiki", tmp.path()).unwrap();
        assert_eq!(report2.paths_normalized, 0);
    }

    #[test]
    fn v3_standardizes_bare_handoff() {
        let (mut conn, tmp) = open_test_db();
        // PR-1 migration runs on init_schema; it normalizes scope. We insert
        // POST-migration so we satisfy CHECK constraints.
        insert_row(&conn, "h1", "/handoff", "general", "{}");
        insert_row(&conn, "h2", "/handoff/agent-x", "general", "{}");
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.handoff_paths_standardized, 1);
        let p1: String = conn
            .query_row("SELECT path FROM memories WHERE id='h1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(p1, "/handoff/unknown");
    }

    #[test]
    fn v4_quarantines_cross_db_rows() {
        let (mut conn, tmp) = open_test_db();
        // Row whose provenance.db_path points elsewhere → quarantined.
        let other_meta = serde_json::json!({
            "provenance": { "db_path": "/some/other/db.sqlite" }
        })
        .to_string();
        insert_row(&conn, "x", "/hapi/foo", "general", &other_meta);
        // Row with provenance pointing at this DB → kept.
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let self_meta = serde_json::json!({
            "provenance": { "db_path": canonical.display().to_string() }
        })
        .to_string();
        insert_row(&conn, "y", "/hapi/bar", "general", &self_meta);
        // Pad with extra clean rows so the bad row is <50% of total.
        for i in 0..5 {
            insert_row(&conn, &format!("z{i}"), "/hapi/clean", "general", "{}");
        }

        let report = run_data_migrations(&mut conn, "hapi", tmp.path()).unwrap();
        assert_eq!(report.quarantined, 1);
        assert!(!report.quarantine_skipped_sanity_guard);

        let px: String = conn
            .query_row("SELECT path FROM memories WHERE id='x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(px, "/_quarantine/cross-db/hapi/foo");
        let qmeta: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='x'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&qmeta).unwrap();
        assert_eq!(v["quarantine"]["reason"], "cross_db_pollution");
        assert_eq!(v["quarantine"]["original_path"], "/hapi/foo");
    }

    #[test]
    fn v4_sanity_guard_aborts_when_majority_would_move() {
        let (mut conn, tmp) = open_test_db();
        let bad_meta = serde_json::json!({
            "provenance": { "db_path": "/elsewhere.db" }
        })
        .to_string();
        // 3 polluted rows, 1 clean → would move 75% → guard aborts.
        for i in 0..3 {
            insert_row(&conn, &format!("p{i}"), "/proj/foo", "general", &bad_meta);
        }
        insert_row(&conn, "ok", "/proj/clean", "general", "{}");
        let report = run_data_migrations(&mut conn, "proj", tmp.path()).unwrap();
        assert_eq!(report.quarantined, 0);
        assert!(report.quarantine_skipped_sanity_guard);
        // Migration is still marked run so we don't retry every startup.
        assert!(was_run(&conn, "v4_quarantine_cross_db_rows").unwrap());
    }

    #[test]
    fn v7_reconciles_indexed_tags_after_v5_sentinel() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                keywords TEXT NOT NULL DEFAULT '[]',
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]',
                domain TEXT,
                domain_key TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE hard_state (
                namespace TEXT NOT NULL,
                key TEXT NOT NULL,
                value_json TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (namespace, key)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, keywords, indexed_tags, entities) VALUES ('m1', '[]', '[\"rust\"]', '[]')",
            [],
        )
        .unwrap();
        mark_run(&conn, "v5_drop_hypertachi_legacy_columns").unwrap();

        let actions = migrate_v7_reconcile_legacy_memory_columns(&conn).unwrap();
        assert!(actions > 0);
        assert!(!table_has_column(&conn, "memories", "indexed_tags").unwrap());
        assert!(!table_has_column(&conn, "memories", "persons").unwrap());

        let keywords: String = conn
            .query_row("SELECT keywords FROM memories WHERE id='m1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(keywords.contains("rust"));
    }

    #[test]
    fn v6_folds_persons_into_entities() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                persons TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]'
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES (?1, ?2, ?3)",
            params!["m1", r#"["Kyle"]"#, r#"["Sigil"]"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES ('m2', '[]', '[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES (?1, ?2, ?3)",
            params!["m3", r#"["sigil"]"#, r#"["Sigil"]"#],
        )
        .unwrap();

        let folded = migrate_v6_fold_persons_into_entities(&conn).unwrap();
        assert_eq!(folded, 2);

        let (persons, entities): (String, String) = conn
            .query_row(
                "SELECT persons, entities FROM memories WHERE id='m1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(persons, "[]");
        let ents: Vec<String> = serde_json::from_str(&entities).unwrap();
        assert!(ents.iter().any(|e| e == "Kyle"));
        assert!(ents.iter().any(|e| e == "user"));
        assert!(ents.iter().any(|e| e == "Sigil"));

        let duplicate_persons: String = conn
            .query_row("SELECT persons FROM memories WHERE id='m3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(duplicate_persons, "[]");
    }

    #[test]
    fn v5_drops_hypertachi_legacy_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                domain_key TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();
        let dropped = migrate_v5_drop_hypertachi_legacy_columns(&conn).unwrap();
        assert_eq!(dropped, 2);
        assert!(!table_has_column(&conn, "memories", "indexed_tags").unwrap());
        assert!(!table_has_column(&conn, "memories", "domain_key").unwrap());
        assert_eq!(migrate_v5_drop_hypertachi_legacy_columns(&conn).unwrap(), 0);
    }

    #[test]
    fn v9_relocates_path_like_location_and_drops_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                location TEXT NOT NULL DEFAULT '',
                metadata TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m1", "/", "/scratch/hyperion", "{}"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m2", "/notes/x", "/code-review/sigil", "{}"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m3", "/facts/y", "Shanghai", "{}"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m4", "/facts/z", "Paris", r#"["legacy"]"#],
        )
        .unwrap();

        let (relocated, dropped) = migrate_v9_relocate_and_drop_location(&conn).unwrap();
        assert_eq!(relocated, 4);
        assert_eq!(dropped, 1);
        assert!(!table_has_column(&conn, "memories", "location").unwrap());

        let path: String = conn
            .query_row("SELECT path FROM memories WHERE id='m1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "/scratch/hyperion");

        let metadata: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='m2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(meta["context_path"], "/code-review/sigil");

        let metadata: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='m3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(meta["geo"], "Shanghai");

        let metadata: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='m4'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(meta["geo"], "Paris");
        assert_eq!(meta["legacy_metadata"], json!(["legacy"]));
    }

    #[test]
    fn v9_relocates_many_location_rows_in_batches() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                location TEXT NOT NULL DEFAULT '',
                metadata TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();

        let batch = conn.transaction().unwrap();
        {
            let mut stmt = batch
                .prepare(
                    "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, '{}')",
                )
                .unwrap();
            for i in 0..1200 {
                let id = format!("row-{i:04}");
                stmt.execute(params![id, "/", format!("/scratch/batch-{i}")])
                    .unwrap();
            }
        }
        batch.commit().unwrap();

        let (relocated, dropped) = migrate_v9_relocate_and_drop_location(&conn).unwrap();
        assert_eq!(relocated, 1200);
        assert_eq!(dropped, 1);
        assert!(!table_has_column(&conn, "memories", "location").unwrap());

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE trim(path) = '/'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn v9_relocate_location_rows_handles_empty_batch() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                location TEXT NOT NULL DEFAULT '',
                metadata TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();

        let relocated = relocate_location_rows(&conn).unwrap();
        assert_eq!(relocated, 0);
    }

    fn table_present(conn: &Connection, name: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .unwrap();
        n > 0
    }

    #[test]
    fn v10_drops_legacy_pack_tables() {
        let (mut conn, tmp) = open_test_db();
        // init_schema no longer creates the pack tables; emulate a legacy DB
        // that still carries them by creating them manually.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS packs (id TEXT PRIMARY KEY, name TEXT);
             CREATE TABLE IF NOT EXISTS agent_projections (agent TEXT, pack_id TEXT);",
        )
        .unwrap();
        assert!(table_present(&conn, "packs"));
        assert!(table_present(&conn, "agent_projections"));

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.pack_tables_dropped, 2);
        assert!(!table_present(&conn, "packs"));
        assert!(!table_present(&conn, "agent_projections"));

        // Idempotent: re-running is a no-op (sentinel guards it).
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.pack_tables_dropped, 0);
    }

    #[test]
    fn v10_is_a_noop_on_db_without_pack_tables() {
        let (mut conn, tmp) = open_test_db();
        assert!(!table_present(&conn, "packs"));
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.pack_tables_dropped, 0);
        assert!(!table_present(&conn, "packs"));
    }

    #[test]
    fn v11_drops_legacy_domains_table() {
        let (mut conn, tmp) = open_test_db();
        // init_schema no longer creates the `domains` registry table; emulate
        // a legacy DB that still carries it (and a row) by creating it
        // manually.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS domains (
                name TEXT PRIMARY KEY,
                description TEXT NOT NULL DEFAULT ''
            );
            INSERT INTO domains (name, description) VALUES ('legacy', 'old registry row');",
        )
        .unwrap();
        assert!(table_present(&conn, "domains"));

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.domains_table_dropped, 1);
        assert!(!table_present(&conn, "domains"));

        // The live free-text `memories.domain` column is untouched.
        assert!(table_has_column(&conn, "memories", "domain").unwrap());

        // Idempotent: re-running is a no-op (sentinel guards it).
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.domains_table_dropped, 0);
    }

    #[test]
    fn v11_is_a_noop_on_db_without_domains_table() {
        let (mut conn, tmp) = open_test_db();
        assert!(!table_present(&conn, "domains"));
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.domains_table_dropped, 0);
        assert!(!table_present(&conn, "domains"));
    }

    /// Hold `BEGIN EXCLUSIVE` on `db_path` from a second, rollback-journal
    /// connection until `release` fires. While held, any other connection's
    /// read against `sqlite_master` (or any table) with `busy_timeout(0)`
    /// fails immediately with `SQLITE_BUSY` instead of blocking — a
    /// realistic stand-in for the transient lock/I/O/authorizer failure
    /// #978 describes: the existence-check itself errors, rather than
    /// legitimately finding the table absent.
    fn hold_exclusive_lock(
        db_path: std::path::PathBuf,
    ) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // Rollback journal mode (not WAL) so EXCLUSIVE blocks other
            // connections' reads, matching the test DB created without WAL.
            let holder = Connection::open(&db_path).expect("open lock-holder conn");
            holder
                .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
                .expect("acquire exclusive lock");
            // Touch the DB so the lock is actually taken out, not just queued.
            holder
                .execute_batch("CREATE TABLE IF NOT EXISTS __lock_probe(x)")
                .expect("write under exclusive lock");
            ready_tx.send(()).expect("signal ready");
            let _ = release_rx.recv();
            holder.execute_batch("ROLLBACK").expect("release lock");
        });
        ready_rx.recv().expect("wait for lock to be held");
        (handle, release_tx)
    }

    #[test]
    fn v10_propagates_existence_check_error_and_does_not_mark_sentinel() {
        let (conn, tmp) = open_test_db();
        drop(conn); // release our handle so the lock-holder thread can open cleanly

        let (lock_thread, release) = hold_exclusive_lock(tmp.path().to_path_buf());

        let locked = Connection::open(tmp.path()).expect("open under lock");
        locked
            .busy_timeout(std::time::Duration::from_millis(0))
            .expect("set zero busy timeout so the lock fails fast");

        let err = migrate_v10_drop_pack_tables(&locked)
            .expect_err("existence-check error must propagate, not collapse to absent");
        assert!(
            err.to_string().to_lowercase().contains("lock")
                || err.to_string().to_lowercase().contains("busy"),
            "unexpected error shape: {err}"
        );
        drop(locked);

        release.send(()).expect("release lock");
        lock_thread.join().expect("lock-holder thread panicked");

        // Once the lock is released, a fresh connection must show the
        // sentinel was never written — the failed existence-check must not
        // have let `run_data_migrations` reach `mark_run`.
        let healthy = Connection::open(tmp.path()).expect("reopen after lock release");
        assert!(
            !was_run(&healthy, "v10_drop_pack_tables").unwrap(),
            "sentinel must stay unset after a failed existence-check so the migration retries"
        );
    }

    #[test]
    fn v11_propagates_existence_check_error_and_does_not_mark_sentinel() {
        let (conn, tmp) = open_test_db();
        drop(conn);

        let (lock_thread, release) = hold_exclusive_lock(tmp.path().to_path_buf());

        let locked = Connection::open(tmp.path()).expect("open under lock");
        locked
            .busy_timeout(std::time::Duration::from_millis(0))
            .expect("set zero busy timeout so the lock fails fast");

        let err = migrate_v11_drop_domains_table(&locked)
            .expect_err("existence-check error must propagate, not collapse to absent");
        assert!(
            err.to_string().to_lowercase().contains("lock")
                || err.to_string().to_lowercase().contains("busy"),
            "unexpected error shape: {err}"
        );
        drop(locked);

        release.send(()).expect("release lock");
        lock_thread.join().expect("lock-holder thread panicked");

        let healthy = Connection::open(tmp.path()).expect("reopen after lock release");
        assert!(
            !was_run(&healthy, "v11_drop_domains_table").unwrap(),
            "sentinel must stay unset after a failed existence-check so the migration retries"
        );
    }
}
