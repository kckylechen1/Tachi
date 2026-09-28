use super::*;
use rusqlite::Connection;

#[test]
fn open_without_label_also_backs_up_before_migration() {
    // CP1 regression guard: the unlabelled MemoryStore open path goes through the
    // path_validation=false branch of open_with_label_inner. Before the
    // fix that branch called init_schema directly, skipping backup for
    // every CLI / open_cli_store path. Now both branches route through
    // init_schema_with_label_mut. This test fails (no backup file) if
    // the branches are ever split again.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("cp1.db");
    seed_v22_fixture(&db_path);

    {
        let conn = Connection::open(&db_path).expect("open");
        assert_eq!(
            crate::db::migrations::read_schema_version(&conn).expect("read v22 stamp"),
            22,
            "fixture must be a genuine v22 database"
        );
        crate::db::validate_persistent_trigger_inventory(&conn, false)
            .expect("pre-v23 fixture has no unexpected trigger definitions");
    }

    // MemoryStore::open itself correctly denies a stamped-v22 migration. Keep
    // the unlabelled path while passing the explicit production migration
    // authority required by the pre-open gate.
    let context = crate::db::DbOpenContext::open_existing_allow("test:cp1-unlabelled-backup");
    let store = crate::MemoryStore::open_with_context(db_path.to_str().expect("path"), &context)
        .expect("authorized unlabelled open must migrate v22");

    let backup_path = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|path| path.to_string_lossy().contains("migration-bak"))
        .expect("unlabelled authorized migration must back up before v22 migration");
    let backup = Connection::open(&backup_path).expect("open pre-migration backup");
    assert_eq!(
        crate::db::migrations::read_schema_version(&backup).expect("read backup version"),
        22,
        "backup must preserve the v22 stamp"
    );
    crate::db::validate_persistent_trigger_inventory(&backup, false)
        .expect("backup must preserve the authentic v22 trigger inventory");

    assert_eq!(
        crate::db::migrations::read_schema_version(store.connection())
            .expect("read migrated version"),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION,
        "authorized unlabelled migration must reach the current schema version"
    );
    crate::db::validate_persistent_trigger_inventory(store.connection(), true)
        .expect("migrated database must have the complete current trigger inventory");
}

fn seed_v22_fixture(db_path: &Path) {
    // v23 added the guard triggers and v24 added `scored_count`; removing both
    // atomic migration results produces a legitimate v22 database. A
    // stamped-current database with the guards missing would instead be damaged
    // and the pre-open inventory validation must refuse it.
    let path = db_path.to_str().expect("path");
    drop(
        crate::MemoryStore::open_with_context(path, &crate::db::DbOpenContext::create_fresh())
            .expect("provision current fixture"),
    );

    let conn = Connection::open(db_path).expect("open current fixture for v22 seeding");
    conn.execute("ALTER TABLE memories DROP COLUMN scored_count", [])
        .expect("remove v24 scored_count column");
    conn.execute_batch(
        "DROP TRIGGER memories_reserved_refs_insert_guard;
         DROP TRIGGER memories_reserved_refs_update_guard;
         DELETE FROM hard_state
          WHERE namespace = 'migrations'
             AND key IN ('v23_reserved_reference_guards', 'v24_memories_scored_count');
          PRAGMA user_version = 22;",
    )
    .expect("remove v23 and v24 migration effects");

    // A matching marker must not suppress backup for an authorized version
    // migration. This makes the test discriminate the version-migration path
    // from the ordinary fingerprint-mismatch backup path.
    let fingerprint = migration_schema_fingerprint(&conn).expect("fingerprint v22 fixture");
    std::fs::write(migration_marker_path(db_path), fingerprint).expect("write matching marker");
}

#[test]
fn backup_skipped_for_fresh_db() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("fresh.db");
    let conn = Connection::open(&db_path).expect("open");

    let result = maybe_backup_before_migration(&conn, &db_path)
        .expect("backup check")
        .backup_path()
        .cloned();
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

    let result = maybe_backup_before_migration(&conn, &db_path)
        .expect("backup check")
        .backup_path()
        .cloned();
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

    let result = maybe_backup_before_migration(&conn, &db_path)
        .expect("backup check")
        .backup_path()
        .cloned();
    assert!(
        result.is_none(),
        "matching marker on an ordinary same-version restart (stored == EXPECTED) should skip backup"
    );
}

#[test]
fn backup_still_created_when_marker_matches_but_version_migration_is_pending() {
    // #1180: a matching fingerprint must NOT suppress a REAL, authorized
    // schema-version migration's backup.
    //
    // With the `schema:<user_version>:<schema_version>` fingerprint this is
    // the ORDINARY upgrade shape, not a corner case: the previous binary's
    // last successful init recorded the marker at its own (older) stamp, and
    // no DDL has run since, so the new binary's pre-migration fingerprint is
    // byte-identical to the marker. Only the #1180 override backs it up.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("pending-migration.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch("CREATE TABLE t(x); PRAGMA user_version = 1;")
        .expect("create a stamped-older store");

    // Marker written in the exact pending-migration state, as the previous
    // binary's last successful init would have left it.
    let fp = migration_schema_fingerprint(&conn).expect("fingerprint");
    std::fs::write(migration_marker_path(&db_path), &fp).expect("write marker");
    assert_eq!(
        std::fs::read_to_string(migration_marker_path(&db_path)).expect("marker"),
        migration_schema_fingerprint(&conn).expect("fingerprint at backup time"),
        "precondition: the marker must match the state the backup decision sees"
    );
    assert!(
        (1..crate::db::migrations::EXPECTED_SCHEMA_VERSION)
            .contains(&crate::db::migrations::read_schema_version(&conn).expect("stamp")),
        "precondition: a version migration must be pending"
    );

    let result = maybe_backup_before_migration(&conn, &db_path)
        .expect("backup check")
        .backup_path()
        .cloned();
    assert!(
        result.is_some(),
        "an in-progress version migration (1 <= stored < EXPECTED) must ALWAYS back up, \
         even when the schema fingerprint matches the last marker"
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
    // stamped-older DB" without touching DDL, then record the marker the
    // previous binary's last successful init would have left at that older
    // stamp. The marker therefore matches the pre-migration fingerprint
    // exactly, which is the ordinary upgrade shape under the
    // `schema:<user_version>:<schema_version>` fingerprint.
    {
        let conn = Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
        let fp = migration_schema_fingerprint(&conn).expect("pending fingerprint");
        std::fs::write(migration_marker_path(&db_path), fp).expect("write matching marker");
    }
    {
        // Precondition, measured on a fresh connection exactly as the funnel
        // will see the file: marker == fingerprint, and a migration is pending.
        let conn = Connection::open(&db_path).expect("reopen to check precondition");
        assert_eq!(
            std::fs::read_to_string(migration_marker_path(&db_path)).expect("marker"),
            migration_schema_fingerprint(&conn).expect("fingerprint"),
            "precondition: the marker must match the pending-migration state"
        );
        assert_eq!(
            crate::db::migrations::read_schema_version(&conn).expect("stamp"),
            crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        );
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
    let schema_version: i64 = conn
        .query_row("PRAGMA schema_version", [], |r| r.get(0))
        .expect("schema_version");
    assert_eq!(
        content,
        format!("schema:0:{schema_version}"),
        "marker should be formatted as schema:<user_version>:<schema_version>"
    );
}

// ── W1-3: backup cost and ordering ──────────────────────────────────────────

fn file_sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).expect("read db bytes");
    format!("{:x}", Sha256::digest(&bytes))
}

/// Every file next to the database, by name, INCLUDING SQLite's `-wal`/`-shm`
/// sidecars. The clean-store refusal tests start from a cleanly closed store,
/// where the last close already removed both sidecars, so any sidecar LEFT
/// behind by a refused open would show up here. These comparisons observe the
/// final directory after the connection closed: they prove nothing remains,
/// not that nothing was ever created (SQLite may create and remove sidecars
/// transiently while the refused open reads). The unclean-shutdown test
/// below is where sidecars legitimately change, and it says so explicitly.
fn sibling_names(dir: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Every `hard_state` row verbatim (identity stamps, migration sentinels and
/// the rest), plus `user_version` and the schema object inventory: the
/// logical state a refused open must not change.
/// `(user_version, hard_state rows, (type, name) schema inventory)`.
type LogicalState = (i64, Vec<(String, String, String)>, Vec<(String, String)>);

fn logical_state(conn: &Connection) -> LogicalState {
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("user_version");
    let mut stmt = conn
        .prepare("SELECT namespace, key, value_json FROM hard_state ORDER BY namespace, key")
        .expect("prepare hard_state dump");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query hard_state")
        .map(|r| r.expect("hard_state row"))
        .collect();
    let mut stmt = conn
        .prepare("SELECT type, name FROM main.sqlite_schema ORDER BY type, name")
        .expect("prepare schema dump");
    let schema = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query schema")
        .map(|r| r.expect("schema row"))
        .collect();
    (user_version, rows, schema)
}

/// Provision a real store through the public funnel, then delete its marker so
/// the NEXT open is one the fingerprint heuristic would back up. That makes a
/// refused open discriminating: before W1-3 the backup ran ahead of identity
/// resolution, so this exact setup left a full-size `.migration-bak` behind a
/// refused open.
fn seed_store_needing_backup(dir: &Path, label: &str, ctx: &crate::db::DbOpenContext) -> PathBuf {
    let db_path = dir.join("memory.db");
    let path = db_path.to_str().expect("utf8 path");
    drop(crate::MemoryStore::open_with_label_and_context(path, label, ctx).expect("seed store"));
    std::fs::remove_file(migration_marker_path(&db_path)).expect("drop marker");
    assert_eq!(count_migration_backups(dir), 0, "seeding must not back up");
    db_path
}

#[test]
fn refused_role_conflict_open_leaves_no_backup_and_clean_store_bytes_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = seed_store_needing_backup(
        tmp.path(),
        "wiki",
        &crate::db::DbOpenContext::create_fresh(),
    );
    let before_hash = file_sha256(&db_path);
    let before_siblings = sibling_names(tmp.path());

    let err = crate::MemoryStore::open_with_label_and_context(
        db_path.to_str().expect("utf8 path"),
        "global",
        &crate::db::DbOpenContext::open_existing_deny(),
    )
    .err()
    .expect("a claim that disagrees with the stamp must refuse");
    assert!(
        matches!(err, MemoryError::StoreRoleConflict { .. }),
        "expected StoreRoleConflict, got {err:?}"
    );

    assert_eq!(
        count_migration_backups(tmp.path()),
        0,
        "a refused open must not write a migration backup"
    );
    assert_eq!(
        sibling_names(tmp.path()),
        before_siblings,
        "a refused open must not create or remove any sibling artifact"
    );
    // Byte identity holds here because the fixture was closed cleanly (no
    // WAL frames to fold). See `refused_open_after_unclean_shutdown_...` for
    // what is and is not promised when a WAL is left behind.
    assert_eq!(
        file_sha256(&db_path),
        before_hash,
        "a refused open of a cleanly closed store must leave its bytes unchanged"
    );
}

/// A preflight refusal must not flip a rollback-journal store to WAL.
/// `journal_mode = WAL` is persistent and is set by `apply_connection_pragmas`,
/// which runs only after the preflight; every other refusal fixture is
/// already WAL, so this is the one that catches the PRAGMAs moving ahead of
/// admission again (#1990 measured the flip on main).
#[test]
fn refused_open_leaves_a_rollback_journal_store_in_delete_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = seed_store_needing_backup(
        tmp.path(),
        "wiki",
        &crate::db::DbOpenContext::create_fresh(),
    );
    let mode: String = Connection::open(&db_path)
        .expect("switch journal mode")
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .expect("journal_mode = DELETE");
    assert_eq!(mode, "delete", "fixture: a rollback-journal store");

    let err = crate::MemoryStore::open_with_label_and_context(
        db_path.to_str().expect("utf8 path"),
        "global",
        &crate::db::DbOpenContext::open_existing_deny(),
    )
    .err()
    .expect("a conflicting role claim must refuse");
    assert!(
        matches!(err, MemoryError::StoreRoleConflict { .. }),
        "expected StoreRoleConflict, got {err:?}"
    );

    let mode: String = Connection::open(&db_path)
        .expect("inspect")
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    assert_eq!(
        mode, "delete",
        "a preflight refusal must leave the persistent journal mode unchanged"
    );
    assert_eq!(count_migration_backups(tmp.path()), 0);
}

/// The unclean-shutdown case the byte-identity claim does NOT cover. A store
/// whose last writer died with committed frames still in `-wal` is opened
/// with a conflicting role claim. After it closes, no backup, no marker and
/// no memcore DDL/stamp/identity row may remain, and the conflicting role
/// must be the one committed only in the WAL (proving the refusal read it).
/// What SQLite itself does on the connection's last close is allowed: it may
/// checkpoint those frames into the main file and remove `-wal`/`-shm` (and
/// it may create `-shm` transiently while recovering the WAL index), so
/// neither the main-file hash nor the sidecar pair is compared for equality.
#[test]
fn refused_open_after_unclean_shutdown_leaves_no_backup_and_keeps_logical_state() {
    let src = tempfile::tempdir().expect("src tempdir");
    let dst = tempfile::tempdir().expect("dst tempdir");
    let src_db = src.path().join("memory.db");
    let src_path = src_db.to_str().expect("utf8 path");
    drop(
        crate::MemoryStore::open_with_context(src_path, &crate::db::DbOpenContext::create_fresh())
            .expect("provision"),
    );

    // Keep one extra connection open so no close below is the LAST close:
    // SQLite then leaves the committed frames in `-wal` un-checkpointed,
    // which is exactly what a crashed writer leaves on disk.
    let keeper = Connection::open(&src_db).expect("keeper connection");
    keeper
        .query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("keeper read");
    drop(
        crate::MemoryStore::open_with_label_and_context(
            src_path,
            "wiki",
            &crate::db::DbOpenContext::open_existing_deny(),
        )
        .expect("stamp the role; the stamp commits into the WAL"),
    );
    let before = logical_state(&keeper);
    assert!(
        before
            .1
            .iter()
            .any(|(ns, key, value)| ns == "store_identity"
                && key == "role"
                && value.contains("wiki")),
        "fixture: the role stamp must be committed"
    );

    // Crash image: the main file plus its WAL, copied while the keeper still
    // holds them open. The marker is deliberately NOT copied, so the backup
    // heuristic would fire for this open.
    let dst_db = dst.path().join("memory.db");
    std::fs::copy(&src_db, &dst_db).expect("copy main file");
    std::fs::copy(
        src.path().join("memory.db-wal"),
        dst.path().join("memory.db-wal"),
    )
    .expect("copy wal");
    drop(keeper);
    let wal_len = std::fs::metadata(dst.path().join("memory.db-wal"))
        .expect("wal metadata")
        .len();
    assert!(
        wal_len > 0,
        "fixture: the crash image must carry WAL frames"
    );
    let before_siblings = sibling_names(dst.path());

    let err = crate::MemoryStore::open_with_label_and_context(
        dst_db.to_str().expect("utf8 path"),
        "global",
        &crate::db::DbOpenContext::open_existing_deny(),
    )
    .err()
    .expect("the WAL-committed `wiki` stamp must refuse a `global` claim");
    assert!(
        matches!(err, MemoryError::StoreRoleConflict { .. }),
        "expected StoreRoleConflict, got {err:?}"
    );

    let after_siblings = sibling_names(dst.path());
    let added: Vec<_> = after_siblings.difference(&before_siblings).collect();
    let removed: Vec<_> = before_siblings.difference(&after_siblings).collect();
    assert!(
        added.is_empty(),
        "no new file may remain after a refused open (no backup, no marker): {added:?}"
    );
    assert!(
        removed
            .iter()
            .all(|name| name.ends_with("-wal") || name.ends_with("-shm")),
        "only SQLite's own sidecars may disappear (last-close checkpoint): {removed:?}"
    );
    assert_eq!(count_migration_backups(dst.path()), 0);

    let conn = Connection::open(&dst_db).expect("inspect after refusal");
    assert_eq!(
        logical_state(&conn),
        before,
        "user_version, hard_state (identity and role rows included) and the schema \
         inventory must be exactly what the crashed writer committed"
    );
}

#[test]
fn refused_profile_mismatch_open_leaves_no_backup_and_clean_store_bytes_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = seed_store_needing_backup(
        tmp.path(),
        crate::path_router::UNKNOWN_DB_LABEL,
        &crate::db::DbOpenContext::create_fresh()
            .with_profile(crate::db::StoreProfile::PortableKernel),
    );
    let before_hash = file_sha256(&db_path);
    let before_siblings = sibling_names(tmp.path());

    let err = crate::MemoryStore::open_with_context(
        db_path.to_str().expect("utf8 path"),
        &crate::db::DbOpenContext::open_existing_deny()
            .with_profile(crate::db::StoreProfile::TachiFull),
    )
    .err()
    .expect("a full-profile caller must refuse a portable store");
    assert!(
        matches!(err, MemoryError::StoreProfileMismatch { .. }),
        "expected StoreProfileMismatch, got {err:?}"
    );

    assert_eq!(count_migration_backups(tmp.path()), 0);
    assert_eq!(sibling_names(tmp.path()), before_siblings);
    assert_eq!(file_sha256(&db_path), before_hash);
}

#[test]
fn crate_version_is_not_part_of_the_backup_fingerprint() {
    // A binary whose crate version differs but whose schema is identical must
    // see the same fingerprint the previous binary recorded, so an ordinary
    // upgrade restart does not copy every store. The marker is the only
    // cross-binary channel, so pin that it is a pure function of the file's
    // schema state and carries nothing about the binary that wrote it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("upgrade.db");
    let path = db_path.to_str().expect("utf8 path");
    let reopen = || {
        drop(
            crate::MemoryStore::open_with_context(
                path,
                &crate::db::DbOpenContext::open_existing_deny(),
            )
            .expect("same-schema reopen"),
        )
    };
    drop(
        crate::MemoryStore::open_with_context(path, &crate::db::DbOpenContext::create_fresh())
            .expect("provision"),
    );
    // Reach steady state first. The marker is written at the end of the
    // schema transaction, but the open funnel creates `memories_vec` (a vec0
    // virtual table, when sqlite-vec loads) after it, so the second open of a
    // brand-new store can see a moved DDL cookie. That is pre-existing and
    // independent of the crate version; it is not what this test pins.
    reopen();
    let steady_backups = count_migration_backups(tmp.path());

    let marker = std::fs::read_to_string(migration_marker_path(&db_path)).expect("marker");
    assert!(
        !marker.contains(env!("CARGO_PKG_VERSION")),
        "the marker must not encode the binary's crate version, got: {marker}"
    );
    let conn = Connection::open(&db_path).expect("raw open");
    assert_eq!(
        marker,
        migration_schema_fingerprint(&conn).expect("fingerprint"),
        "the marker must be exactly the file's schema fingerprint"
    );
    assert!(
        marker.starts_with(&format!(
            "schema:{}:",
            crate::db::migrations::EXPECTED_SCHEMA_VERSION
        )),
        "got: {marker}"
    );
    drop(conn);

    reopen();
    assert_eq!(
        count_migration_backups(tmp.path()),
        steady_backups,
        "a same-schema reopen must not back up"
    );
}

#[test]
fn large_store_backup_is_not_paced() {
    // Old code: `run_to_completion(128, 100ms)` slept 100 ms after every
    // 128-page step, so its cost is a function of the PAGE count, not bytes.
    // Small pages give a large page count in few bytes: ~26k pages of 512 B
    // (~13 MiB) put the old pacing floor above 20 s, while the bound below
    // leaves the unpaced copy ≥5x headroom over its worst observed time on a
    // loaded host (see the PR's cold-review table), so the test stays
    // discriminating without being a scheduler-latency test.
    const BOUND: Duration = Duration::from_secs(8);
    const OLD_FLOOR_AT_LEAST: Duration = Duration::from_secs(20);
    const PAGE_SIZE: i64 = 512;
    const TARGET_PAGES: i64 = 26_000;

    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("large.db");
    let conn = Connection::open(&db_path).expect("open");
    conn.execute_batch(&format!(
        "PRAGMA page_size = {PAGE_SIZE};
         CREATE TABLE bulk(b BLOB NOT NULL);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {TARGET_PAGES})
         INSERT INTO bulk(b) SELECT zeroblob(400) FROM n;"
    ))
    .expect("seed large store");
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .expect("page_count");
    let old_pacing_floor = Duration::from_millis(100) * ((page_count / 128) as u32);
    assert!(
        old_pacing_floor >= OLD_FLOOR_AT_LEAST,
        "fixture too small to discriminate: {page_count} pages → old pacing floor \
         {old_pacing_floor:?} vs bound {BOUND:?}"
    );

    let started = std::time::Instant::now();
    let backup_path = maybe_backup_before_migration(&conn, &db_path)
        .expect("backup")
        .backup_path()
        .cloned()
        .expect("no marker → backup");
    let elapsed = started.elapsed();
    eprintln!("large_store_backup_is_not_paced: {page_count} pages copied in {elapsed:?}");
    assert!(
        elapsed < BOUND,
        "backup of {page_count} pages took {elapsed:?} (bound {BOUND:?}; the old 128-page/100 ms \
         pacing alone costs {old_pacing_floor:?})"
    );

    let copy = Connection::open(&backup_path).expect("open backup");
    let copied_pages: i64 = copy
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .expect("backup page_count");
    assert_eq!(copied_pages, page_count, "backup must be a complete copy");
    let quick_check: String = copy
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .expect("quick_check");
    assert_eq!(quick_check, "ok");
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
