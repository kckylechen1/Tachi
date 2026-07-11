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

// ─── Direct CAS-contract test (CP1: discriminates a naive upsert) ─────────
//
// CP1 (opus xhigh review of #964/PR #1003): the 32-thread concurrency test
// below asserts `winners == 1`, but that property falls out of SQLite's own
// row/table locking under concurrent connections — the reviewer mutated
// `try_claim_sticky` to a naive "read status, then write claimed" shape and
// it STILL passed (winners=1), because SQLite serialized the underlying
// reads/writes tightly enough that the race window was never actually hit
// in that run. It does NOT positively verify the function itself is a CAS.
//
// This test asserts on the CAS return value directly, sequentially (no
// concurrency, no scheduler luck involved): call `try_claim_sticky` twice in
// a row for the SAME sticky id. A real CAS (`INSERT ... ON CONFLICT DO
// NOTHING`) returns `Ok(true)` then `Ok(false)` — the second call sees the
// row already present and its `changed == 0` branch fires.
//
// Important honesty note on what a SEQUENTIAL test can and cannot catch: it
// discriminates an unconditional/no-check naive upsert (e.g. `set_state`,
// which always writes and always returns success) — that mutation returns
// `Ok(true)` on BOTH sequential calls, and this test catches it (verified
// red below). It does NOT discriminate the reviewer's specific
// "read-then-check-then-write" naive shape, because two SEQUENTIAL calls
// never race with each other — the second call's read always observes the
// first call's already-committed write, so a presence-checked
// read-then-write returns the same `Ok(true)`/`Ok(false)` sequence a real
// CAS does when called one at a time. That race only manifests under true
// concurrency, which is exactly why the 32-thread smoke test below exists
// (and exactly why the reviewer's finding — that even THAT test can miss
// it under a given scheduler interleaving — is a real, standing risk this
// pair of tests narrows but cannot fully close without a construct that
// deterministically interleaves two threads at the read/write boundary).
//
// Verified red against an unconditional-naive-upsert mutation (temporarily
// replacing the `insert_state_if_absent` call in `try_claim_sticky` with an
// unconditional `store.set_state(...)` that always returns `Ok(true)`, no
// presence check at all) — see PR comment for the pasted red run. Mutation
// was reverted before this commit; this test source reflects only the real
// CAS implementation.
#[test]
fn try_claim_sticky_is_cas_not_naive_upsert() {
    let db_path = std::env::temp_dir().join(format!(
        "sticky-cas-contract-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let mut store = MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open db");

    let first = try_claim_sticky(&mut store, "cas-contract-sticky", Some("racer-a"))
        .expect("first claim call");
    assert!(
        first,
        "first claim on an unclaimed sticky must win (Ok(true))"
    );

    let second = try_claim_sticky(&mut store, "cas-contract-sticky", Some("racer-b"))
        .expect("second claim call");
    assert!(
        !second,
        "second claim on an ALREADY-claimed sticky must lose (Ok(false)) — \
         a naive upsert would return Ok(true) here too, which is exactly \
         the regression this test exists to catch"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{}-wal", db_path.to_string_lossy()));
    let _ = std::fs::remove_file(format!("{}-shm", db_path.to_string_lossy()));
}

// ─── Concurrency smoke test (NOT a CAS-discrimination proof — see above) ───
//
// N concurrent claimers race on ONE sticky id via the store's real `hard_state`
// CAS (`insert_state_if_absent` -> SQLite `INSERT ... ON CONFLICT DO NOTHING`).
// This is a smoke test for "no crashes / no double-delivery under load with
// the real implementation" — it does NOT, by itself, prove the code is a
// CAS (see `try_claim_sticky_is_cas_not_naive_upsert` above for the test
// that actually discriminates that). Uses a real file-backed DB (not
// :memory:) so every thread holds an independent `rusqlite::Connection` and
// the race is a genuine multi-connection SQLite race, not a single-connection
// illusion.
#[test]
fn concurrent_claim_smoke_single_winner_under_load() {
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

// ─── CP4 security: secrets scrubbed before storage AND before render ───────
//
// CP4 (opus xhigh review of #964/PR #1003): the sticky path used to store
// `memo.text` RAW (no `scrub_secrets`/`scrub_think_tags`), so a bearer
// token / API key / AWS key left in a sticky body round-tripped verbatim
// into the DB row and into the leader briefing markdown. This test asserts
// BOTH surfaces are masked: the persisted row (via `sticky_from_entry`) and
// the rendered briefing markdown (via `agent_markdown::format_briefing`,
// the same renderer `handle_memory_briefing` calls in production).
#[tokio::test]
async fn sticky_leave_scrubs_secrets_in_storage_and_briefing_render() {
    let db_path =
        std::env::temp_dir().join(format!("sticky-cp4-scrub-{}.sqlite", uuid::Uuid::new_v4()));
    let server = test_server(db_path.clone());

    let raw_secret_text =
        "heads up: Authorization: Bearer sk-abc123def456ghi789jkl012mno345 is still live";

    let leave_response = handle_sticky_leave(
        &server,
        StickyLeaveInput {
            text: raw_secret_text.to_string(),
            to: None,
            ttl_days: None,
        },
    )
    .await
    .expect("sticky_leave must succeed");
    let leave_json: serde_json::Value =
        serde_json::from_str(&leave_response).expect("sticky_leave response is JSON");
    assert_eq!(leave_json["status"], serde_json::json!("sticky_left"));

    // (a) Persisted row: read the row straight back out of the store and
    // confirm the raw bearer token never landed in the DB.
    let stored_text = server
        .with_global_store_read(|store| {
            let entries = all_sticky_entries(store)?;
            let memo = entries
                .iter()
                .find_map(sticky_from_entry)
                .ok_or_else(|| "expected exactly one persisted sticky".to_string())?;
            Ok(memo.text)
        })
        .expect("read back persisted sticky text");
    assert!(
        !stored_text.contains("sk-abc123def456ghi789jkl012mno345"),
        "raw bearer token must never be persisted verbatim in the sticky row; got: {stored_text}"
    );
    assert!(
        stored_text.contains("[REDACTED]"),
        "persisted sticky text must show the masked form; got: {stored_text}"
    );

    // (b) Briefing render: claim the sticky for the leader (this IS the
    // read, per frozen semantics #4) and render it through the SAME
    // markdown formatter `handle_memory_briefing` uses, then confirm the
    // raw token never appears in the rendered markdown either.
    let claimed = claim_unread_stickies_for_briefing(&server, None, 5)
        .expect("claim sticky for briefing render");
    assert_eq!(
        claimed.len(),
        1,
        "the scrubbed sticky must still be delivered"
    );
    let stickies_value = serde_json::json!(claimed);

    let markdown = crate::agent_markdown::format_briefing(
        "test query",
        None,
        &stickies_value,
        &serde_json::json!([]),
        &serde_json::json!([]),
        &serde_json::json!([]),
        &serde_json::json!({}),
        &serde_json::json!([]),
        &serde_json::json!([]),
        &serde_json::json!([]),
        &[],
        &serde_json::json!({}),
        false,
    );
    assert!(
        !markdown.contains("sk-abc123def456ghi789jkl012mno345"),
        "raw bearer token must never appear in the rendered briefing markdown; got: {markdown}"
    );
    // The briefing markdown renderer runs sticky text through `md_escape`
    // (escapes `[`/`]`/`*`/`_` for markdown safety), so the masked marker
    // appears as `\[REDACTED\]` in the final rendered output — assert on
    // that literal escaped form rather than the raw `[REDACTED]` string.
    assert!(
        markdown.contains("\\[REDACTED\\]"),
        "rendered briefing markdown must show the masked form; got: {markdown}"
    );

    let _ = std::fs::remove_file(&db_path);
}

// ─── CP3 crash-safety: mark_claimed failure must not lose delivery ────────
//
// CP3 (opus xhigh review of #964/PR #1003): claim (hard_state CAS) and mark
// (memory-row mirror) are two non-transactional writes. This test proves
// the two halves of the fix directly:
//   (a) `mark_claimed`'s own error branch is real and reachable (malformed
//       metadata triggers `Err`, it does not silently succeed).
//   (b) the delivery pipeline decides "did this caller get the sticky?" at
//       the CAS win, NOT at the mark-row write — a winning CAS with a
//       subsequently-failing mark still leaves `sticky_is_claimed` true
//       (nobody else can ever win this sticky again) while the CURRENT
//       caller's own `try_claim_sticky` call already told them they won,
//       matching how `claim_unread_stickies_for_briefing` pushes to
//       `delivered` before attempting the (best-effort) mark write.
#[test]
fn mark_claimed_error_branch_is_reachable_and_does_not_affect_cas_outcome() {
    let db_path = std::env::temp_dir().join(format!(
        "sticky-cp3-mark-failure-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = test_server(db_path.clone());

    server
        .with_global_store(|store| {
            // Win the CAS first, exactly like the production call order.
            let won = try_claim_sticky(store, "cp3-sticky", Some("leader"))?;
            assert!(won, "first claim on a fresh sticky must win");

            // Malformed metadata (not a JSON object) forces mark_claimed's
            // `as_object_mut()` branch to fail — this is the exact failure
            // this test exists to prove is non-fatal to the CAS outcome.
            let mut memo = test_memo("cp3-sticky", None);
            memo.text = "irrelevant for this test".to_string();
            let mut entry = test_entry(&memo);
            entry.metadata = serde_json::json!("not-an-object");

            let mark_result = mark_claimed(store, entry, "leader");
            assert!(
                mark_result.is_err(),
                "mark_claimed must surface its own error rather than silently succeeding"
            );
            Ok(())
        })
        .expect("test body");

    // The CAS itself is unaffected by the mark_claimed failure above: the
    // sticky is durably claimed (nobody else can ever win it), which is
    // exactly the "delivery already decided" guarantee the CURRENT caller
    // relied on when they pushed to `delivered` before the mark write ran.
    assert!(
        sticky_is_claimed(&server, "cp3-sticky"),
        "CAS win must remain durable even though the row-mirror write failed"
    );

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

// ─── CP2: server-side identity chain on the delivery path ─────────────────
//
// CP2 (opus xhigh review of #964/PR #1003): the delivery path used to trust
// ONLY the caller-supplied `params.agent_id`, with no server-side fallback,
// while `sticky_leave` already resolved identity server-side. A worker
// briefing with no `agent_id` param was silently treated as leader and
// consumed broadcast (`to`-absent) stickies meant for the real leader.
//
// `resolve_caller_agent_id` implements the three-step chain: params ->
// agent_profile (server-side, expected None post-#973) -> TACHI_PROFILE env
// -> None (leader). These tests cover exactly the three adjudicated cases.
fn with_tachi_profile_env<F: FnOnce()>(value: Option<&str>, f: F) {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original = std::env::var_os("TACHI_PROFILE");
    match value {
        Some(v) => std::env::set_var("TACHI_PROFILE", v),
        None => std::env::remove_var("TACHI_PROFILE"),
    }
    f();
    match original {
        Some(v) => std::env::set_var("TACHI_PROFILE", v),
        None => std::env::remove_var("TACHI_PROFILE"),
    }
}

#[test]
fn cp2_caller_with_explicit_agent_id_consumes_only_its_own_addressed_stickies() {
    with_tachi_profile_env(Some("some-other-profile-must-be-ignored"), || {
        let db_path = std::env::temp_dir().join(format!(
            "sticky-cp2-explicit-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = test_server(db_path.clone());

        server
            .with_global_store(|store| {
                let broadcast = test_memo("s-cp2-broadcast", None);
                store
                    .upsert(&test_entry(&broadcast))
                    .map_err(|e| format!("{e}"))?;
                let addressed = test_memo("s-cp2-addressed", Some("wizard"));
                store
                    .upsert(&test_entry(&addressed))
                    .map_err(|e| format!("{e}"))
            })
            .expect("seed stickies");

        // (a) caller with agent_id set consumes only its own addressed
        // stickies — never the broadcast one, even though TACHI_PROFILE is
        // ALSO set (explicit param must win over the env fallback).
        let resolved = resolve_caller_agent_id(&server, Some("wizard"));
        assert_eq!(resolved.as_deref(), Some("wizard"));

        let delivered = claim_unread_stickies_for_briefing(&server, resolved.as_deref(), 5)
            .expect("claim for wizard");
        assert_eq!(
            delivered.len(),
            1,
            "wizard must see only its addressed sticky"
        );
        assert_eq!(delivered[0]["id"], serde_json::json!("s-cp2-addressed"));

        let _ = std::fs::remove_file(&db_path);
    });
}

#[test]
fn cp2_param_less_caller_with_tachi_profile_env_does_not_consume_broadcast() {
    with_tachi_profile_env(Some("wizard"), || {
        let db_path = std::env::temp_dir().join(format!(
            "sticky-cp2-env-fallback-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = test_server(db_path.clone());

        server
            .with_global_store(|store| {
                let broadcast = test_memo("s-cp2-env-broadcast", None);
                store
                    .upsert(&test_entry(&broadcast))
                    .map_err(|e| format!("{e}"))
            })
            .expect("seed broadcast sticky");

        // (b) caller with NO param but TACHI_PROFILE=wizard set must resolve
        // to "wizard" and therefore NOT consume the to:-absent broadcast —
        // this is the exact CP2 regression: before the fix, a param-less
        // call here would have been silently treated as leader and eaten
        // the broadcast meant for the real leader.
        let resolved = resolve_caller_agent_id(&server, None);
        assert_eq!(
            resolved.as_deref(),
            Some("wizard"),
            "param-less caller must resolve via the TACHI_PROFILE env fallback"
        );

        let delivered = claim_unread_stickies_for_briefing(&server, resolved.as_deref(), 5)
            .expect("claim as resolved wizard seat");
        assert!(
            delivered.is_empty(),
            "a worker seat resolved via TACHI_PROFILE must NOT consume a broadcast sticky"
        );

        // The broadcast is still there, unclaimed, waiting for the real leader.
        let leader_view = claim_unread_stickies_for_briefing(&server, None, 5)
            .expect("claim as leader (no agent_id, no env)");
        assert_eq!(
            leader_view.len(),
            1,
            "the real leader (identity-less caller) must still receive the broadcast"
        );

        let _ = std::fs::remove_file(&db_path);
    });
}

#[test]
fn cp2_identity_less_caller_resolves_to_leader() {
    with_tachi_profile_env(None, || {
        let db_path = std::env::temp_dir().join(format!(
            "sticky-cp2-identity-less-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = test_server(db_path.clone());

        // (c) identity-less caller (no param, no env) still resolves to
        // leader. Dispatch-bound worker seats get TACHI_PROFILE injected by
        // the dispatch harness (see mcp_config.rs); handcrafted worker
        // sessions calling the MCP tool directly must pass agent_id
        // explicitly (or address via `to:`) — otherwise, same as any other
        // identity-less caller, they resolve to the leader here.
        let resolved = resolve_caller_agent_id(&server, None);
        assert_eq!(
            resolved, None,
            "identity-less caller resolves to leader (None)"
        );

        let _ = std::fs::remove_file(&db_path);
    });
}
