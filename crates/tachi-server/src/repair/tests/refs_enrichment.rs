use super::*;

#[test]
fn r7_orphan_edges_detected_and_purged() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "edges.db");
    insert_memory(&conn, "m1", "/a", "x", "{}", None, None);
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at)
         VALUES ('m1', 'ghost', 'rel', 1.0, '{}', ?1)",
        [&now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at)
         VALUES ('also_ghost', 'm1', 'rel', 1.0, '{}', ?1)",
        [&now],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = OrphanRefs.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert!(total >= 2, "expected at least 2 orphans, got {dry:?}");

    let app = OrphanRefs.apply(&mut ctx).unwrap();
    assert!(app.applied >= 2);
    let dry2 = OrphanRefs.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-purge should be clean: {dry2:?}"
    );
}

#[test]
fn r7_orphan_vectors_and_superseded_refs_detected_and_purged() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "vectors.db");
    memcore::db::try_load_sqlite_vec(&conn);
    insert_memory(&conn, "m1", "/a", "x", "{}", None, None);
    conn.execute(
        "UPDATE memories SET superseded_by = 'ghost' WHERE id = 'm1'",
        [],
    )
    .unwrap();
    let embedding = memcore::db::serialize_f32(&vec![0.0; 1024]);
    conn.execute(
        "INSERT INTO memories_vec(id, embedding) VALUES ('ghost-vector', ?1)",
        [embedding],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = OrphanRefs.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "orphans_vectors"),
        "expected vector orphan finding, got {dry:?}"
    );
    assert!(
        dry.findings
            .iter()
            .any(|f| f.kind == "orphans_memories_superseded_by"),
        "expected broken superseded_by finding, got {dry:?}"
    );

    let app = OrphanRefs.apply(&mut ctx).unwrap();
    assert!(app.applied >= 2, "expected at least 2 fixes, got {app:?}");
    let dry2 = OrphanRefs.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-purge should be clean: {dry2:?}"
    );
}

#[test]
fn r10_enrichment_failure_reset_clears_failed_markers_only() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "enrichment.db");
    insert_memory(
        &conn,
        "m1",
        "/a",
        "x",
        &serde_json::json!({
            "enrichment": {
                "status": "failed",
                "failed_stage": "embedding",
                "last_error": "Voyage batch API error: 401 Unauthorized",
                "last_failure_at": "2026-06-08T00:00:00Z",
                "attempts": 3
            }
        })
        .to_string(),
        None,
        None,
    );
    insert_memory(
        &conn,
        "m2",
        "/b",
        "y",
        &serde_json::json!({
            "enrichment": {
                "status": "failed",
                "failed_stage": "db_update",
                "last_error": "no such column: persons",
                "last_failure_at": "2026-06-08T00:00:00Z"
            }
        })
        .to_string(),
        None,
        None,
    );
    insert_memory(
        &conn,
        "m3",
        "/c",
        "z",
        &serde_json::json!({
            "enrichment": {
                "status": "complete",
                "attempts": 1
            }
        })
        .to_string(),
        None,
        None,
    );
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = EnrichmentFailureReset.dry_run(&mut ctx).unwrap();
    assert_eq!(
        dry.finding_total(),
        2,
        "expected two failed markers: {dry:?}"
    );
    assert!(dry
        .findings
        .iter()
        .any(|finding| finding.kind == "enrichment_failed_embedding"));
    assert!(dry
        .findings
        .iter()
        .any(|finding| finding.kind == "enrichment_failed_db_update"));

    let applied = EnrichmentFailureReset.apply(&mut ctx).unwrap();
    assert_eq!(applied.applied, 2);
    let remaining = EnrichmentFailureReset.dry_run(&mut ctx).unwrap();
    assert!(
        remaining.findings.is_empty(),
        "failed markers should be cleared: {remaining:?}"
    );

    let metadata: String = ctx
        .conn
        .query_row("SELECT metadata FROM memories WHERE id = 'm1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(parsed["enrichment"]["attempts"], serde_json::json!(3));
    assert!(parsed["enrichment"].get("status").is_none());
    assert!(parsed["enrichment"].get("last_error").is_none());

    let complete_status: Option<String> = ctx
        .conn
        .query_row(
            "SELECT json_extract(metadata, '$.enrichment.status') FROM memories WHERE id = 'm3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(complete_status.as_deref(), Some("complete"));
}

#[test]
fn r7_and_r10_skip_retired_sticky_candidates_while_ordinary_rows_progress() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "retired-sticky-refs-enrichment.db");
    for (id, memory_path) in [
        ("ordinary", "/notes/ordinary"),
        ("sticky-canonical", "/sticky/legacy"),
        ("sticky-malformed", "//STICKY///legacy"),
    ] {
        insert_memory(
            &conn,
            id,
            memory_path,
            id,
            &serde_json::json!({
                "enrichment": {"status": "failed", "attempts": 3}
            })
            .to_string(),
            None,
            None,
        );
        conn.execute(
            "UPDATE memories SET superseded_by='missing-target' WHERE id=?1",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_edges
             (source_id,target_id,relation,weight,metadata,created_at)
             VALUES (?1,'missing-target','rel',1.0,'{}','2026-01-01T00:00:00Z')",
            [id],
        )
        .unwrap();
    }
    let sticky_before = ["sticky-canonical", "sticky-malformed"].map(|id| {
        conn.query_row(
            "SELECT superseded_by,metadata FROM memories WHERE id=?1",
            [id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap()
    });
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let refs = OrphanRefs.apply(&mut ctx).unwrap();
    assert_eq!(refs.applied, 2, "ordinary edge and link should progress");
    let enrichment = EnrichmentFailureReset.apply(&mut ctx).unwrap();
    assert_eq!(enrichment.applied, 1, "only ordinary metadata should reset");

    let ordinary: (Option<String>, Option<String>, i64) = ctx
        .conn
        .query_row(
            "SELECT superseded_by,json_extract(metadata,'$.enrichment.status'),
                    (SELECT COUNT(*) FROM memory_edges WHERE source_id='ordinary')
             FROM memories WHERE id='ordinary'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(ordinary, (None, None, 0));
    let sticky_after = ["sticky-canonical", "sticky-malformed"].map(|id| {
        ctx.conn
            .query_row(
                "SELECT superseded_by,metadata FROM memories WHERE id=?1",
                [id],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap()
    });
    assert_eq!(sticky_after, sticky_before);
    for id in ["sticky-canonical", "sticky-malformed"] {
        assert_eq!(
            ctx.conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id=?1",
                    [id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "R7 must retain the dangling edge owned by retired sticky row {id}"
        );
    }
}
