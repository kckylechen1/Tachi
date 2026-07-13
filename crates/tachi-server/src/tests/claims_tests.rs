//! Server-layer tests for #1001 presence claims (`crate::claims_ops`) that
//! need a real `MemoryServer` — the memcore-layer lease/TTL logic itself is
//! covered by `memcore::db::session_claims::tests`, and the pure
//! `collision_warnings`/`generate_claim_id` logic by `claims_ops::tests`.
//! What's server-layer-specific and only provable with a live `MemoryServer`:
//!
//! 1. the zero-ceremony hook's degrade-to-no-op contract (#1001 Scope item 2
//!    acceptance: "claims 表损坏 → briefing 照常成功") when the underlying
//!    table is actually missing/broken, not just a guard-condition unit test;
//! 2. two sessions each seeing the other's claim through the real
//!    `MemoryServer` storage path (not just `memcore::list_active_claims`
//!    called directly).

use super::make_server;
use crate::claims_ops::{
    auto_register_or_heartbeat_claim, briefing_claims_board, emit_terminal_receipt,
    list_live_claims_for_briefing, presence_briefing_section, ClaimHookInput,
};

#[tokio::test]
async fn no_issue_dispatch_preserves_initiating_session_and_terminal_inbox_is_recipient_isolated() {
    let server_a = make_server();
    let db_path = server_a.global_db_path_buf();
    let server_b = crate::server_state::MemoryServer::new(db_path, None).expect("second server");
    server_a.set_session_identity(Some("initiator-seat".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server_a,
        &ClaimHookInput {
            issue_ref: None,
            flow_id: None,
            dispatch_id: Some("dispatch-no-issue".to_string()),
            branch: None,
            declared_file_scope: None,
        },
    );
    let claim = server_a
        .with_global_store_read(|store| {
            memcore::get_claim_for_dispatch(store.connection(), "dispatch-no-issue")
                .map_err(|e| e.to_string())
        })
        .expect("claim read")
        .expect("dispatch claim");
    assert_eq!(claim.session_client.as_deref(), Some("initiator-seat"));
    assert!(claim.issue_ref.is_none());
    assert!(
        claim.flow_id.is_none(),
        "a no-flow dispatch must retain no flow_id"
    );
    assert_eq!(claim.dispatch_id.as_deref(), Some("dispatch-no-issue"));

    auto_register_or_heartbeat_claim(
        &server_a,
        &ClaimHookInput {
            issue_ref: None,
            flow_id: None,
            dispatch_id: Some("dispatch-no-issue-two".to_string()),
            branch: None,
            declared_file_scope: None,
        },
    );
    let second_claim = server_a
        .with_global_store_read(|store| {
            memcore::get_claim_for_dispatch(store.connection(), "dispatch-no-issue-two")
                .map_err(|e| e.to_string())
        })
        .expect("second claim read")
        .expect("second dispatch claim");
    assert_eq!(
        second_claim.session_client.as_deref(),
        Some("initiator-seat")
    );
    assert!(second_claim.flow_id.is_none());

    emit_terminal_receipt(
        &server_a,
        "dispatch-no-issue",
        "TASK_STATE_FAILED",
        "safe terminal summary",
        None,
    );
    emit_terminal_receipt(
        &server_a,
        "dispatch-no-issue-two",
        "TASK_STATE_COMPLETED",
        "second safe terminal summary",
        None,
    );
    let a_list = crate::facade_memory_ops::handle_terminal_inbox(
        &server_a,
        crate::tool_params::TerminalInboxParams {
            action: "list".to_string(),
            dispatch_id: None,
            include_acknowledged: false,
            limit: 10,
        },
    )
    .expect("initiator list");
    assert!(a_list.contains("dispatch-no-issue"));
    assert!(a_list.contains("dispatch-no-issue-two"));
    server_b.set_session_identity(Some("other-seat".to_string()), None, None);
    let b_list = crate::facade_memory_ops::handle_terminal_inbox(
        &server_b,
        crate::tool_params::TerminalInboxParams {
            action: "list".to_string(),
            dispatch_id: None,
            include_acknowledged: false,
            limit: 10,
        },
    )
    .expect("other list");
    assert!(
        !b_list.contains("dispatch-no-issue"),
        "other sessions must not list another recipient's receipt"
    );

    let briefing: crate::tool_params::TachiMemoryParams =
        serde_json::from_value(serde_json::json!({
            "action": "briefing", "format": "json"
        }))
        .expect("briefing params");
    let briefing = crate::facade_memory_ops::handle_tachi_memory(&server_a, briefing)
        .await
        .expect("briefing");
    assert!(briefing.contains("terminal_inbox") && briefing.contains("dispatch-no-issue"));
    let ack = crate::facade_memory_ops::handle_terminal_inbox(
        &server_a,
        crate::tool_params::TerminalInboxParams {
            action: "ack".to_string(),
            dispatch_id: Some("dispatch-no-issue".to_string()),
            include_acknowledged: false,
            limit: 10,
        },
    )
    .expect("ack");
    assert!(ack.contains("\"acknowledged\":true"));
    let after_ack = crate::facade_memory_ops::handle_terminal_inbox(
        &server_a,
        crate::tool_params::TerminalInboxParams {
            action: "list".to_string(),
            dispatch_id: None,
            include_acknowledged: false,
            limit: 10,
        },
    )
    .expect("unacked list");
    assert!(!after_ack.contains("\"dispatch_id\":\"dispatch-no-issue\""));
    assert!(after_ack.contains("\"dispatch_id\":\"dispatch-no-issue-two\""));
}

/// Drop the `session_claims` table on the test server's global store,
/// simulating a corrupted/missing-table DB (an old DB predating #1001, or a
/// damaged file) so the hook's fail-safe path is exercised for real rather
/// than just at the guard-condition level.
fn drop_claims_table(server: &crate::server_state::MemoryServer) {
    server
        .with_global_store(|store| {
            store
                .connection_mut()
                .execute("DROP TABLE session_claims", [])
                .map_err(|e| e.to_string())
        })
        .expect("drop session_claims table for fail-safe test setup");
}

#[tokio::test]
async fn auto_register_hook_degrades_to_no_op_when_claims_table_is_missing() {
    let server = make_server();
    drop_claims_table(&server);

    // Must not panic and must not propagate an error anywhere observable —
    // the function's signature is `-> ()` precisely so a caller can't even
    // accidentally propagate a claims-layer failure.
    auto_register_or_heartbeat_claim(
        &server,
        &ClaimHookInput {
            issue_ref: Some("org/repo#1001".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: Some("feat/x".to_string()),
            declared_file_scope: None,
        },
    );

    // Board/list reads must also degrade to empty, not panic/error.
    let live = list_live_claims_for_briefing(&server);
    assert!(
        live.is_empty(),
        "read path must degrade to empty when the table is gone"
    );
    let board = briefing_claims_board(&server);
    assert_eq!(board["count"], 0);
    assert!(board["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn briefing_section_is_empty_not_erroring_when_claims_table_is_missing() {
    let server = make_server();
    drop_claims_table(&server);

    // The consolidated single call point (Scope item 3) must be exactly as
    // fail-safe as the pieces it composes — this is the literal acceptance
    // criterion from #1001: "claims 表损坏 → briefing 照常成功".
    let section = presence_briefing_section(&server, Some("org/repo#1001"));
    assert_eq!(section["board"]["count"], 0);
    assert!(section["warnings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn two_sessions_each_see_the_others_claim_through_the_real_server() {
    // Two independent `MemoryServer` handles sharing the same on-disk DB —
    // this mirrors two real sessions (Claude leader, codex session) hitting
    // the same daemon-managed global store, without racing on a process-
    // global env var the way `TACHI_AGENT_SEAT` would across concurrently
    // run tests (see `feedback_test_worktree_race` — session identity here
    // uses the per-`MemoryServer` `set_session_identity` binding instead,
    // which is the higher-priority resolution path anyway).
    let server_a = make_server();
    let db_path = server_a.global_db_path_buf();
    let server_b =
        crate::server_state::MemoryServer::new(db_path, None).expect("second server handle");

    server_a.set_session_identity(Some("claude-code-seat-a".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server_a,
        &ClaimHookInput {
            issue_ref: Some("org/repo#500".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: Some("feat/a".to_string()),
            declared_file_scope: None,
        },
    );

    server_b.set_session_identity(Some("codex-seat-b".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server_b,
        &ClaimHookInput {
            issue_ref: Some("org/repo#600".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: Some("feat/b".to_string()),
            declared_file_scope: None,
        },
    );

    // Both sessions must see both claims via their own read path.
    let live_from_a = list_live_claims_for_briefing(&server_a);
    assert_eq!(live_from_a.len(), 2, "session A must see both claims");
    let live_from_b = list_live_claims_for_briefing(&server_b);
    assert_eq!(live_from_b.len(), 2, "session B must see both claims");
    assert!(live_from_a.iter().any(
        |c| c.session_client.as_deref() == Some("claude-code-seat-a")
            && c.issue_ref.as_deref() == Some("org/repo#500")
    ));
    assert!(live_from_b
        .iter()
        .any(|c| c.session_client.as_deref() == Some("codex-seat-b")
            && c.issue_ref.as_deref() == Some("org/repo#600")));

    // The briefing board projection also reflects both.
    let board = briefing_claims_board(&server_a);
    assert_eq!(board["count"], 2);
}

#[tokio::test]
async fn collision_warning_fires_when_second_session_claims_the_same_issue() {
    let server_a = make_server();
    let db_path = server_a.global_db_path_buf();
    let server_b =
        crate::server_state::MemoryServer::new(db_path, None).expect("second server handle");

    server_a.set_session_identity(Some("seat-a".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server_a,
        &ClaimHookInput {
            issue_ref: Some("org/repo#947".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: None,
            declared_file_scope: None,
        },
    );

    // Second session (a different identity) reads the presence section
    // scoped to the SAME issue_ref before claiming it itself — this is the
    // #1001 acceptance replay of the 2026-07-11 near-miss: "第二 session 对
    // #947 发起 intake/dispatch 前,briefing/预检能给出已有 claim 警告".
    server_b.set_session_identity(Some("seat-b".to_string()), None, None);
    let section = presence_briefing_section(&server_b, Some("org/repo#947"));

    let warnings = section["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("double-claim"));
    assert!(warnings[0].as_str().unwrap().contains("seat-a"));
}

/// #1001 round 2 item 3: `presence_briefing_section` used to always pass an
/// empty `new_scope` to `collision_warnings`, which made the file-scope-
/// overlap half of that check structurally unreachable from briefing (it
/// only fires when `new_scope` is non-empty). This proves the fix: session B
/// declares a live claim with a file scope that overlaps session A's live
/// claim, and B's OWN briefing read (using its own declared scope, since a
/// read-only briefing call has no scope parameter of its own) surfaces the
/// overlap warning — the acceptance criterion the mission states verbatim:
/// "两 session 声明重叠 scope → 双方 briefing 都出预警".
#[tokio::test]
async fn briefing_surfaces_file_scope_collision_using_the_calling_sessions_own_declared_scope() {
    let server_a = make_server();
    let db_path = server_a.global_db_path_buf();
    let server_b =
        crate::server_state::MemoryServer::new(db_path, None).expect("second server handle");

    server_a.set_session_identity(Some("seat-a".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server_a,
        &ClaimHookInput {
            issue_ref: Some("org/repo#1001".to_string()),
            flow_id: Some("flow-a".to_string()),
            dispatch_id: None,
            branch: Some("feat/a".to_string()),
            declared_file_scope: Some(vec!["crates/tachi-server/src/claims_ops.rs".to_string()]),
        },
    );

    server_b.set_session_identity(Some("seat-b".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server_b,
        &ClaimHookInput {
            issue_ref: Some("org/repo#1002".to_string()),
            flow_id: Some("flow-b".to_string()),
            dispatch_id: None,
            branch: Some("feat/b".to_string()),
            declared_file_scope: Some(vec!["crates/tachi-server/src/claims_ops.rs".to_string()]),
        },
    );

    // B reads its own briefing (no explicit issue_ref match to A's — this is
    // a file-scope collision, not a double-claim-on-the-same-issue one).
    let section = presence_briefing_section(&server_b, Some("org/repo#1002"));
    let warnings = section["warnings"].as_array().unwrap();
    assert_eq!(
        warnings.len(),
        1,
        "must surface exactly the file-scope overlap, no double-claim (different issue_refs): {warnings:?}"
    );
    assert!(warnings[0].as_str().unwrap().contains("file-scope overlap"));
    assert!(warnings[0].as_str().unwrap().contains("seat-a"));
    // #1023: sanitize_presence_field now ESCAPES metachars (fidelity
    // preserved) instead of deleting them, so the underscore in the real
    // filename survives as a self-escaping `\_` pair rather than as a bare
    // `_` (which round 4 replaced with a space, breaking this exact
    // assertion — this is the discriminating fix: pre-fix the substring
    // check below would have failed against `claims ops.rs`).
    assert!(warnings[0].as_str().unwrap().contains("claims\\_ops.rs"));

    // And session A's own briefing symmetrically surfaces the same overlap
    // against B.
    let section_a = presence_briefing_section(&server_a, Some("org/repo#1001"));
    let warnings_a = section_a["warnings"].as_array().unwrap();
    assert_eq!(warnings_a.len(), 1);
    assert!(warnings_a[0]
        .as_str()
        .unwrap()
        .contains("file-scope overlap"));
    assert!(warnings_a[0].as_str().unwrap().contains("seat-b"));
}

/// A session's own live claim must never be reported as colliding with
/// itself once `presence_briefing_section` starts passing a real scope
/// through (self-exclusion parity with `handle_manual_claim`).
#[tokio::test]
async fn briefing_does_not_self_collide_on_its_own_declared_scope() {
    let server = make_server();
    server.set_session_identity(Some("solo-seat".to_string()), None, None);
    auto_register_or_heartbeat_claim(
        &server,
        &ClaimHookInput {
            issue_ref: Some("org/repo#2000".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: None,
            declared_file_scope: Some(vec!["crates/foo/src/lib.rs".to_string()]),
        },
    );

    let section = presence_briefing_section(&server, Some("org/repo#2000"));
    let warnings = section["warnings"].as_array().unwrap();
    assert!(
        warnings.is_empty(),
        "a session must never see its own claim reported as a collision: {warnings:?}"
    );
}

/// #527 CONCERN: an unbounded presence board grows every briefing payload
/// linearly with fleet size. Six live claims (distinct `issue_ref`s under
/// one session — enough to produce 6 distinct claim rows, since a claim's
/// identity key is `(session_client, issue_ref, flow_id)`) must render as
/// exactly 5 board rows plus an `overflow: 1` marker — never a silent 6th
/// row, and never a silently-dropped count either.
#[tokio::test]
async fn presence_board_caps_at_five_and_reports_overflow() {
    let server = make_server();
    server.set_session_identity(Some("seat-cap".to_string()), None, None);
    for n in 0..6 {
        auto_register_or_heartbeat_claim(
            &server,
            &ClaimHookInput {
                issue_ref: Some(format!("org/repo#{n}")),
                flow_id: None,
                dispatch_id: None,
                branch: None,
                declared_file_scope: None,
            },
        );
    }

    let board = briefing_claims_board(&server);
    assert_eq!(
        board["count"], 6,
        "count must report the TOTAL live-claim count, not just what's shown: {board:?}"
    );
    let items = board["items"].as_array().unwrap();
    assert_eq!(items.len(), 5, "items must be capped at 5: {board:?}");
    assert_eq!(
        board["overflow"], 1,
        "overflow must report exactly the cut-off count: {board:?}"
    );
}

/// Discrimination for `emit_terminal_receipt`'s non-propagation contract: an
/// actual receipt-write failure must NOT prevent the caller's terminal
/// transition from completing. The function's signature is `-> ()` so it
/// structurally cannot propagate an error; this test proves that by inducing a
/// REAL receipt-INSERT failure (a SQLite BEFORE INSERT trigger with
/// RAISE(ABORT) on `terminal_dispatch_inbox`) and showing:
///   1. `emit_terminal_receipt` returns normally (no panic),
///   2. the caller's immediately-following `release_claim_for_dispatch`
///      still succeeds (the terminal transition is usable),
///   3. no receipt is listed as delivered.
/// If `emit_terminal_receipt` were ever changed to return `Result` and the
/// callers propagated it with `?`, the release below would be skipped — this
/// test would fail at the claim-state assertion.
#[tokio::test]
async fn emit_terminal_receipt_failure_remains_non_propagating_and_caller_continues() {
    let server = make_server();
    server.set_session_identity(Some("receipt-fault-seat".to_string()), None, None);
    let dispatch_id = "dispatch-receipt-write-fault";

    // Register a claim so emit_terminal_receipt has a recipient.
    auto_register_or_heartbeat_claim(
        &server,
        &ClaimHookInput {
            issue_ref: None,
            flow_id: None,
            dispatch_id: Some(dispatch_id.to_string()),
            branch: None,
            declared_file_scope: None,
        },
    );
    let claim_before = server
        .with_global_store_read(|store| {
            memcore::get_claim_for_dispatch(store.connection(), dispatch_id)
                .map_err(|e| e.to_string())
        })
        .expect("claim read")
        .expect("claim seeded");
    assert_eq!(claim_before.state, memcore::ClaimState::Active);

    // Induce a REAL receipt-INSERT failure via a BEFORE INSERT trigger.
    server
        .with_global_store(|store| {
            store
                .connection_mut()
                .execute_batch(
                    "CREATE TRIGGER test_block_receipt_insert BEFORE INSERT ON terminal_dispatch_inbox \
                     BEGIN SELECT RAISE(ABORT, 'test-induced receipt write failure'); END;",
                )
                .map_err(|e| e.to_string())
        })
        .expect("create receipt-blocking trigger");

    // Prove the trigger is live: a direct insert must fail.
    let blocked = server.with_global_store(|store| {
        memcore::insert_terminal_receipt(
            store.connection(),
            &memcore::NewTerminalReceipt {
                dispatch_id: "probe-blocked".to_string(),
                recipient_session_client: "receipt-fault-seat".to_string(),
                terminal_state: "TASK_STATE_FAILED".to_string(),
                safe_summary: "probe".to_string(),
                reference: None,
                created_at: String::new(),
            },
        )
        .map_err(|e| e.to_string())
    });
    assert!(
        blocked.is_err(),
        "trigger must genuinely block receipt inserts, got: {blocked:?}"
    );

    // The action under test: emit_terminal_receipt must not propagate the
    // write failure. It returns `-> ()`, so the caller always continues.
    emit_terminal_receipt(
        &server,
        dispatch_id,
        "TASK_STATE_FAILED",
        "safe summary for fault test",
        None,
    );

    // PROOF 1 — the caller's terminal transition remains usable: the claim
    // release (which every terminal caller invokes right after emit) still
    // succeeds despite the receipt write failure.
    crate::claims_ops::release_claim_for_dispatch(&server, dispatch_id, "test_terminal");
    let claim_after = server
        .with_global_store_read(|store| {
            memcore::get_claim_for_dispatch(store.connection(), dispatch_id)
                .map_err(|e| e.to_string())
        })
        .expect("claim read after")
        .expect("claim row retained");
    assert_eq!(
        claim_after.state,
        memcore::ClaimState::Released,
        "claim release must succeed despite receipt write failure — the terminal transition is still usable"
    );

    // PROOF 2 — no receipt is listed as delivered for this dispatch.
    let receipt = server
        .with_global_store_read(|store| {
            memcore::get_terminal_receipt(store.connection(), dispatch_id)
                .map_err(|e| e.to_string())
        })
        .expect("receipt read");
    assert!(
        receipt.is_none(),
        "no receipt must be reported delivered when the write genuinely failed"
    );

    // PROOF 3 — list_terminal_receipts for the recipient returns empty.
    let listed = server
        .with_global_store_read(|store| {
            memcore::list_terminal_receipts(store.connection(), "receipt-fault-seat", true, 10)
                .map_err(|e| e.to_string())
        })
        .expect("list receipts");
    assert!(
        listed.iter().all(|r| r.dispatch_id != dispatch_id),
        "failed receipt must not appear in the recipient's delivered list"
    );
}
