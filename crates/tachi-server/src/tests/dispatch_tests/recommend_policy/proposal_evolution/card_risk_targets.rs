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
                eval_run_ids: Vec::new(),
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
                adjudication: None,
            }))
            .await
            .expect("seed card risk eval row");
    }

    let mut proposal_params = tune_params("route_proposals");
    proposal_params.limit = Some(50);
    let raw = run_tune(&server, proposal_params)
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

    // #1431 binds each proposal to the profile/card overlay revision it was minted
    // against and refuses on drift, so applying the first of these two moves the
    // overlay and legitimately staleness-refuses the second. Re-mint per
    // iteration by the proposal's semantic identity — the same thing an operator
    // has to do — instead of reusing ids from the single pre-loop `proposals`
    // call.
    for (operation, key_field, key_value) in [
        ("add_card_weakness", "weakness_id", "plan_request"),
        (
            "mark_skill_demotion_target",
            "skill_id",
            "skill:superpowers-writing-plans",
        ),
    ] {
        let mut remint = tune_params("route_proposals");
        remint.limit = Some(50);
        let remint_raw = run_tune(&server, remint)
            .await
            .expect("re-mint proposals should succeed");
        let reminted: serde_json::Value =
            serde_json::from_str(&remint_raw).expect("re-mint proposals JSON");
        let proposal_id = reminted["proposals"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|proposal| {
                    proposal["kind"] == json!("loadout_evolution")
                        && proposal["profile"] == json!("claude_plan")
                        && proposal["operation"] == json!(operation)
                        && proposal[key_field] == json!(key_value)
                })
            })
            .and_then(|proposal| proposal["proposal_id"].as_str())
            .unwrap_or_else(|| panic!("re-minted {operation} proposal must exist"))
            .to_string();
        let proposal_id = proposal_id.as_str();
        let mut review = tune_params("route_review");
        review.proposal_id = Some(proposal_id.to_string());
        review.review_status = Some("approved".to_string());
        review.notes = Some("Human approved card risk projection.".to_string());
        run_tune(&server, review)
            .await
            .expect("review should succeed");

        let mut apply = tune_params("route_apply");
        apply.proposal_id = Some(proposal_id.to_string());
        apply.confirm = true;
        run_tune(&server, apply)
            .await
            .expect("card risk projection should apply");
    }

    // tachi#1173 item 2 slimmed action='profiles' rows to name/backend/model/role
    // by default; this assertion needs the full mbit_card/weak_against, so
    // request the verbose escape hatch explicitly (#1182 consumer sweep).
    let mut profiles_params = task_params("profiles");
    profiles_params.verbose = Some(true);
    let profiles_raw = server
        .tachi_task(Parameters(profiles_params))
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

}
