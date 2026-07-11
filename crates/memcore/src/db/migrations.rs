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
//!
//! ## Schema version stamp (#984)
//!
//! In addition to the sentinel-row idempotency above, the DB carries an
//! integer version stamp in `PRAGMA user_version` (SQLite's canonical slot
//! for this; unused anywhere else in this codebase prior to #984). This is a
//! *coarse* hard-fail gate, orthogonal to the fine-grained sentinel
//! migrations: it exists so a downstream reader (e.g. HyperMem) opening a DB
//! written by a newer kernel fails loudly instead of silently proceeding
//! against data/columns it doesn't understand yet.
//!
//! [`EXPECTED_SCHEMA_VERSION`] counts the migration sequence above: 11
//! sentinel migrations (v1..v11) plus the pre-sentinel baseline schema (v0),
//! so the current stamp is 11. Bump this const (and add a `vN` doc line
//! above) whenever a new migration is appended to [`run_data_migrations`].

use std::path::Path;

use rusqlite::Connection;

use crate::error::MemoryError;

use super::common::now_utc_iso;

/// Current schema version stamp, persisted via `PRAGMA user_version`.
///
/// See the module doc comment ("Schema version stamp (#984)") for what this
/// counts and when to bump it.
pub const EXPECTED_SCHEMA_VERSION: u32 = 11;

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

/// Read the schema version stamp (`PRAGMA user_version`). Absent/fresh DBs
/// read back `0`.
pub fn read_schema_version(conn: &Connection) -> Result<u32, MemoryError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(version.max(0) as u32)
}

/// Persist `version` as the schema version stamp (`PRAGMA user_version`).
///
/// `PRAGMA` statements don't accept bound parameters, so the value is
/// interpolated directly; it is always a `u32` we control (never
/// attacker-controlled input), so this is not a SQL-injection surface.
fn write_schema_version(conn: &Connection, version: u32) -> Result<(), MemoryError> {
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    Ok(())
}

/// Hard-fail gate: refuse to open/operate on a DB stamped with a schema
/// version newer than this kernel supports. Called at the top of
/// [`run_data_migrations`] (i.e. from `init_schema_with_label_mut`'s entry
/// path), before any migration touches the DB.
///
/// - stamped version > `EXPECTED_SCHEMA_VERSION` → hard error, never proceed.
/// - stamped version <= `EXPECTED_SCHEMA_VERSION` (including the `0` fresh/
///   absent case) → caller proceeds to run migrations and re-stamp.
pub fn check_schema_version_gate(conn: &Connection) -> Result<(), MemoryError> {
    let stored = read_schema_version(conn)?;
    if stored > EXPECTED_SCHEMA_VERSION {
        return Err(MemoryError::InvalidArg(format!(
            "db schema version {stored} newer than supported {EXPECTED_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

/// Run all data-fix migrations in order. Idempotent.
///
/// `db_label` is the manifest role/project label for this DB ("global",
/// "wiki", a project name, or "unknown"). `current_db_path` is the canonical
/// filesystem path to this DB file (used by v4 to detect rows whose
/// `metadata.provenance.db_path` points elsewhere).
///
/// Enforces the [`EXPECTED_SCHEMA_VERSION`] hard-fail gate on entry (a stored
/// version newer than this kernel supports errors out before any migration
/// runs) and stamps the current version on successful exit — fresh DBs
/// (version 0/absent), DBs at an older version, and DBs already at the
/// current version all end this call stamped at `EXPECTED_SCHEMA_VERSION`.
///
/// ## Transactional compatibility boundary (#984 F1)
///
/// The migration effects (sentinel writes and the final `user_version` stamp
/// included) all run inside a single `BEGIN IMMEDIATE` transaction opened
/// here and committed only after every migration and the version stamp have
/// succeeded. A crash or error partway through rolls the *entire* call back
/// — there is no window where the DB is left partially migrated but still
/// carrying an old/zero version stamp (which would let an older kernel pass
/// [`check_schema_version_gate`] against data it doesn't understand).
/// `PRAGMA user_version` is a page in the database header and is journaled
/// like any other write, so it participates in the same rollback as the
/// schema/data changes (see `stamp_and_migration_effects_roll_back_together`
/// in the test module for a fault-injection proof).
pub fn run_data_migrations(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<MigrationReport, MemoryError> {
    check_schema_version_gate(conn)?;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let report = run_data_migrations_in_tx(&tx, db_label, current_db_path)?;
    write_schema_version(&tx, EXPECTED_SCHEMA_VERSION)?;
    tx.commit()?;

    Ok(report)
}

/// The actual migration sequence, run against an already-open transaction.
/// Split out from [`run_data_migrations`] so tests can inject a failure
/// between individual migration steps and the final stamp while still
/// exercising the real transaction boundary.
fn run_data_migrations_in_tx(
    conn: &Connection,
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

    // --- #984: schema version stamp / hard-fail gate ---------------------

    #[test]
    fn schema_version_gate_errors_on_db_stamped_newer_than_supported() {
        let (conn, _tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 1).unwrap();

        let err = check_schema_version_gate(&conn).expect_err("newer stamp must hard-fail");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!(
                "db schema version {} newer than supported {}",
                EXPECTED_SCHEMA_VERSION + 1,
                EXPECTED_SCHEMA_VERSION
            )),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn fresh_db_migrates_and_ends_stamped_at_expected_version() {
        let (mut conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn older_stamped_db_migrates_forward_and_re_stamps() {
        let (mut conn, tmp) = open_test_db();
        // A DB with an explicit older-than-current stamp (but no legacy data
        // and no sentinels — see `unstamped_db_with_existing_sentinels_skips_and_restamps`
        // below for the genuine "already migrated by an older kernel" case)
        // must still migrate forward (no-op, nothing to touch) and re-stamp.
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        // No legacy tables/rows to touch on a freshly-initialized DB, so the
        // report is all-zero; the assertion under test is the re-stamp.
        assert_eq!(report.domains_table_dropped, 0);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    /// #984 F3(a): a genuine fixture for "DB last migrated by an older
    /// kernel" — sentinel rows actually exist (inserted directly via
    /// `mark_run`, the same mechanism the migrations themselves use) for
    /// every migration, and the version stamp is left at 0 (as it would be
    /// for any real DB written before #984 introduced the stamp). Unlike
    /// `older_stamped_db_migrates_forward_and_re_stamps`, this proves the
    /// sentinel-skip path itself, not just "nothing to migrate on a fresh DB".
    #[test]
    fn unstamped_db_with_existing_sentinels_skips_and_restamps() {
        let (mut conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        for key in ALL_MIGRATION_SENTINEL_KEYS {
            assert!(!was_run(&conn, key).unwrap(), "sentinel {key} pre-seeded?");
            mark_run(&conn, key).unwrap();
        }

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        // Every migration was already marked run, so every counted field is
        // zero — the sentinels actually skipped the work, not "there was no
        // work regardless".
        assert_eq!(report.paths_normalized, 0);
        assert_eq!(report.scopes_fixed, 0);
        assert_eq!(report.handoff_paths_standardized, 0);
        assert_eq!(report.quarantined, 0);
        assert_eq!(report.hypertachi_legacy_columns_dropped, 0);
        assert_eq!(report.persons_folded_into_entities, 0);
        assert_eq!(report.legacy_columns_reconciled, 0);
        assert_eq!(report.persons_columns_dropped, 0);
        assert_eq!(report.locations_relocated, 0);
        assert_eq!(report.location_columns_dropped, 0);
        assert_eq!(report.pack_tables_dropped, 0);
        assert_eq!(report.domains_table_dropped, 0);

        // Sentinels skipped the data work, but the version stamp — which is
        // independent of the sentinel mechanism — still advances.
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn gate_is_checked_before_any_migration_runs() {
        let (mut conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 5).unwrap();

        // init_schema_with_label_mut (the real entry point) must refuse too.
        let result = crate::db::init_schema_with_label_mut(&mut conn, "global", tmp.path());
        assert!(
            result.is_err(),
            "gate must reject via the schema.rs entry point too"
        );
    }

    // --- #984 F2: read-only opens are gated too ---------------------------

    #[test]
    fn read_only_open_rejects_db_stamped_newer_than_supported() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let _ = libsimple::enable_auto_extension();
            register_sqlite_vec();
            let conn = Connection::open(tmp.path()).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema(&conn).expect("init_schema");
            write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 1).unwrap();
        }

        let path = tmp.path().to_str().expect("utf8 tmp path");
        let err = crate::MemoryStore::open_read_only(path)
            .expect_err("read-only open of a newer-stamped DB must hard-fail");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!(
                "db schema version {} newer than supported {}",
                EXPECTED_SCHEMA_VERSION + 1,
                EXPECTED_SCHEMA_VERSION
            )),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn read_only_open_permits_older_stamped_db() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let _ = libsimple::enable_auto_extension();
            register_sqlite_vec();
            let conn = Connection::open(tmp.path()).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema(&conn).expect("init_schema");
            write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        }

        let path = tmp.path().to_str().expect("utf8 tmp path");
        // Read-only opens never migrate; an older-stamped DB must still be
        // readable (only NEWER-than-supported is fatal).
        crate::MemoryStore::open_read_only(path)
            .expect("read-only open of an older-stamped DB must succeed");
    }

    #[test]
    fn read_only_open_permits_db_at_current_version() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let _ = libsimple::enable_auto_extension();
            register_sqlite_vec();
            let conn = Connection::open(tmp.path()).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema(&conn).expect("init_schema");
            write_schema_version(&conn, EXPECTED_SCHEMA_VERSION).unwrap();
        }

        let path = tmp.path().to_str().expect("utf8 tmp path");
        crate::MemoryStore::open_read_only(path)
            .expect("read-only open at the current version must succeed");
    }

    // --- #984 F1: transactional compatibility boundary ---------------------

    /// Fault-injection proof that the migration effects, sentinel writes, and
    /// final `user_version` stamp are one atomic unit: force a failure after
    /// several migrations have run (and been marked) but before the final
    /// stamp, and assert BOTH the schema/data changes and the stamp are
    /// rolled back together — not just the stamp.
    #[test]
    fn stamp_and_migration_effects_roll_back_together() {
        let (mut conn, tmp) = open_test_db();
        // Seed a legacy `packs` table so v10 has real, observable work to do
        // (and roll back) rather than being a no-op.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS packs (id TEXT PRIMARY KEY, name TEXT);")
            .unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        // Run migrations for real up through a known point, then simulate a
        // crash by rolling back the outer transaction ourselves instead of
        // letting `run_data_migrations` commit — this stands in for "the
        // process dies between the migrations and the final PRAGMA write",
        // which we cannot deterministically inject through the public API
        // without a fault-injection connection wrapper. What this proves:
        // the migration effects (sentinel rows, `packs` table drop) and the
        // version stamp live in the SAME transaction, so any rollback of
        // that transaction — crash or otherwise — takes both together. If
        // they were separate autocommit statements (the pre-fix behavior),
        // this rollback would be a no-op on already-committed sub-steps.
        {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            let report = run_data_migrations_in_tx(&tx, "global", tmp.path()).unwrap();
            assert_eq!(report.pack_tables_dropped, 1, "v10 should have real work here");
            write_schema_version(&tx, EXPECTED_SCHEMA_VERSION).unwrap();
            // Do NOT commit — roll back instead, simulating the crash.
            tx.rollback().unwrap();
        }

        // Both the data effect (packs table still present) and the sentinel
        // (v10 not marked run) and the version stamp (still 0) must have
        // rolled back together.
        assert_eq!(
            read_schema_version(&conn).unwrap(),
            0,
            "version stamp must roll back with the migration effects"
        );
        assert!(
            !was_run(&conn, "v10_drop_pack_tables").unwrap(),
            "sentinel must roll back with the migration effects"
        );
        let packs_still_present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='packs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            packs_still_present, 1,
            "packs table drop must roll back with the version stamp"
        );

        // And a real run (commit path) now proceeds cleanly from scratch.
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.pack_tables_dropped, 1);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    // --- #984 F3(e): EXPECTED_SCHEMA_VERSION invariant ----------------------

    /// All sentinel keys `run_data_migrations` gates on, in the same order
    /// the runner checks them. Shared by the "genuine existing sentinels"
    /// fixture above and the invariant test below so both stay in lockstep
    /// with the runner's actual migration list.
    const ALL_MIGRATION_SENTINEL_KEYS: &[&str] = &[
        "v1_path_normalize_legacy",
        "v2_scope_self_normalize",
        "v3_handoff_path_standardize",
        "v4_quarantine_cross_db_rows",
        "v5_drop_hypertachi_legacy_columns",
        "v6_fold_persons_into_entities",
        "v7_reconcile_legacy_memory_columns",
        "v8_drop_legacy_persons_column",
        "v9_relocate_and_drop_location",
        "v10_drop_pack_tables",
        "v11_drop_domains_table",
    ];

    /// Ties `EXPECTED_SCHEMA_VERSION` to the migration count the runner
    /// *itself* produces — not a hand-maintained duplicate list — by running
    /// the real `run_data_migrations` against a fresh DB and counting the
    /// sentinel rows it actually wrote to `hard_state`. Appending a
    /// `v12_...` migration to `run_data_migrations_in_tx` (with its own
    /// `mark_run` call, as every migration above does) increases this count
    /// automatically; forgetting to bump `EXPECTED_SCHEMA_VERSION` to match
    /// then fails this test — silently under-stamping newly-migrated DBs
    /// would otherwise defeat the #984 gate for the new migration.
    ///
    /// `ALL_MIGRATION_SENTINEL_KEYS` above is a separate, hand-maintained
    /// list used only to seed the "genuine existing sentinels" fixture; this
    /// test intentionally does not depend on it being complete or in sync.
    #[test]
    fn expected_schema_version_matches_migration_count() {
        let (mut conn, tmp) = open_test_db();

        run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        let sentinel_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace = ?1",
                params![MIGRATION_NS],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(
            sentinel_count, EXPECTED_SCHEMA_VERSION,
            "EXPECTED_SCHEMA_VERSION ({EXPECTED_SCHEMA_VERSION}) must equal the number of \
             sentinel migrations run_data_migrations actually marks run ({sentinel_count}) — \
             bump the const (and add a vN doc line) when a new migration is appended"
        );
    }
}
