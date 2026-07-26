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
fn r8_junk_cleanup_removes_duplicate_and_cache_rows() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "junk.db");
    let old = "2026-01-01T00:00:00Z";
    let new = "2026-01-02T00:00:00Z";
    conn.execute(
        "UPDATE memories SET timestamp = ?2 WHERE id = ?1",
        ["missing-row", old],
    )
    .ok();

    insert_memory(
        &conn,
        "dup-old",
        "/notes/a",
        "same body duplicated long enough for cleanup",
        "{}",
        Some("durable"),
        None,
    );
    insert_memory(
        &conn,
        "dup-new",
        "/notes/b",
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
    insert_memory(
        &conn,
        "cache-1",
        "/system/foundry_recall_rerank_cache",
        "cache body",
        r#"{"cache_key":"foundry_recall_rerank_cache"}"#,
        Some("durable"),
        None,
    );
    insert_memory(
        &conn,
        "cache-2",
        "/scratch/recall-cache/noisy",
        "cache body by recall-cache path",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET topic='recall_rerank_cache' WHERE id='cache-2'",
        [],
    )
    .unwrap();
    insert_memory(
        &conn,
        "empty-turn",
        "/hermes/turns/1",
        "{}",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET category='other', topic='hermes_turn' WHERE id='empty-turn'",
        [],
    )
    .unwrap();
    // Discriminative seed: insert_memory does not project into symbolic FTS.
    // Seed the junk ids so a delete that forgets symbolic_fts fails red.
    let junk_ids = ["cache-1", "cache-2", "empty-turn"];
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
    assert_eq!(total, 3, "expected 3 junk candidates, got {dry:?}");

    let app = JunkCleanup.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 3);

    let remaining: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        remaining, 2,
        "same text under different paths should be preserved"
    );

    for id in junk_ids {
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
