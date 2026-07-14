//! #1001 round 2 item 1: `tachi_complete` must release the presence claim
//! its dispatch registered (`auto_register_or_heartbeat_claim`, keyed on
//! `dispatch_id`) — the module doc in `memcore::session_claims` promises
//! manual `release`, `complete`, and `cancel` all route through THE single
//! release path (`release_claim`), but before this fix the only production
//! caller was manual release; a completed dispatch's claim row stayed
//! `active` forever.

use super::*;

fn base_complete_for_dispatch(dispatch_id: &str) -> TachiCompleteParams {
    TachiCompleteParams {
        task_id: Some(format!("presence-release-{dispatch_id}")),
        task: "Round 2 item 1 discrimination".to_string(),
        agent: "claude-code".to_string(),
        outcome: "success".to_string(),
        task_type: None,
        profile: None,
        risk: None,
        duration_ms: None,
        skills_used: Vec::new(),
        cost_tokens: None,
        cost_usd: None,
        quality_score: None,
        notes: None,
        trajectory: None,
        diff: None,
        worktree: None,
        subagents: Vec::new(),
        feedback_rules_applied: Vec::new(),
        dispatch_id: Some(dispatch_id.to_string()),
        flow_id: None,
        issue_ref: None,
        pr_ref: None,
        evidence_refs: Vec::new(),
        tests_run: Vec::new(),
        diff_present: None,
        scope: Some("project".to_string()),
        project: None,
        format: Some("full".to_string()),
        signatures: Vec::new(),
        rulings: Vec::new(),
        adjudication: None,
    }
}

#[tokio::test]
async fn tachi_complete_releases_the_presence_claim_registered_for_its_dispatch_id() {
    let server = make_server();
    let dispatch_id = "disp-presence-release-001";

    // Register a claim the way dispatch.rs's zero-ceremony hook does —
    // keyed on dispatch_id.
    crate::claims_ops::auto_register_or_heartbeat_claim(
        &server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: Some("org/repo#1001".to_string()),
            flow_id: None,
            dispatch_id: Some(dispatch_id.to_string()),
            branch: Some("feat/x".to_string()),
            declared_file_scope: None,
        },
    );
    let live_before = crate::claims_ops::list_live_claims_for_briefing(&server);
    assert!(
        live_before
            .iter()
            .any(|c| c.dispatch_id.as_deref() == Some(dispatch_id)),
        "fixture sanity: claim must be live before completion"
    );

    let params = base_complete_for_dispatch(dispatch_id);
    let _ = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");

    let live_after = crate::claims_ops::list_live_claims_for_briefing(&server);
    assert!(
        !live_after
            .iter()
            .any(|c| c.dispatch_id.as_deref() == Some(dispatch_id)),
        "claim for dispatch_id={dispatch_id} must no longer be active after tachi_complete: {live_after:?}"
    );
}

#[tokio::test]
async fn tachi_complete_with_no_live_claim_for_dispatch_id_is_a_harmless_noop() {
    // #1001 fail-safe discipline: a dispatch that never got a claim
    // registered (e.g. the auto-register hook itself degraded to no-op)
    // must not make `tachi_complete` error or panic when the release path
    // finds nothing to release.
    let server = make_server();
    let params = base_complete_for_dispatch("disp-presence-release-002-no-claim");
    let result = server.tachi_complete(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "completion must succeed even with no live claim to release"
    );
}
