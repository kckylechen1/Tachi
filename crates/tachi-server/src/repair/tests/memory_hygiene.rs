use super::*;

#[test]
fn r12_memory_hygiene_repairs_legacy_distill_and_archives_safe_raw_only() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "memory-hygiene.db");

    insert_memory(
        &conn,
        "raw-safe",
        "/openclaw/agent-main/legacy",
        "safe raw source covered by a distill output",
        "{}",
        Some("durable"),
        Some("external:capture_session"),
    );
    insert_memory(
        &conn,
        "raw-used",
        "/openclaw/agent-main/used",
        "used raw source should remain active",
        "{}",
        Some("durable"),
        Some("external:capture_session"),
    );
    conn.execute("UPDATE memories SET access_count=1 WHERE id='raw-used'", [])
        .unwrap();
    insert_memory(
        &conn,
        "raw-pinned",
        "/openclaw/agent-main/pinned",
        "pinned raw source should remain active",
        "{}",
        Some("pinned"),
        Some("external:capture_session"),
    );
    insert_memory(
        &conn,
        "raw-important",
        "/openclaw/agent-main/important",
        "high importance raw source should remain active",
        "{}",
        Some("durable"),
        Some("external:capture_session"),
    );
    conn.execute(
        "UPDATE memories SET importance=0.95 WHERE id='raw-important'",
        [],
    )
    .unwrap();
    insert_memory(
        &conn,
        "distill-legacy",
        "/foundry/agents/hapi/distilled/legacy",
        "distilled hapi lesson",
        r#"{"source_memory_ids":["raw-safe","raw-used","raw-pinned","raw-important"]}"#,
        Some("permanent"),
        Some("foundry_distill"),
    );

    insert_memory(
        &conn,
        "dup-old",
        "/trading/equity/daily_review/2026-06-10",
        r#"entry {"summary":{"created_at":"old"},"signal":"same"}"#,
        "{}",
        Some("durable"),
        Some("external:mcp"),
    );
    insert_memory(
        &conn,
        "dup-new",
        "/trading/equity/daily_review/2026-06-10",
        r#"entry {"summary":{"created_at":"new"},"signal":"same"}"#,
        "{}",
        Some("durable"),
        Some("external:mcp"),
    );
    conn.execute(
        "UPDATE memories SET timestamp='2026-01-01T00:00:00Z' WHERE id='dup-old'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET timestamp='2026-01-02T00:00:00Z' WHERE id='dup-new'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = MemoryHygiene.dry_run(&mut ctx).unwrap();
    let kinds = dry
        .findings
        .iter()
        .map(|f| (f.kind.as_str(), f.count))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(kinds.get("legacy_foundry_distill_raw"), Some(&1));
    assert_eq!(kinds.get("missing_distill_derived_items"), Some(&1));
    assert_eq!(kinds.get("missing_distilled_from_edges"), Some(&4));
    assert_eq!(kinds.get("covered_safe_raw_sources"), Some(&1));
    assert_eq!(kinds.get("safe_duplicate_raw_sources"), Some(&1));

    let applied = MemoryHygiene.apply(&mut ctx).unwrap();
    assert!(
        applied.applied >= 8,
        "expected multiple R12 repairs: {applied:?}"
    );

    let distill_tier: String = ctx
        .conn
        .query_row(
            "SELECT tier FROM memories WHERE id='distill-legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(distill_tier, "consolidated");

    let derived_count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM derived_items WHERE id='derived:distill-legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(derived_count, 1);

    let edge_count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memory_edges
             WHERE source_id='distill-legacy' AND relation='distilled_from'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(edge_count, 4);

    let safe_state: (bool, Option<String>) = ctx
        .conn
        .query_row(
            "SELECT archived, superseded_by FROM memories WHERE id='raw-safe'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(safe_state, (true, Some("distill-legacy".to_string())));

    for id in ["raw-used", "raw-pinned", "raw-important", "dup-new"] {
        let archived: bool = ctx
            .conn
            .query_row("SELECT archived FROM memories WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!archived, "{id} should remain active");
    }

    let duplicate_state: (bool, Option<String>) = ctx
        .conn
        .query_row(
            "SELECT archived, superseded_by FROM memories WHERE id='dup-old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(duplicate_state, (true, Some("dup-new".to_string())));

    let dry_after = MemoryHygiene.dry_run(&mut ctx).unwrap();
    assert!(
        dry_after.findings.is_empty(),
        "R12 should be idempotent after apply: {dry_after:?}"
    );
}

#[test]
fn r12_skips_retired_sticky_promotion_and_archive_projections() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "retired-sticky-hygiene.db");
    for (id, memory_path) in [
        ("raw-ordinary", "/notes/raw"),
        ("raw-sticky", "//STICKY///raw"),
    ] {
        insert_memory(
            &conn,
            id,
            memory_path,
            id,
            "{}",
            Some("durable"),
            Some("external:capture"),
        );
    }
    insert_memory(
        &conn,
        "distill-ordinary",
        "/foundry/distilled/ordinary",
        "ordinary distill",
        r#"{"source_memory_ids":["raw-ordinary"]}"#,
        Some("permanent"),
        Some("foundry_distill"),
    );
    insert_memory(
        &conn,
        "distill-sticky",
        "/sticky/distilled/legacy",
        "retired distill",
        r#"{"source_memory_ids":["raw-sticky"]}"#,
        Some("permanent"),
        Some("foundry_distill"),
    );
    let sticky_before = ["raw-sticky", "distill-sticky"].map(|id| {
        conn.query_row(
            "SELECT path,tier,archived,superseded_by,revision,metadata FROM memories WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .unwrap()
    });
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let applied = MemoryHygiene.apply(&mut ctx).unwrap();
    assert_eq!(
        applied.applied, 4,
        "ordinary promote/project/archive path should progress"
    );
    assert_eq!(
        ctx.conn
            .query_row(
                "SELECT tier FROM memories WHERE id='distill-ordinary'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "consolidated"
    );
    assert!(ctx
        .conn
        .query_row(
            "SELECT archived FROM memories WHERE id='raw-ordinary'",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    let sticky_after = ["raw-sticky", "distill-sticky"].map(|id| {
        ctx.conn.query_row(
            "SELECT path,tier,archived,superseded_by,revision,metadata FROM memories WHERE id=?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, i64>(4)?, row.get::<_, String>(5)?)),
        ).unwrap()
    });
    assert_eq!(sticky_after, sticky_before);
    assert_eq!(
        ctx.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM derived_items WHERE id='derived:distill-sticky') +
                    (SELECT COUNT(*) FROM memory_edges WHERE source_id IN ('raw-sticky','distill-sticky') OR target_id IN ('raw-sticky','distill-sticky'))",
            [],
            |row| row.get::<_, i64>(0),
        ).unwrap(),
        0,
        "R12 must create no projection involving a retired sticky row"
    );
}

#[test]
fn r12_receipt_collision_rolls_back_every_repair_write() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "memory-hygiene-rollback.db");
    insert_memory(
        &conn,
        "raw-rollback",
        "/notes/raw-rollback",
        "raw source",
        "{}",
        Some("durable"),
        Some("external:capture"),
    );
    insert_memory(
        &conn,
        "distill-rollback",
        "/foundry/distilled/rollback",
        "distill output",
        r#"{"source_memory_ids":["raw-rollback"]}"#,
        Some("permanent"),
        Some("foundry_distill"),
    );
    let receipt_id = memcore::SupersessionReceipt::id_for(
        "r12_memory_hygiene_v1",
        "r12-memory-hygiene-v1",
        "raw-rollback",
        "distill-rollback",
    );
    conn.execute(
        "INSERT INTO tachi_events (
            id, source_repo, adapter, project, domain, session_id, actor,
            event_type, authority, effects, projection_hints,
            payload_json, provenance_json, created_at
         ) VALUES (?1,'','fixture','','memory','','fixture','ordinary.fixture','raw_fact',
                   '[]','[]','{}','{}','2026-01-01T00:00:00Z')",
        [&receipt_id],
    )
    .expect("seed receipt-id collision");
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let error = MemoryHygiene
        .apply(&mut ctx)
        .expect_err("receipt collision must abort the whole R12 transaction");
    assert!(
        error.to_string().contains("identity conflict"),
        "unexpected R12 failure: {error}"
    );
    let source_state: (bool, Option<String>) = ctx
        .conn
        .query_row(
            "SELECT archived, superseded_by FROM memories WHERE id='raw-rollback'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(source_state, (false, None));
    let distill_tier: String = ctx
        .conn
        .query_row(
            "SELECT tier FROM memories WHERE id='distill-rollback'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(distill_tier, "raw");
    for count in [
        ctx.conn
            .query_row(
                "SELECT COUNT(*) FROM derived_items WHERE id='derived:distill-rollback'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        ctx.conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edges
                 WHERE source_id='distill-rollback' AND target_id='raw-rollback'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        ctx.conn
            .query_row(
                "SELECT COUNT(*) FROM hard_state
                 WHERE namespace=?1 AND key=?2",
                [memcore::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
    ] {
        assert_eq!(count, 0, "R12 failure leaked a transactional write");
    }
}
