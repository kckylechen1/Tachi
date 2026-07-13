//! #774 round 2 discriminator: `dispatch_outcomes.reported_outcome` must be
//! the agent's VERBATIM `tachi_complete` claim (trim only — no case-folding),
//! distinct from the normalized value machine-side bucketing logic uses.
//! Before this fix `handle_tachi_complete` passed the lowercased
//! `outcome_norm` into `record_complete_outcome`'s `reported_outcome`
//! argument, silently rewriting e.g. `"Complete "` -> `"complete"` in the row
//! the module doc (`memcore::db::dispatch_outcomes`) promises is verbatim.

use super::*;

fn base_dual_truth_params(dispatch_id: &str, outcome: &str) -> TachiCompleteParams {
    TachiCompleteParams {
        task_id: Some(format!("dual-truth-{dispatch_id}")),
        task: "#774 round 2 dual-truth discrimination".to_string(),
        agent: "codex".to_string(),
        outcome: outcome.to_string(),
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
        // Explicit "global" so the assertion below reads a deterministic
        // store regardless of whether this test server has a project DB.
        scope: Some("global".to_string()),
        project: None,
        format: None,
        signatures: Vec::new(),
        rulings: Vec::new(),
        adjudication: None,
    }
}

#[tokio::test]
async fn reported_outcome_preserves_case_and_whitespace_trim_only() {
    let server = make_server();
    let dispatch_id = "disp-dual-truth-001";

    let params = base_dual_truth_params(dispatch_id, "Complete ");
    server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed even with mixed-case/whitespace outcome");

    let rows = server
        .with_global_store_read(|store| {
            memcore::list_outcomes_by_vendor_window(
                store.connection(),
                "codex",
                "1970-01-01T00:00:00Z",
                None,
                memcore::OutcomeEvidenceClass::AnyAttribution,
            )
            .map_err(|e| e.to_string())
        })
        .expect("read outcomes");
    let row = rows
        .iter()
        .find(|r| r.dispatch_id == dispatch_id)
        .expect("canonical outcome row present for this dispatch_id");

    assert_eq!(
        row.reported_outcome.as_deref(),
        Some("Complete"),
        "reported_outcome must be the agent's verbatim claim (trim only, \
         no case-folding) — NOT the lowercased outcome_norm bucketing value"
    );
    assert_ne!(
        row.execution_outcome, "Complete",
        "execution_outcome is the machine-resolved value, in the machine's \
         own vocabulary — it must never just echo the raw self-report"
    );
}

#[tokio::test]
async fn reported_outcome_matches_verbatim_for_canonical_success() {
    let server = make_server();
    let dispatch_id = "disp-dual-truth-002";

    let params = base_dual_truth_params(dispatch_id, "success");
    server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");

    let rows = server
        .with_global_store_read(|store| {
            memcore::list_outcomes_by_vendor_window(
                store.connection(),
                "codex",
                "1970-01-01T00:00:00Z",
                None,
                memcore::OutcomeEvidenceClass::AnyAttribution,
            )
            .map_err(|e| e.to_string())
        })
        .expect("read outcomes");
    let row = rows
        .iter()
        .find(|r| r.dispatch_id == dispatch_id)
        .expect("canonical outcome row present for this dispatch_id");

    // A canonical, already-lowercase self-report round-trips unchanged —
    // this is the baseline sanity check alongside the mixed-case
    // discriminator above.
    assert_eq!(row.reported_outcome.as_deref(), Some("success"));
    assert_eq!(row.execution_outcome, "completed");
}
