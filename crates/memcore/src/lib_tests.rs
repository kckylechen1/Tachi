use super::*;

fn test_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/facts/readonly".to_string(),
        summary: "Readonly search fixture".to_string(),
        text: "Hermes rate limit was caused by routing to the ZAI global endpoint".to_string(),
        importance: 0.8,
        timestamp: chrono::Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "hermes-rate-limit".to_string(),
        keywords: vec!["hermes".to_string(), "rate-limit".to_string()],
        persons: vec![],
        entities: vec!["Hermes".to_string(), "ZAI".to_string()],
        location: String::new(),
        source: "test".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: serde_json::json!({}),
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: Some("coding".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn read_only_store_supports_stats_and_search_without_access_writes() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let db_path = temp.path().to_string_lossy().to_string();

    {
        let mut store = MemoryStore::open(&db_path).expect("open writable store");
        store
            .upsert(&test_entry("readonly-search"))
            .expect("seed memory");
    }

    let store = MemoryStore::open_read_only(&db_path).expect("open read-only store");
    assert_eq!(store.stats(false).expect("stats").total, 1);

    let rows = store
        .search(
            "Hermes rate limit",
            Some(SearchOptions {
                record_access: false,
                ..Default::default()
            }),
        )
        .expect("read-only search");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entry.id, "readonly-search");
}

#[test]
fn upsert_retries_short_lived_sqlite_writer_lock() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let db_path = temp.path().to_string_lossy().to_string();
    let mut store = MemoryStore::open(&db_path).expect("open writable store");
    store
        .connection()
        .busy_timeout(std::time::Duration::from_millis(1))
        .expect("short busy timeout");

    let lock_conn = rusqlite::Connection::open(&db_path).expect("open lock connection");
    lock_conn
        .busy_timeout(std::time::Duration::from_millis(1))
        .expect("lock busy timeout");
    lock_conn
        .execute_batch("BEGIN IMMEDIATE;")
        .expect("hold writer lock");

    let handle = std::thread::spawn(move || store.upsert(&test_entry("lock-retry-upsert")));

    std::thread::sleep(std::time::Duration::from_millis(80));
    lock_conn.execute_batch("COMMIT;").expect("release lock");

    handle
        .join()
        .expect("upsert thread joined")
        .expect("upsert should retry after writer lock clears");

    let store = MemoryStore::open_read_only(&db_path).expect("open read-only store");
    assert!(
        store
            .get("lock-retry-upsert")
            .expect("read saved entry")
            .is_some(),
        "entry should be persisted after retry"
    );
}

#[test]
fn checkpoint_wal_truncate_reclaims_wal_file() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let db_path = temp.path().to_string_lossy().to_string();
    let wal_path = format!("{db_path}-wal");

    let mut store = MemoryStore::open(&db_path).expect("open writable store");
    for i in 0..50 {
        store
            .upsert(&test_entry(&format!("wal-{i}")))
            .expect("seed memory");
    }
    let wal_before = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);

    store
        .checkpoint_wal_truncate()
        .expect("checkpoint should succeed");

    let wal_after = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    assert!(
        wal_after <= wal_before,
        "TRUNCATE checkpoint must never grow the WAL (before={wal_before}, after={wal_after})"
    );
    if wal_before > 0 {
        assert_eq!(
            wal_after, 0,
            "with this store as the only connection, TRUNCATE must fully reclaim the WAL"
        );
    }
}

#[test]
fn run_optimize_refreshes_planner_statistics() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let db_path = temp.path().to_string_lossy().to_string();

    let mut store = MemoryStore::open(&db_path).expect("open writable store");
    for i in 0..50 {
        store
            .upsert(&test_entry(&format!("optimize-{i}")))
            .expect("seed memory");
    }

    store
        .run_optimize()
        .expect("PRAGMA optimize should succeed");

    // PRAGMA optimize's ANALYZE-equivalent effect is observable via
    // sqlite_stat1: it should now carry at least one row for `memories`,
    // giving the planner real selectivity data instead of guessing.
    let stat_rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'memories'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert!(
        stat_rows > 0,
        "PRAGMA optimize should populate sqlite_stat1 for the memories table"
    );

    // Idempotent / repeat-safe: calling it again on the same connection
    // must not error.
    store
        .run_optimize()
        .expect("PRAGMA optimize should be safe to call repeatedly");
}

#[test]
fn save_derived_with_id_upserts_existing_row() {
    let store = MemoryStore::open_in_memory().expect("open in-memory store");
    let metadata = serde_json::json!({"version": 1});

    store
        .save_derived_with_id(
            "stable-derived",
            "first",
            "/derived/stable",
            "first summary",
            0.4,
            "test_source",
            "project",
            &metadata,
        )
        .expect("initial derived save");
    store
        .save_derived_with_id(
            "stable-derived",
            "second",
            "/derived/stable",
            "second summary",
            0.9,
            "test_source",
            "project",
            &serde_json::json!({"version": 2}),
        )
        .expect("upsert derived save");

    let rows = store
        .list_derived_by_source("test_source", "/derived", 10)
        .expect("list derived");
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["id"], "stable-derived");
    assert_eq!(rows[0]["text"], "second");
    assert_eq!(rows[0]["summary"], "second summary");
    assert_eq!(rows[0]["importance"], 0.9);
}

#[test]
fn metadata_stats_counts_total_and_coverage_in_one_query() {
    let mut store = MemoryStore::open_in_memory().expect("open in-memory store");

    let mut complete = test_entry("metadata-complete");
    complete.keywords = vec!["hermes".to_string()];
    complete.entities = vec!["ZAI".to_string()];
    store.upsert(&complete).expect("seed complete metadata");

    let mut keywords_only = test_entry("metadata-keywords-only");
    keywords_only.keywords = vec!["routing".to_string()];
    keywords_only.entities = vec![];
    store
        .upsert(&keywords_only)
        .expect("seed keywords-only metadata");

    let mut missing = test_entry("metadata-missing");
    missing.keywords = vec![];
    missing.entities = vec!["Tachi".to_string()];
    store.upsert(&missing).expect("seed missing metadata");

    assert_eq!(store.metadata_stats().expect("metadata stats"), (3, 2));
    let missing_entries = store
        .entries_missing_metadata()
        .expect("missing metadata entries");
    assert_eq!(missing_entries.len(), 1);
    assert_eq!(missing_entries[0].0, "metadata-missing");
}
