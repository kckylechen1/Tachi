use super::*;

#[tokio::test]
async fn tachi_task_recommend_uses_live_eval_and_dispatch_profiles() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("recommend-review-001".to_string()),
            task: "Review dispatch profile implementation".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(1200),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.95),
            notes: Some("Found no blockers after verification.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-194".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
            eval_run_ids: Vec::new(),
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval profile routing change".to_string());
    params.risk = Some("high".to_string());
    params.limit = Some(50);
    // tachi#1201 item 2: the default JSON candidate row is slimmed to
    // profile/role/score/reasons; this test asserts on dropped per-candidate
    // telemetry fields (performance_samples/human_override_rate/
    // avg_retry_count/avg_latency_ms), so request the unslimmed escape hatch.
    params.format = Some("full".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(
        rec["recommended_profile"],
        serde_json::json!("codex_55_review")
    );
    assert_eq!(rec["risk"], serde_json::json!("high"));
    assert!(rec["fallback_chain"]
        .as_array()
        .is_some_and(|chain| chain.iter().any(|item| item == "codex_55_review")));
    assert!(
        rec["live_eval"]["matched_samples"].as_u64().unwrap_or(0) >= 1,
        "expected live eval evidence in recommendation: {rec:#}"
    );
    assert!(
        rec["live_eval"]["performance_matrix_hits"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "expected live performance matrix evidence in recommendation: {rec:#}"
    );
    assert!(rec["reason"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| reason
            .as_str()
            .is_some_and(|s| s.contains("live_useful_rate")))));
    assert!(rec["resolved_skills"]
        .as_array()
        .is_some_and(|skills| skills
            .iter()
            .any(|skill| skill == "skill:superpowers-requesting-code-review")));
    assert_eq!(
        rec["resolved_skill_loadout"]["passive_traits"][0],
        serde_json::json!("strict_on_missing_tests")
    );
    let codex_candidate = rec["candidates"]
        .as_array()
        .and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["profile"] == "codex_55_review")
        })
        .expect("codex candidate should be present");
    assert_eq!(codex_candidate["performance_samples"], serde_json::json!(1));
    assert_eq!(
        codex_candidate["human_override_rate"],
        serde_json::json!(0.0)
    );
    assert_eq!(codex_candidate["avg_retry_count"], serde_json::json!(0.0));
    assert_eq!(codex_candidate["avg_latency_ms"], serde_json::json!(1200.0));
}

#[tokio::test]
async fn tachi_task_recommend_surfaces_human_override_and_retry_penalties() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("recommend-override-retry-001".to_string()),
            task: "Review dispatch/eval routing regression".to_string(),
            agent: "leader".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(9000),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: Some(2000),
            cost_usd: Some(0.05),
            quality_score: Some(0.85),
            notes: Some("Reviewer needed human correction and retries.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "senior_reviewer".to_string(),
                agent: "codex".to_string(),
                model: None,
                task: Some("review dispatch policy".to_string()),
                task_type: Some("review_request".to_string()),
                outcome: Some("useful".to_string()),
                usefulness_score: Some(0.7),
                failure_mode: None,
                verification_impact: Some("modified".to_string()),
                verification_present: true,
                evaluator: Some("leader".to_string()),
                plan_delta: Some("modified".to_string()),
                human_override: true,
                retry_count: 3,
                notes: None,
                latency_ms: Some(8000),
                input_tokens: Some(1500),
                output_tokens: Some(500),
                cost_tokens: Some(2000),
                cost_usd: Some(0.05),
                ..Default::default()
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-194".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec!["human review".to_string()],
            tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
            eval_run_ids: Vec::new(),
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval routing change".to_string());
    params.risk = Some("high".to_string());
    params.limit = Some(50);
    // tachi#1201 item 2: see the sibling test above — this one also asserts
    // on dropped per-candidate telemetry (performance_samples/
    // human_override_rate/avg_retry_count), so request the unslimmed shape.
    params.format = Some("full".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let codex_candidate = rec["candidates"]
        .as_array()
        .and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["profile"] == "codex_55_review")
        })
        .expect("codex candidate should be present");

    assert_eq!(codex_candidate["performance_samples"], serde_json::json!(2));
    assert!(
        codex_candidate["human_override_rate"]
            .as_f64()
            .is_some_and(|rate| rate > 0.0),
        "human override telemetry should be surfaced: {codex_candidate:#}"
    );
    assert!(
        codex_candidate["avg_retry_count"]
            .as_f64()
            .is_some_and(|count| count > 0.0),
        "retry telemetry should be surfaced: {codex_candidate:#}"
    );
    assert!(
        codex_candidate["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("perf_human_override_rate")))),
        "human override should affect routing reasons: {codex_candidate:#}"
    );
    assert!(
        codex_candidate["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("perf_avg_retry_count")))),
        "retry should affect routing reasons: {codex_candidate:#}"
    );
}
