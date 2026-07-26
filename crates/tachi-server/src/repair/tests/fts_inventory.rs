use super::*;

#[test]
fn r1_fts_drift_detected_and_rebuilt() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "drift.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    insert_memory(&conn, "m2", "/x/b", "world", "{}", None, None);
    // Manually delete one row from FTS to simulate drift.
    conn.execute("DELETE FROM memories_fts WHERE id = 'm2'", [])
        .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "fts_drift"),
        "dry-run should detect fts_drift, got {dry:?}"
    );

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert!(app.errors.is_empty(), "apply errors: {:?}", app.errors);
    assert_eq!(app.applied, 2, "expected 2 fts rows after rebuild");

    let dry2 = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-rebuild should be clean: {dry2:?}"
    );
}

#[test]
fn r1_missing_fts_table_rebuilt() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "no_fts.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    conn.execute_batch("DROP TABLE memories_fts;").unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(dry.findings.iter().any(|f| f.kind == "fts_table_missing"));

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 1);
}

/// #1335 oracle: R1 must reconcile `memories_symbolic_fts` drift, not just
/// `memories_fts`. `insert_memory` bypasses both manual sync paths (no
/// triggers on either table), so a fresh DB already has symbolic drift after
/// raw inserts — R1 must detect and fix it in the same pass.
#[test]
fn r1_symbolic_fts_drift_detected_and_rebuilt() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "symbolic_drift.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    insert_memory(&conn, "m2", "/x/b", "world", "{}", None, None);
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "symbolic_fts_drift"),
        "dry-run should detect symbolic_fts_drift, got {dry:?}"
    );

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert!(app.errors.is_empty(), "apply errors: {:?}", app.errors);

    let symbolic_count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories_symbolic_fts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        symbolic_count, 2,
        "expected 2 symbolic_fts rows after rebuild"
    );
    let match_hits: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memories_symbolic_fts \
             WHERE memories_symbolic_fts MATCH '\"hello\"' AND id = 'm1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(match_hits, 1, "trigram MATCH must retrieve rebuilt row");

    let dry2 = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-rebuild should be clean: {dry2:?}"
    );
}

#[test]
fn inventory_accepts_explicit_absolute_tachi_db_outside_manifest() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "outside-manifest.db");
    drop(conn);

    let manifest = crate::manifest::Manifest::empty();
    let entries = select_dbs(&manifest, Some(path.to_str().unwrap())).expect("select explicit DB");

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].path,
        std::fs::canonicalize(&path)
            .unwrap()
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(entries[0].schema_kind, "tachi");
    assert_eq!(entries[0].last_classification, "explicit_path");
}

#[test]
fn inventory_skips_genuinely_absent_project_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_db = dir.path().join("missing-project.db");
    let mut manifest = crate::manifest::Manifest::empty();
    manifest.dbs.push(manifest_db_entry(
        &missing_db,
        crate::manifest::DbRole::Project,
    ));

    let entries = select_dbs(&manifest, None).expect("inspect missing project DB");

    assert!(entries.is_empty());
    assert!(std::fs::symlink_metadata(missing_db).is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn dangling_manifest_project_symlink_refuses_bulk_and_fts_selection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app_home = dir.path().join("home");
    std::fs::create_dir_all(&app_home).expect("app home");
    let missing_target = dir.path().join("missing-external.db");
    let project_link = app_home.join("project.db");
    std::os::unix::fs::symlink(&missing_target, &project_link).expect("dangling project symlink");
    let link_identity = repair_file_identity(&project_link);
    let mut manifest = crate::manifest::Manifest::empty();
    manifest.dbs.push(manifest_db_entry(
        &project_link,
        crate::manifest::DbRole::Project,
    ));
    manifest.save(&app_home.join("manifest.json")).unwrap();

    let bulk_error = super::super::run_repair_sweep(
        None,
        vec!["R1".to_string()],
        false,
        false,
        true,
        None,
        &app_home,
    )
    .await
    .expect_err("bulk repair must refuse dangling manifest project symlink");
    let fts_error = super::super::run_fts_cli("project:test", false, &app_home, true, false)
        .await
        .expect_err("FTS selection must refuse dangling manifest project symlink");

    for error in [bulk_error.to_string(), fts_error.to_string()] {
        assert!(
            error.contains("canonical repo DB path") && error.contains("must not be a symlink"),
            "unexpected refusal: {error}"
        );
    }
    assert_eq!(repair_file_identity(&project_link), link_identity);
    assert_eq!(std::fs::read_link(&project_link).unwrap(), missing_target);
    assert!(
        std::fs::symlink_metadata(&missing_target).is_err(),
        "repair selection must not create or mutate the dangling target"
    );
}

/// Reproduces the `project:quant` failure mode: the parent virtual table
/// `memories_fts` is gone but the FTS5 shadow tables (`memories_fts_data` &c.)
/// were left behind. A naive `CREATE VIRTUAL TABLE memories_fts` errors with
/// "table memories_fts_data already exists". R1 must drop the orphan shadows
/// inside the same transaction before recreating.
///
/// In practice this state arises from a half-applied DROP / crash. We
/// reproduce it by dropping the virtual head with FTS5's internal hooks
/// disabled via direct sqlite_master manipulation: SQLite normally cascades
/// the shadow tables, but when DDL is interrupted (or when an old version
/// wrote to the schema directly), shadows can survive.
#[test]
fn r1_orphan_shadow_tables_are_cleaned_before_rebuild() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "orphan_shadows.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    insert_memory(&conn, "m2", "/x/b", "world", "{}", None, None);

    // Surgical reproduction of the production state: delete the virtual table
    // entry from sqlite_master directly via writable_schema, leaving the
    // shadow tables (memories_fts_data, _idx, _docsize, _config) behind. This
    // mimics what a partially-applied DROP / crash leaves on disk.
    conn.execute_batch(
        "PRAGMA writable_schema = ON;\n\
         DELETE FROM sqlite_master WHERE name = 'memories_fts';\n\
         PRAGMA writable_schema = OFF;",
    )
    .unwrap();
    drop(conn);

    // Reopen so SQLite re-reads the now-tampered schema.
    let conn = Connection::open(&path).unwrap();
    let shadow_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name LIKE 'memories_fts\\_%' ESCAPE '\\'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        shadow_count > 0,
        "test setup invariant: shadow tables should survive after schema-row deletion"
    );
    // And confirm the recreate would fail without the fix.
    let recreate_attempt: rusqlite::Result<()> = conn.execute_batch(
        "CREATE VIRTUAL TABLE memories_fts USING fts5(\
            id UNINDEXED, path, summary, text, keywords, entities, tokenize='simple');",
    );
    assert!(
        recreate_attempt.is_err(),
        "without the fix, naive CREATE VIRTUAL TABLE should fail on orphan shadows"
    );
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings
            .iter()
            .any(|f| f.kind == "fts_table_missing_with_orphan_shadows"),
        "dry-run should surface the orphan-shadow blocker, got {dry:?}"
    );

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert!(
        app.errors.is_empty(),
        "apply must clean shadows + recreate without errors: {:?}",
        app.errors
    );
    assert_eq!(app.applied, 2, "expected both rows reindexed");

    let dry2 = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-rebuild should be clean: {dry2:?}"
    );
}
