use super::*;
use memcore::MemoryStore;

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
        std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
        std::env::set_var("SILICONFLOW_MODEL", "test-model");
        std::env::set_var("SUMMARY_MODEL", "test-summary-model");
    });
}

fn test_server(db_path: std::path::PathBuf) -> crate::MemoryServer {
    ensure_test_env();
    crate::MemoryServer::new(db_path, None).expect("test memory server")
}

fn test_store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("test memory store")
}

fn test_memo(id: &str, to: Option<&str>) -> StickyMemo {
    StickyMemo {
        id: id.to_string(),
        from_agent: "leader".to_string(),
        to: to.map(str::to_string),
        text: "do the thing".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        ttl_days: 7,
    }
}

fn test_entry(memo: &StickyMemo) -> memcore::MemoryEntry {
    memcore::MemoryEntry {
        id: format!("sticky:{}", memo.id),
        path: memcore::path_router::standardize_sticky_path(memo.to.as_deref()),
        summary: format!("Sticky from {}", memo.from_agent),
        text: memo.text.clone(),
        importance: 0.6,
        timestamp: memo.created_at.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "sticky".to_string(),
        topic: "agent-sticky".to_string(),
        keywords: vec!["sticky".to_string()],
        persons: vec![],
        entities: vec![memo.from_agent.clone()],
        location: String::new(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: serde_json::json!({
            "sticky_id": memo.id,
            "sticky": memo,
            "status": "unread",
        }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

// ─── Addressing (frozen semantics #3) ──────────────────────────────────────

#[test]
fn broadcast_sticky_visible_only_to_leader() {
    let broadcast = test_memo("s1", None);
    assert!(sticky_visible_to(&broadcast, None), "leader sees broadcast");
    assert!(
        !sticky_visible_to(&broadcast, Some("oz")),
        "worker seat must NOT see unaddressed/broadcast stickies"
    );
}

#[test]
fn addressed_sticky_visible_only_to_named_seat() {
    let addressed = test_memo("s2", Some("oz"));
    assert!(
        !sticky_visible_to(&addressed, None),
        "leader must not see a sticky addressed to a named seat"
    );
    assert!(
        !sticky_visible_to(&addressed, Some("wizard")),
        "a different named seat must not see it"
    );
    assert!(
        sticky_visible_to(&addressed, Some("oz")),
        "the addressed seat sees it"
    );
}

// ─── TTL (frozen semantics #6) ──────────────────────────────────────────────

#[test]
fn ttl_expiry_matrix() {
    let now = chrono::Utc::now();
    let mut memo = test_memo("s3", None);
    memo.ttl_days = 7;
    memo.created_at = (now - chrono::Duration::days(1)).to_rfc3339();
    assert!(
        !sticky_ttl_expired(&memo, now),
        "1 day old, ttl=7: not expired"
    );

    memo.created_at = (now - chrono::Duration::days(8)).to_rfc3339();
    assert!(sticky_ttl_expired(&memo, now), "8 days old, ttl=7: expired");

    memo.created_at = (now - chrono::Duration::days(7)).to_rfc3339();
    assert!(
        sticky_ttl_expired(&memo, now),
        "exactly at cutoff: expired (>= cutoff)"
    );
}

// ─── Persisted round-trip ───────────────────────────────────────────────────

#[test]
fn sticky_persists_and_reads_back() {
    let mut store = test_store();
    let memo = test_memo("s4", Some("oz"));
    store.upsert(&test_entry(&memo)).expect("upsert sticky");

    let entries = all_sticky_entries(&mut store).expect("list sticky entries");
    assert_eq!(entries.len(), 1);
    let read_back = sticky_from_entry(&entries[0]).expect("sticky metadata present");
    assert_eq!(read_back.id, "s4");
    assert_eq!(read_back.to.as_deref(), Some("oz"));
    assert!(sticky_row_is_unread(&entries[0]));
}

// ─── Concurrency discrimination (mandatory, #964 spec) ─────────────────────
//
// N concurrent claimers race on ONE sticky id via the store's real `hard_state`
// CAS (`insert_state_if_absent` -> SQLite `INSERT ... ON CONFLICT DO NOTHING`).
// This test asserts directly on the CAS return value (not on a higher-level
// "delivered" count derived from a read-then-write on the memory row), so it
// WOULD catch a regression to naive read-then-write: a non-atomic
// "read status, then write claimed" implementation lets multiple threads read
// "unread" before any of them writes "claimed", so more than one would report
// success under this level of concurrency. Uses a real file-backed DB (not
// :memory:) so every thread holds an independent `rusqlite::Connection` and
// the race is a genuine multi-connection SQLite race, not a single-connection
// illusion.
#[test]
fn concurrent_claim_exactly_one_winner() {
    let db_path =
        std::env::temp_dir().join(format!("sticky-claim-race-{}.sqlite", uuid::Uuid::new_v4()));
    let db_path_str = db_path.to_string_lossy().to_string();

    // Seed the DB once so schema exists before concurrent opens.
    {
        let _store = MemoryStore::open(&db_path_str).expect("seed db");
    }

    const N: usize = 32;
    let sticky_id = "race-sticky-1";
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
    let mut handles = Vec::with_capacity(N);

    for _ in 0..N {
        let db_path_str = db_path_str.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || -> bool {
            let mut store = MemoryStore::open(&db_path_str).expect("open db in thread");
            barrier.wait();
            try_claim_sticky(&mut store, sticky_id, Some("racer")).unwrap_or(false)
        }));
    }

    let winners: usize = handles
        .into_iter()
        .map(|h| h.join().expect("thread join"))
        .filter(|won| *won)
        .count();

    assert_eq!(
        winners, 1,
        "exactly one of {N} concurrent claimers must win the atomic claim; got {winners} winners \
         (a naive read-then-write claim would let more than one through)"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path_str}-wal"));
    let _ = std::fs::remove_file(format!("{db_path_str}-shm"));
}

#[test]
fn is_claimed_reflects_successful_claim() {
    let db_path = std::env::temp_dir().join(format!(
        "sticky-claimed-check-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = test_server(db_path.clone());

    assert!(!sticky_is_claimed(&server, "unclaimed-id"));

    server
        .with_global_store(|store| try_claim_sticky(store, "claimed-id", Some("oz")))
        .expect("claim sticky");

    assert!(sticky_is_claimed(&server, "claimed-id"));
    assert!(!sticky_is_claimed(&server, "unclaimed-id"));

    let _ = std::fs::remove_file(&db_path);
}

// ─── Briefing surfaces unread once, then disappears ────────────────────────

#[tokio::test]
async fn briefing_claims_sticky_exactly_once_then_absent() {
    let db_path = std::env::temp_dir().join(format!(
        "sticky-briefing-once-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = test_server(db_path.clone());

    server
        .with_global_store(|store| {
            let memo = test_memo("s-brief-1", None);
            store.upsert(&test_entry(&memo)).map_err(|e| format!("{e}"))
        })
        .expect("seed sticky");

    let first = claim_unread_stickies_for_briefing(&server, None, 5).expect("first claim call");
    assert_eq!(
        first.len(),
        1,
        "first call must surface the unread sticky exactly once"
    );
    assert_eq!(first[0]["id"], serde_json::json!("s-brief-1"));

    let second = claim_unread_stickies_for_briefing(&server, None, 5).expect("second claim call");
    assert!(
        second.is_empty(),
        "sticky must not surface again after being claimed by the first briefing call"
    );

    let _ = std::fs::remove_file(&db_path);
}

// ─── Addressing at the handler/briefing boundary ───────────────────────────

#[tokio::test]
async fn addressed_sticky_invisible_to_leader_and_other_seats() {
    let db_path =
        std::env::temp_dir().join(format!("sticky-addressing-{}.sqlite", uuid::Uuid::new_v4()));
    let server = test_server(db_path.clone());

    server
        .with_global_store(|store| {
            let memo = test_memo("s-addr-1", Some("oz"));
            store.upsert(&test_entry(&memo)).map_err(|e| format!("{e}"))
        })
        .expect("seed addressed sticky");

    let leader_view = claim_unread_stickies_for_briefing(&server, None, 5).expect("leader view");
    assert!(
        leader_view.is_empty(),
        "leader briefing must not see a sticky addressed to a named seat"
    );

    let other_seat_view =
        claim_unread_stickies_for_briefing(&server, Some("wizard"), 5).expect("other seat view");
    assert!(
        other_seat_view.is_empty(),
        "a non-addressed seat must not see the sticky either"
    );

    let addressed_view =
        claim_unread_stickies_for_briefing(&server, Some("oz"), 5).expect("addressed seat view");
    assert_eq!(
        addressed_view.len(),
        1,
        "the addressed seat sees exactly one sticky"
    );
    assert_eq!(addressed_view[0]["id"], serde_json::json!("s-addr-1"));

    let _ = std::fs::remove_file(&db_path);
}

// ─── TTL expiry -> archived, include_read shows it ─────────────────────────

#[tokio::test]
async fn expired_sticky_hidden_from_unread_but_visible_in_archive() {
    let db_path = std::env::temp_dir().join(format!(
        "sticky-ttl-archive-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = test_server(db_path.clone());

    server
        .with_global_store(|store| {
            let mut memo = test_memo("s-ttl-1", None);
            memo.ttl_days = 1;
            memo.created_at = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
            let mut entry = test_entry(&memo);
            entry.timestamp = memo.created_at.clone();
            store.upsert(&entry).map_err(|e| format!("{e}"))
        })
        .expect("seed expired sticky");

    let unread = claim_unread_stickies_for_briefing(&server, None, 5).expect("unread view");
    assert!(
        unread.is_empty(),
        "expired sticky must not be delivered as unread"
    );

    let archive = list_or_claim_stickies(&server, None, true, 10).expect("archive view");
    assert_eq!(
        archive.len(),
        1,
        "expired sticky must be visible via include_read"
    );
    assert_eq!(archive[0]["status"], serde_json::json!("expired"));

    let _ = std::fs::remove_file(&db_path);
}

// ─── GC sweep (batch pass) ──────────────────────────────────────────────────

#[test]
fn gc_sweep_archives_expired_unread_stickies() {
    let mut store = test_store();
    let mut memo = test_memo("s-gc-1", None);
    memo.ttl_days = 1;
    memo.created_at = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
    let mut entry = test_entry(&memo);
    entry.timestamp = memo.created_at.clone();
    store.upsert(&entry).expect("seed expired sticky");

    let expired = super::gc_expired_sticky_memories(&mut store).expect("gc sweep");
    assert_eq!(expired, 1);

    let entries = all_sticky_entries(&mut store).expect("list after gc");
    assert!(entries[0].archived);
}
