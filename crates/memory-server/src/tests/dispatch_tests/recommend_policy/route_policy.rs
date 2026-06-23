use super::*;

#[tokio::test]
async fn tachi_task_route_simulate_compares_policy_variants_from_live_eval() {
    let server = make_server();

    for (task_id, profile, agent, outcome, duration_ms, cost_usd, quality_score) in [
        (
            "simulate-cheap-success",
            "opencode_builder",
            "custom",
            "success",
            100_000,
            0.01,
            0.75,
        ),
        (
            "simulate-cheap-failure",
            "opencode_builder",
            "custom",
            "failure",
            100_000,
            0.01,
            0.20,
        ),
        (
            "simulate-quality-success",
            "glm_51_impl",
            "custom",
            "success",
            800_000,
            2.00,
            0.98,
        ),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "Implement dispatch policy replay".to_string(),
                agent: agent.to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(duration_ms),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost_usd),
                quality_score: Some(quality_score),
                notes: Some("Seed route simulation fixture.".to_string()),
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-route-sim".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed eval row");
    }

    let mut params = task_params("route_simulate");
    params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("route_simulate should succeed");
    let sim: serde_json::Value = serde_json::from_str(&raw).expect("route simulation JSON");

    assert_eq!(sim["action"], json!("route_simulate"));
    assert_eq!(sim["read_only"], json!(true));
    assert_eq!(sim["row_count"], json!(3));
    let policies = sim["policies"].as_array().expect("policies");
    assert_eq!(policies.len(), 3);
    let cost_sensitive = policies
        .iter()
        .find(|policy| policy["policy"] == json!("cost_sensitive"))
        .expect("cost_sensitive policy");
    let quality_first = policies
        .iter()
        .find(|policy| policy["policy"] == json!("quality_first"))
        .expect("quality_first policy");
    let cost_choice = cost_sensitive["route_choices"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice["task_type"] == json!("fix_request"))
        })
        .expect("cost-sensitive fix route");
    let quality_choice = quality_first["route_choices"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice["task_type"] == json!("fix_request"))
        })
        .expect("quality-first fix route");

    assert_eq!(cost_choice["profile"], json!("opencode_builder"));
    assert_eq!(quality_choice["profile"], json!("glm_51_impl"));
    assert!(
        cost_sensitive["caveats"]
            .as_array()
            .is_some_and(|caveats| caveats.iter().any(|caveat| caveat
                .as_str()
                .is_some_and(|s| s.contains("low sample count")))),
        "low sample caveat should be visible: {sim:#}"
    );
}

#[tokio::test]
async fn tachi_task_route_policy_proposals_require_review_before_apply() {
    let server = make_server();

    for (task_id, profile, outcome, cost_usd, quality_score) in [
        (
            "proposal-cheap-success",
            "opencode_builder",
            "success",
            0.01,
            0.75,
        ),
        (
            "proposal-cheap-failure",
            "opencode_builder",
            "failure",
            0.01,
            0.20,
        ),
        (
            "proposal-quality-success",
            "glm_51_impl",
            "success",
            2.00,
            0.98,
        ),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "Implement dispatch policy proposal flow".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost_usd),
                quality_score: Some(quality_score),
                notes: Some("Seed route policy proposal fixture.".to_string()),
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-route-policy-proposals".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| items.first())
        .expect("at least one proposal");
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal_id")
        .to_string();
    assert_eq!(proposal["status"], json!("pending"));
    assert_eq!(proposal["requires_human_approval"], json!(true));

    let mut premature_apply = task_params("apply_proposals");
    premature_apply.proposal_id = Some(proposal_id.clone());
    premature_apply.confirm = true;
    let err = server
        .tachi_task(Parameters(premature_apply))
        .await
        .expect_err("pending proposal must not apply");
    assert!(err.contains("must be approved before apply"));

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("Human approved cost-sensitive route rule.".to_string());
    let reviewed_raw = server
        .tachi_task(Parameters(review))
        .await
        .expect("review should succeed");
    let reviewed: serde_json::Value = serde_json::from_str(&reviewed_raw).expect("review JSON");
    assert_eq!(reviewed["proposal"]["status"], json!("approved"));

    let mut missing_confirm = task_params("apply_proposals");
    missing_confirm.proposal_id = Some(proposal_id.clone());
    let err = server
        .tachi_task(Parameters(missing_confirm))
        .await
        .expect_err("apply requires confirm");
    assert!(err.contains("confirm=true"));

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply))
        .await
        .expect("apply should succeed");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply JSON");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["proposal"]["status"], json!("applied"));
    assert_eq!(
        applied["rule_namespace"],
        json!("dispatch_route_policy_rules")
    );
}
