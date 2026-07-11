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
    auto_register_or_heartbeat_claim, briefing_claims_board, list_live_claims_for_briefing,
    presence_briefing_section, ClaimHookInput,
};

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
