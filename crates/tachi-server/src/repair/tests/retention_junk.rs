use super::*;

#[test]
fn r2_retention_backfill() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "ret.db");
    insert_memory(&conn, "h1", "/handoff/a", "h", "{}", None, None);
    insert_memory(&conn, "k1", "/kanban/x", "k", "{}", None, None);
    insert_memory(&conn, "g1", "/ghost/x", "g", "{}", None, None);
    insert_memory(&conn, "w1", "/wiki/y", "w", "{}", None, None);
    insert_memory(
        &conn,
        "d1",
        "/notes/z",
        "d",
        "{}",
        None,
        Some("foundry_distill"),
    );
    conn.execute("UPDATE memories SET category='decision' WHERE id='n1'", [])
        .unwrap();
    insert_memory(
        &conn,
        "f1",
        "/notes/f",
        "fallback",
        "{}",
        None,
        Some("manual"),
    );
    insert_memory(&conn, "n1", "/notes/n", "n", "{}", Some("durable"), None);
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = RetentionBackfill.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert_eq!(total, 6, "expected 6 backfill candidates, got {dry:?}");

    let app = RetentionBackfill.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 6);

    let dry2 = RetentionBackfill.dry_run(&mut ctx).unwrap();
    assert!(dry2.findings.is_empty());
}

#[test]
fn r8_routes_exact_duplicates_to_dedupe_without_deleting_evidence() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "junk.db");
    let old = "2026-01-01T00:00:00Z";
    let new = "2026-01-02T00:00:00Z";

    insert_memory(
        &conn,
        "dup-old",
        "/notes/exact",
        "same body duplicated long enough for cleanup",
        "{}",
        Some("permanent"),
        None,
    );
    insert_memory(
        &conn,
        "dup-new",
        "/notes/exact",
        "same body duplicated long enough for cleanup",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET timestamp = ?2 WHERE id = ?1",
        ["dup-old", old],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET timestamp = ?2 WHERE id = ?1",
        ["dup-new", new],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories
         SET access_count=1, recall_count=1, last_access=?2
         WHERE id=?1",
        ["dup-old", old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO access_history(memory_id, accessed_at, query_hash, event_kind)
         VALUES (?1, ?2, 'exact-duplicate-query', 'display')",
        ["dup-old", old],
    )
    .unwrap();
    drop(conn);

    let identity = path.to_string_lossy().into_owned();
    let exact_plan = memcore::MemoryStore::open_read_only(&identity)
        .unwrap()
        .plan_exact_dedupe(identity, None, Some("/notes/exact"))
        .unwrap();
    assert_eq!(exact_plan.planned_groups, 1);
    assert_eq!(exact_plan.planned_losers, 1);

    let mut ctx = open_ctx(&path, "test");
    let dry = JunkCleanup.dry_run(&mut ctx).unwrap();
    assert!(
        dry.rule_name.contains("repair dedupe"),
        "human and JSON reports must name the exact-dedupe ownership boundary"
    );
    assert!(
        dry.findings
            .iter()
            .all(|finding| finding.kind != "duplicate_text_old_versions"),
        "R8 must route exact duplicates to repair dedupe, got {dry:?}"
    );

    let app = JunkCleanup.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 0);
    for id in ["dup-old", "dup-new"] {
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM memories WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1, "R8 must preserve exact-dedupe row {id}");
    }
    let history: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id='dup-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(history, 1, "R8 must preserve duplicate use/recall evidence");
}

#[test]
fn r8_deletes_only_unprotected_ephemeral_junk_and_keeps_projections_consistent() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "junk-guards.db");

    insert_memory(
        &conn,
        "cache-eligible",
        "/system/foundry_recall_rerank_cache",
        "cache body",
        r#"{"cache_key":"foundry_recall_rerank_cache"}"#,
        Some("durable"),
        None,
    );
    insert_memory(
        &conn,
        "empty-eligible",
        "/hermes/turns/eligible",
        "{}",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET category='other', topic='hermes_turn' WHERE id='empty-eligible'",
        [],
    )
    .unwrap();

    for (id, retention) in [("cache-permanent", "permanent"), ("cache-pinned", "pinned")] {
        insert_memory(
            &conn,
            id,
            &format!("/scratch/recall-cache/{id}"),
            "protected cache lookalike",
            "{}",
            Some(retention),
            None,
        );
    }
    for id in ["cache-archived", "cache-superseded", "cache-used"] {
        insert_memory(
            &conn,
            id,
            &format!("/scratch/recall-cache/{id}"),
            "stateful cache lookalike",
            "{}",
            Some("durable"),
            None,
        );
    }
    conn.execute(
        "UPDATE memories SET archived=1 WHERE id='cache-archived'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET superseded_by='cache-eligible' WHERE id='cache-superseded'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories
         SET access_count=1, recall_count=1, query_diversity=1,
             last_access='2026-07-28T00:00:00Z', last_use_at='2026-07-28T00:00:00Z'
         WHERE id='cache-used'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO access_history(memory_id, accessed_at, query_hash, event_kind)
         VALUES ('cache-used', '2026-07-28T00:00:00Z', 'used-query', 'use')",
        [],
    )
    .unwrap();
    for (id, retention) in [("empty-permanent", "permanent"), ("empty-used", "durable")] {
        insert_memory(
            &conn,
            id,
            &format!("/hermes/turns/{id}"),
            "{}",
            "{}",
            Some(retention),
            None,
        );
        conn.execute(
            "UPDATE memories SET category='other', topic='hermes_turn' WHERE id=?1",
            [id],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE memories
         SET access_count=1, last_access='2026-07-28T00:00:00Z'
         WHERE id='empty-used'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO access_history(memory_id, accessed_at, query_hash, event_kind)
         VALUES ('empty-used', '2026-07-28T00:00:00Z', 'empty-query', 'display')",
        [],
    )
    .unwrap();

    // Discriminative seed: insert_memory does not project into symbolic FTS.
    // Seed the junk ids so a delete that forgets symbolic_fts fails red.
    let junk_ids = ["cache-eligible", "empty-eligible"];
    {
        let tx = conn.unchecked_transaction().unwrap();
        for id in junk_ids {
            memcore::db::sync_memories_symbolic_fts(&tx, id).unwrap();
        }
        tx.commit().unwrap();
    }
    for id in junk_ids {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_symbolic_fts WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "precondition: symbolic seed for {id}");
    }
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = JunkCleanup.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert_eq!(total, 2, "expected only 2 eligible junk rows, got {dry:?}");

    let app = JunkCleanup.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 2);

    for id in [
        "cache-permanent",
        "cache-pinned",
        "cache-archived",
        "cache-superseded",
        "cache-used",
        "empty-permanent",
        "empty-used",
    ] {
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM memories WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1, "R8 must preserve ineligible lookalike {id}");
    }
    let use_history: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id='cache-used'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(use_history, 1, "R8 must preserve real use evidence");

    for id in junk_ids {
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM memories WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "eligible junk row {id} must be deleted");
        let n: i64 = ctx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memories_symbolic_fts WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 0,
            "junk cleanup must leave zero memories_symbolic_fts rows for deleted id {id}"
        );
    }
    let orphan_symbolic: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memories_symbolic_fts \
             WHERE id NOT IN (SELECT id FROM memories)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        orphan_symbolic, 0,
        "junk cleanup must leave no orphan memories_symbolic_fts rows"
    );
}

#[test]
fn r8_overlap_is_classified_once_and_reports_one_unique_target() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "junk-overlap.db");
    insert_memory(
        &conn,
        "cache-empty-overlap",
        "/recall-cache/hermes-turn",
        "{}",
        "{}",
        Some("ephemeral"),
        None,
    );
    conn.execute(
        "UPDATE memories SET category='other', topic='hermes_turn'
         WHERE id='cache-empty-overlap'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = JunkCleanup.dry_run(&mut ctx).unwrap();
    assert_eq!(
        dry.finding_total(),
        1,
        "overlap must count as one unique R8 target, got {dry:?}"
    );
    assert_eq!(
        dry.findings
            .iter()
            .find(|finding| finding.kind == "foundry_recall_rerank_cache")
            .map(|finding| finding.count),
        Some(1),
        "overlap must retain its cache classification"
    );
    assert!(
        dry.findings
            .iter()
            .all(|finding| finding.kind != "empty_json_turns"),
        "cache ownership must make the empty-turn class exclusive"
    );

    let applied = JunkCleanup.apply(&mut ctx).unwrap();
    assert_eq!(applied.finding_total(), 1);
    assert_eq!(applied.applied, 1);
    let remaining: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id='cache-empty-overlap'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn r8_cli_backend_keeps_dry_run_non_mutating_and_apply_opt_in() {
    let dir = TempDir::new().unwrap();
    let app_home = dir.path().join("home");
    std::fs::create_dir_all(&app_home).unwrap();
    let db_path = app_home.join("junk-cli.db");
    let conn = fresh_db_at(&db_path, "junk-cli");
    insert_memory(
        &conn,
        "cache-cli",
        "/recall-cache/cli",
        "ephemeral CLI fixture",
        "{}",
        Some("ephemeral"),
        None,
    );
    drop(conn);

    let mut manifest = crate::manifest::Manifest::empty();
    manifest
        .dbs
        .push(manifest_db_entry(&db_path, crate::manifest::DbRole::Global));
    manifest.save(&app_home.join("manifest.json")).unwrap();
    let db_filter = db_path.to_string_lossy().into_owned();

    let dry_error = super::super::run_repair_sweep(
        Some(db_filter.clone()),
        vec!["R8".to_string()],
        false,
        true,
        true,
        None,
        &app_home,
    )
    .await
    .expect_err("R8 dry-run must report pending maintenance");
    assert_eq!(dry_error.to_string(), "repair completed with exit code 1");
    let dry_conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        dry_conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id='cache-cli'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "dry-run must not mutate the eligible row"
    );
    drop(dry_conn);

    super::super::run_repair_sweep(
        Some(db_filter),
        vec!["R8".to_string()],
        true,
        true,
        false,
        None,
        &app_home,
    )
    .await
    .expect("explicit R8 apply succeeds");
    let applied_conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        applied_conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id='cache-cli'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "explicit apply must delete eligible ephemeral junk"
    );
    assert!(
        !super::super::resolve_rules(&[])
            .iter()
            .any(|rule| rule == "R8"),
        "R8 must remain excluded from the default repair sweep"
    );
}
