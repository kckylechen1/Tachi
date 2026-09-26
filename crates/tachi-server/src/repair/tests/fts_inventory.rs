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

/// Insert a legacy NULL-id memory (`memories.id` is `TEXT PRIMARY KEY`
/// without NOT NULL). `valid_from` is set so a writable open's validity
/// normalization, which decodes the id as a String, leaves the row alone.
fn insert_null_id_memory(conn: &Connection) {
    conn.execute(
        "INSERT INTO memories (id, path, summary, text, timestamp, valid_from)
         VALUES (NULL, '/x/null', 'null id', 'null id text',
                 '2026-09-26T00:00:00Z', '2026-09-26T00:00:00Z')",
        [],
    )
    .expect("insert NULL-id memory");
}

/// Project the NULL-id memory into both FTS tables: the rows a pre-#1993 full
/// rebuild or R1 repair left behind next to the live projections.
fn project_null_id_memory(path: &PathBuf) {
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
             SELECT id, path, summary, text, keywords, entities FROM memories WHERE id IS NULL;
             INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
             SELECT id, path, summary, text, keywords, entities, topic FROM memories WHERE id IS NULL;",
        )
        .expect("project the NULL-id memory");
}

fn null_projection_rows(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT (SELECT COUNT(*) FROM memories_fts WHERE id IS NULL)
              + (SELECT COUNT(*) FROM memories_symbolic_fts WHERE id IS NULL)",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn search_generation_at(path: &PathBuf) -> i64 {
    let conn = Connection::open(path).unwrap();
    memcore::db::search_generation(&conn).expect("read search generation")
}

/// A generic writable open (`MemoryStore::open`): runs schema init, including
/// memcore's open-time FTS drift repair (`ensure_fts_backfilled`).
/// (`open_existing_read_write` would not do: it deliberately skips init.)
fn writable_open(path: &PathBuf) {
    let store = memcore::MemoryStore::open(path.to_str().unwrap()).expect("writable open");
    drop(store);
}

/// tachi#2000 review (astra r1, finding 1): R1 repair and the open-time
/// orphan pass must agree on one rule, a NULL-id memory is never projected,
/// or repair -> open -> repair oscillates. Before, R1 counted NULL-id memories
/// as drift baseline and re-projected them; the next open deleted them again
/// (a generation bump), and R1 reported drift once more.
///
/// `open_first`: start from the state the open-time pass converges to
/// (NULL-id projections already pruned) instead of the legacy state that
/// still holds them.
fn assert_r1_and_open_converge_with_null_memory_id(open_first: bool) {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "null_id_cycle.db");
    insert_null_id_memory(&conn);
    insert_memory(&conn, "keep", "/x/keep", "live row", "{}", None, None);
    drop(conn);
    // Settle the raw-SQL fixture first: the first writable open backfills
    // validity/timestamp columns (memories UPDATEs, each a generation bump)
    // and projects 'keep'. Later generation reads then see FTS changes only.
    writable_open(&path);
    project_null_id_memory(&path);
    assert_eq!(null_projection_rows(&Connection::open(&path).unwrap()), 2);
    if open_first {
        writable_open(&path);
        // Proves `writable_open` really runs the open-time pass, so the
        // generation check after the second open below is not vacuous.
        assert_eq!(
            null_projection_rows(&Connection::open(&path).unwrap()),
            0,
            "the writable open prunes NULL-id projection rows"
        );
    }

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert!(app.errors.is_empty(), "apply errors: {:?}", app.errors);
    drop(ctx);
    if !open_first {
        // NULL-id projection rows are drift against the non-NULL baseline.
        for kind in ["fts_drift", "symbolic_fts_drift"] {
            assert!(
                dry.findings.iter().any(|f| f.kind == kind),
                "legacy NULL-id projections must be reported as {kind}: {dry:?}"
            );
        }
        assert_eq!(app.applied, 1, "R1 re-projects only the non-NULL memory");
    }

    // Second pass: the open after R1 has nothing left to prune.
    let generation_after_repair = search_generation_at(&path);
    writable_open(&path);
    assert_eq!(
        search_generation_at(&path),
        generation_after_repair,
        "the open after R1 must not delete what R1 just projected"
    );

    let mut ctx = open_ctx(&path, "test");
    let final_dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        final_dry.findings.is_empty(),
        "R1 after open must report no drift: {final_dry:?}"
    );
    assert_eq!(null_projection_rows(&ctx.conn), 0);
    let null_memories: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories WHERE id IS NULL", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(null_memories, 1, "neither pass deletes the NULL-id memory");
    if open_first {
        // Checked last so a regression fails on the oscillation itself above.
        assert!(
            dry.findings.is_empty(),
            "the store the open-time pass converged to has no R1 drift: {dry:?}"
        );
    }
}

#[test]
fn r1_and_open_converge_with_null_memory_id_after_open_prune() {
    assert_r1_and_open_converge_with_null_memory_id(true);
}

#[test]
fn r1_and_open_converge_with_null_memory_id_from_legacy_projection() {
    assert_r1_and_open_converge_with_null_memory_id(false);
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
