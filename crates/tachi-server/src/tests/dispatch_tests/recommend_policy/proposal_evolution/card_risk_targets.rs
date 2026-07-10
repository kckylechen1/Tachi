use super::*;

#[tokio::test]
async fn tachi_task_proposals_project_card_weakness_and_demotion_targets() {
    let server = make_server();

    for idx in 0..3 {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("card-risk-plan-failure-{idx}")),
                task: "Plan a dispatch card risk slice".to_string(),
                agent: "claude".to_string(),
                outcome: "failure".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:superpowers-writing-plans".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.20),
                notes: Some("Seed card weakness and demotion proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-card-risk-evolution".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: Vec::new(),
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
            }))
            .await
            .expect("seed card risk eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    let weakness_proposal = proposal_items
        .iter()
        .find(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["operation"] == json!("add_card_weakness")
                && proposal["weakness_id"] == json!("plan_request")
        })
        .expect("card weakness proposal");
    let demotion_proposal = proposal_items
        .iter()
        .find(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["operation"] == json!("mark_skill_demotion_target")
                && proposal["skill_id"] == json!("skill:superpowers-writing-plans")
        })
        .expect("skill demotion proposal");
    assert_eq!(
        weakness_proposal["proposed_patch"]["add_weak_against"][0],
        json!("plan_request")
    );
    assert_eq!(
        demotion_proposal["proposed_patch"]["demotion_targets"][0],
        json!("skill:superpowers-writing-plans")
    );

    for proposal in [weakness_proposal, demotion_proposal] {
        let proposal_id = proposal["proposal_id"].as_str().expect("proposal id");
        let mut review = task_params("review_proposal");
        review.proposal_id = Some(proposal_id.to_string());
        review.review_status = Some("approved".to_string());
        review.notes = Some("Human approved card risk projection.".to_string());
        server
            .tachi_task(Parameters(review))
            .await
            .expect("review should succeed");

        let mut apply = task_params("apply_proposals");
        apply.proposal_id = Some(proposal_id.to_string());
        apply.confirm = true;
        server
            .tachi_task(Parameters(apply))
            .await
            .expect("card risk projection should apply");
    }

    let profiles_raw = server
        .tachi_task(Parameters(task_params("profiles")))
        .await
        .expect("profiles should include card risk projections");
    let profiles: serde_json::Value = serde_json::from_str(&profiles_raw).expect("profiles JSON");
    let claude_profile = profiles["dispatch_profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan profile");
    assert!(
        claude_profile["mbit_card"]["stats"]["risk_control"]
            .as_i64()
            .expect("risk_control stat")
            > 0
    );
    assert!(claude_profile["weak_against"]
        .as_array()
        .expect("merged weak_against")
        .contains(&json!("plan_request")));
    assert!(claude_profile["mbit_card"]["projected_weak_against"]
        .as_array()
        .expect("projected weak_against")
        .contains(&json!("plan_request")));
    assert!(claude_profile["mbit_card"]["demotion_targets"]
        .as_array()
        .expect("demotion targets")
        .contains(&json!("skill:superpowers-writing-plans")));

    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("Plan a dispatch card risk slice".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
            profile: Some("claude_plan".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(2),
            include_section: Some(false),
        }))
        .await
        .expect("loadout should include projected weakness");
    let loadout: serde_json::Value = serde_json::from_str(&loadout_raw).expect("loadout JSON");
    assert_eq!(
        loadout["weak_against"], loadout["mbit_card"]["weak_against"],
        "top-level loadout weak_against should match the MBIT card"
    );
    assert!(loadout["weak_against"]
        .as_array()
        .expect("loadout weak_against")
        .contains(&json!("plan_request")));

    let mut recommend_params = task_params("recommend");
    recommend_params.task = Some("Plan a dispatch card risk slice".to_string());
    recommend_params.limit = Some(50);
    let recommend_raw = server
        .tachi_task(Parameters(recommend_params))
        .await
        .expect("recommend should include weak_against penalty");
    let recommend: serde_json::Value =
        serde_json::from_str(&recommend_raw).expect("recommend JSON");
    let claude_candidate = recommend["candidates"]
        .as_array()
        .expect("candidate list")
        .iter()
        .find(|candidate| candidate["profile"] == json!("claude_plan"))
        .expect("claude_plan candidate");
    assert!(claude_candidate["reasons"]
        .as_array()
        .expect("candidate reasons")
        .contains(&json!("weak_against_signal:plan_request")));
}
