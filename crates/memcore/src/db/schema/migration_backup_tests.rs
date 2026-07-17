use super::*;
use rusqlite::Connection;

#[test]
fn open_without_label_also_backs_up_before_migration() {
    // CP1 regression guard: MemoryStore::open goes through the
    // path_validation=false branch of open_with_label_inner. Before the
    // fix that branch called init_schema directly, skipping backup for
    // every CLI / open_cli_store path. Now both branches route through
    // init_schema_with_label_mut. This test fails (no backup file) if
    // the branches are ever split again.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("cp1.db");

    {
        let conn = Connection::open(&db_path).expect("open");
        conn.execute_batch("CREATE TABLE legacy(x)")
            .expect("create");
    }
    std::fs::write(migration_marker_path(&db_path), "0.0.0:0").expect("stale marker");

    let _store = crate::MemoryStore::open(db_path.to_str().expect("path")).expect("open");

    let backup_exists = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains("migration-bak"));
    assert!(
        backup_exists,
        "MemoryStore::open (path_validation=false) must create a backup before migrating"
    );
}

#[test]
fn backup_skipped_for_fresh_db() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("fresh.db");
    let conn = Connection::open(&db_path).expect("open");

    let result = maybe_backup_before_migration(&conn, &db_path).expect("backup check");
    assert!(
        result.is_none(),
        "fresh DB (schema_version=0) should not be backed up"
    );

    let bak_count = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("migration-bak"))
        .count();
    assert_eq!(bak_count, 0);
}

#[test]
fn backup_created_on_fingerprint_mismatch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("existing.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch("CREATE TABLE t(x)")
        .expect("create table");
    std::fs::write(migration_marker_path(&db_path), "0.0.0:0").expect("stale marker");

    let result = maybe_backup_before_migration(&conn, &db_path).expect("backup check");
    let backup_path = result.expect("mismatched marker should trigger backup");
    assert!(backup_path.exists());
    assert!(
        backup_path.to_string_lossy().contains("migration-bak"),
        "backup filename should contain migration-bak"
    );
}

#[test]
fn backup_skipped_when_marker_matches() {
    // Codex review (2026-07-17, checkpoint 1): the ordinary skip path this
    // guards is a plain same-version reopen (`stored == EXPECTED_SCHEMA_VERSION`),
    // not an unstamped `stored == 0` DB — that shape is already covered
    // separately by `backup_skipped_for_fresh_db`. Stamp `user_version` to
    // EXPECTED_SCHEMA_VERSION so this test represents a real "already fully
    // migrated, restarting at the same version" daemon restart, matching
    // what a genuine post-migration DB looks like (see migrations.rs:
    // successful migrations end stamped at EXPECTED_SCHEMA_VERSION).
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("matched.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch("CREATE TABLE t(x)")
        .expect("create table");
    conn.execute_batch(&format!(
        "PRAGMA user_version = {}",
        crate::db::migrations::EXPECTED_SCHEMA_VERSION
    ))
    .expect("stamp current schema version");

    let fp = migration_schema_fingerprint(&conn).expect("fingerprint");
    std::fs::write(migration_marker_path(&db_path), fp).expect("write marker");

    let result = maybe_backup_before_migration(&conn, &db_path).expect("backup check");
    assert!(
        result.is_none(),
        "matching marker on an ordinary same-version restart (stored == EXPECTED) should skip backup"
    );
}

#[test]
fn backup_still_created_when_marker_matches_but_version_migration_is_pending() {
    // #1180: a matching fingerprint must NOT suppress a REAL, authorized
    // schema-version migration's backup. This reproduces the shape of the
    // 2026-07-17 incident without any daemon/CLI distinction: `PRAGMA
    // schema_version` is unaffected by rolling `PRAGMA user_version` back (no
    // DDL runs), so a marker written on a PRIOR (non-migrating) init can
    // coincidentally still match `current_fp` at the moment a version
    // migration begins.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("pending-migration.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch("CREATE TABLE t(x)")
        .expect("create table");

    // Marker matches the CURRENT (pre-migration) fingerprint, exactly as it
    // would after a long-lived process's last ordinary restart.
    let fp = migration_schema_fingerprint(&conn).expect("fingerprint");
    std::fs::write(migration_marker_path(&db_path), fp).expect("write marker");

    // Simulate "this file is really a stamped-older DB": bump `user_version`
    // without touching DDL — mirrors the real incident, where the
    // migration's own DDL hasn't run yet at the point this function runs.
    conn.execute_batch("PRAGMA user_version = 1")
        .expect("stamp older schema version");

    let result = maybe_backup_before_migration(&conn, &db_path).expect("backup check");
    assert!(
        result.is_some(),
        "an in-progress version migration (1 <= stored < EXPECTED) must ALWAYS back up, \
         even when the schema fingerprint coincidentally matches the last marker"
    );
}

#[test]
fn authorized_version_migration_writes_backup_trail_end_to_end() {
    // Order-independence: the simple-tokenizer auto-extension is process-global
    // and normally registered by whichever test opens a store first; a filtered
    // run of only this test never triggers that, so register explicitly.
    crate::db::enable_simple_auto_extension().unwrap();

    // #1180 acceptance: "live daemon-path migration writes the same trail as
    // every other authorized open." Exercises the full public entry point
    // (`init_schema_with_label_mut`, what every `MemoryStore::open*` and the
    // daemon's `MemoryServer::new_with_migration_authority` funnel through),
    // not just the private helper above, so a future refactor that
    // reintroduces the fingerprint-skip bug at a different layer still fails
    // this test.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("daemon-open.db");

    // Seed a current-schema DB and let the ordinary open path stamp its
    // marker — mirrors a long-lived daemon's last ordinary (non-migrating)
    // restart.
    {
        let mut conn = Connection::open(&db_path).expect("open");
        crate::db::init_schema_with_label_mut(
            &mut conn,
            "global",
            &db_path,
            &crate::db::DbOpenContext::create_fresh(),
        )
        .expect("seed current schema");
    }
    assert!(
        migration_marker_path(&db_path).exists(),
        "seeding must leave a marker, as a real daemon's last restart would"
    );
    assert_eq!(
        count_migration_backups(tmp.path()),
        0,
        "seeding a fresh DB must not itself back up"
    );

    // Roll PRAGMA user_version back to simulate "this is really a
    // stamped-older DB" without touching DDL — no schema shape changed, so
    // the fingerprint the marker recorded is still current.
    {
        let conn = Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
    }

    // The authorized open the #1119 refusal message's trail promise covers.
    {
        let mut conn = Connection::open(&db_path).expect("reopen for migration");
        crate::db::init_schema_with_label_mut(
            &mut conn,
            "global",
            &db_path,
            &crate::db::DbOpenContext::open_existing_allow("test:1180-daemon-path"),
        )
        .expect("authorized migration must succeed");
    }

    assert_eq!(
        count_migration_backups(tmp.path()),
        1,
        "an authorized version migration must write a migration-bak trail, \
         even when the pre-migration schema fingerprint matches the last marker"
    );
}

fn count_migration_backups(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("migration-bak"))
        .count()
}

#[test]
fn remember_fingerprint_writes_marker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("remember.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch("CREATE TABLE t(x)")
        .expect("create table");

    remember_migration_fingerprint(&conn, &db_path).expect("remember");

    let content = std::fs::read_to_string(migration_marker_path(&db_path)).expect("read marker");
    assert!(
        content.contains(env!("CARGO_PKG_VERSION")),
        "marker should contain current binary version, got: {content}"
    );
    assert!(
        content.contains(':'),
        "marker should be formatted as version:schema_version, got: {content}"
    );
}

#[test]
fn retain_keeps_only_n_most_recent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("retain.db");
    let retain = migration_backup_retain_count();
    for i in 0..(retain + 2) {
        let bak = sibling_with_suffix(&db_path, &format!("migration-bak.20260101T00000{i}"));
        std::fs::write(&bak, b"fake").expect("write fake backup");
    }

    retain_recent_migration_backups(&db_path);

    let remaining: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("migration-bak"))
        .collect();
    assert_eq!(
        remaining.len(),
        retain,
        "should retain exactly N most recent backups"
    );

    let kept_names: Vec<String> = remaining
        .iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let newest = format!(
        "{}.migration-bak.20260101T00000{}",
        db_path.file_name().unwrap().to_string_lossy(),
        retain + 1
    );
    assert!(
        kept_names.iter().any(|n| n == &newest),
        "newest backup should be retained, kept: {kept_names:?}"
    );
}
