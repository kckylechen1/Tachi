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
        conn.execute_batch("CREATE TABLE legacy(x)").expect("create");
    }
    std::fs::write(migration_marker_path(&db_path), "0.0.0:0").expect("stale marker");

    let _store = crate::MemoryStore::open(db_path.to_str().expect("path")).expect("open");

    let backup_exists = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .contains("migration-bak")
        });
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("matched.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch("CREATE TABLE t(x)")
        .expect("create table");

    let fp = migration_schema_fingerprint(&conn).expect("fingerprint");
    std::fs::write(migration_marker_path(&db_path), fp).expect("write marker");

    let result = maybe_backup_before_migration(&conn, &db_path).expect("backup check");
    assert!(result.is_none(), "matching marker should skip backup");
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
