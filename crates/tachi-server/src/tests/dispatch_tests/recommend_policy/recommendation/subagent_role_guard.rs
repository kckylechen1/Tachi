use super::*;

#[tokio::test]
async fn tachi_task_recommend_does_not_apply_same_backend_wrong_role_subagent_evidence() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("recommend-role-guard-001".to_string()),
            task: "Implement dispatch profile routing".to_string(),
            agent: "leader".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: None,
            risk: Some("high".to_string()),
            duration_ms: Some(1800),
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: Some("Executor was useful, but not review evidence.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "executor".to_string(),
                agent: "codex".to_string(),
                model: None,
                task: Some("draft implementation".to_string()),
                task_type: Some("review_request".to_string()),
                outcome: Some("useful".to_string()),
                usefulness_score: Some(1.0),
                failure_mode: None,
                verification_impact: Some("accepted".to_string()),
                verification_present: true,
                evaluator: Some("leader".to_string()),
                plan_delta: Some("accepted".to_string()),
                human_override: false,
                retry_count: 0,
                notes: None,
                latency_ms: None,
                input_tokens: None,
                output_tokens: None,
                cost_tokens: None,
                cost_usd: None,
                ..Default::default()
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-role-guard".to_string()),
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval profile routing change".to_string());
    params.risk = Some("high".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let reviewer = rec["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["profile"] == serde_json::json!("codex_55_review"))
        .expect("codex reviewer candidate");

    assert_eq!(
        reviewer["live_samples"],
        serde_json::json!(0),
        "executor subagent evidence must not count as reviewer profile evidence: {rec:#}"
    );
    assert!(
        reviewer["reason"]
            .as_array()
            .is_none_or(|reasons| !reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("live_subagent_evidence")))),
        "wrong-role subagent evidence leaked into reviewer reasons: {reviewer:#}"
    );
}
