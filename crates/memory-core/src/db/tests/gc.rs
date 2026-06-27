use super::*;

#[test]
fn gc_tables_prunes_retention_and_orphans() {
    let mut conn = make_conn();
    let e = make_entry("gc-keep", "gc target");
    upsert(&mut conn, &e, false).unwrap();

    for _ in 0..300 {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["gc-keep", now_utc_iso(), ""],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
        params!["gc-orphan", now_utc_iso(), ""],
    )
    .unwrap();

    conn.execute(
            "INSERT INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
            params!["ev-old", "id-old", "ingest", "2000-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
            params!["ev-new", "id-new", "ingest", "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();

    conn.execute(
            "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "2000-01-01T00:00:00.000Z",
                "mcp:test",
                "tool_old",
                "",
                1,
                1,
                Option::<String>::None,
                "2000-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "2999-01-01T00:00:00.000Z",
                "mcp:test",
                "tool_new",
                "",
                1,
                1,
                Option::<String>::None,
                "2999-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();

    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-old", "gc-keep", 1, "2000-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-new", "gc-keep", 2, "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-orphan", "gc-orphan", 1, "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();

    let summary = gc_tables(&mut conn, &GcConfig::default()).unwrap();

    let kept_access: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["gc-keep"],
            |row| row.get(0),
        )
        .unwrap();
    let orphan_access: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["gc-orphan"],
            |row| row.get(0),
        )
        .unwrap();
    let processed_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM processed_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let audit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
        .unwrap();
    let known_state_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agent_known_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    let orphan_known_state: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["gc-orphan"],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        kept_access, 256,
        "access_history should retain latest 256 per memory"
    );
    assert_eq!(orphan_access, 0, "orphaned access rows should be removed");
    assert_eq!(
        processed_count, 1,
        "old processed_events row should be pruned"
    );
    assert_eq!(audit_count, 1, "old audit_log row should be pruned");
    assert_eq!(
        known_state_count, 1,
        "old + orphan known-state rows should be pruned"
    );
    assert_eq!(
        orphan_known_state, 0,
        "orphaned known-state rows should be removed"
    );

    assert!(summary["access_history_pruned"].as_u64().unwrap_or(0) > 0);
    assert!(summary["orphaned_agent_known_state"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn gc_tables_reconciles_query_diversity_after_prune() {
    let mut conn = make_conn();
    let e = make_entry("gc-qd", "diversity target");
    upsert(&mut conn, &e, false).unwrap();

    for i in 0..5 {
        let hash = format!("hash-{i}");
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["gc-qd", now_utc_iso(), hash],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE memories SET query_diversity = 99 WHERE id = ?1",
        params!["gc-qd"],
    )
    .unwrap();

    let cfg = GcConfig {
        access_history_keep_per_memory: 2,
        ..GcConfig::default()
    };
    gc_tables(&mut conn, &cfg).unwrap();

    let qd: i64 = conn
        .query_row(
            "SELECT query_diversity FROM memories WHERE id = ?1",
            params!["gc-qd"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        qd, 2,
        "query_diversity should match distinct query hashes kept after GC"
    );
}
