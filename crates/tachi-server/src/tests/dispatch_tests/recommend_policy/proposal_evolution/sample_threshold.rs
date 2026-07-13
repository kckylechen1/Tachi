use super::*;

#[tokio::test]
async fn tachi_task_proposals_requires_loadout_evolution_sample_threshold() {
    let server = make_server();

    for idx in 0..9 {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-proposal-below-threshold-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: "claude".to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec![
                    "skill:superpowers-writing-plans".to_string(),
                    "skill:planning-ux-review".to_string(),
                ],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed below-threshold loadout proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-evolution-threshold".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,}))
            .await
            .expect("seed below-threshold loadout eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["skill_id"] == json!("skill:planning-ux-review")
        }),
        "loadout evolution proposal should require at least 10 profile samples: {proposal_items:?}"
    );
}
