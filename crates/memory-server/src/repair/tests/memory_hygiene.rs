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
