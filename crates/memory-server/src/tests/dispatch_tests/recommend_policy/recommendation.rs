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
            evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval profile routing change".to_string());
    params.risk = Some("high".to_string());
    params.limit = Some(50);
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
async fn tachi_task_recommend_falls_back_to_builtin_profiles_without_eval_rows() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a low-risk documentation update".to_string());
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed without eval rows");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert!(rec["recommended_profile"].as_str().is_some());
    assert!(
        rec.as_object()
            .is_some_and(|obj| obj.contains_key("recommended_model")),
        "recommend should surface model choice: {rec:#}"
    );
    assert!(rec["recommended_transport"].as_str().is_some());
    assert_eq!(rec["live_eval"]["row_count"], serde_json::json!(0));
    assert!(
        rec["evidence_note"]
            .as_str()
            .is_some_and(|note| note.contains("low_sample_fallback")),
        "expected fallback note: {rec:#}"
    );
    assert!(rec["mbit_card"].is_object());
}

#[tokio::test]
async fn tachi_task_recommend_surfaces_kimi_ux_for_agent_experience_tasks() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some(
        "Run an agent-facing UX 大满贯 test for tachi_arena and summarize tool surface friction"
            .to_string(),
    );
    params.limit = Some(20);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    let candidates = rec["candidates"].as_array().expect("candidates");
    let kimi_ux = candidates
        .iter()
        .find(|candidate| candidate["profile"] == serde_json::json!("kimi_ux"))
        .expect("kimi_ux candidate");
    assert!(
        kimi_ux["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| {
                reason
                    .as_str()
                    .is_some_and(|s| s.contains("role_matches_agent_facing_ux"))
            })),
        "kimi_ux should explain UX routing fit: {kimi_ux:#}"
    );
    assert!(
        candidates
            .iter()
            .take(3)
            .any(|candidate| candidate["profile"] == serde_json::json!("kimi_ux")),
        "kimi_ux should be near the top for agent-facing UX tasks: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_consumes_approved_route_policy_rules() {
    let server = make_server();

    for (task_id, profile, outcome, cost_usd, quality_score) in [
        (
            "rule-loader-cheap-success",
            "opencode_builder",
            "success",
            0.01,
            0.75,
        ),
        (
            "rule-loader-cheap-failure",
            "opencode_builder",
            "failure",
            0.01,
            0.20,
        ),
        (
            "rule-loader-quality-success",
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
                notes: Some("Seed route policy rule loader fixture.".to_string()),
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-route-policy-loader".to_string()),
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
    let proposals_raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value =
        serde_json::from_str(&proposals_raw).expect("proposals JSON");
    let proposal_id = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["policy"] == json!("cost_sensitive")
                    && proposal["task_type"] == json!("fix_request")
                    && proposal["proposed_profile"] == json!("opencode_builder")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("cost-sensitive opencode proposal")
        .to_string();

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    server
        .tachi_task(Parameters(review))
        .await
        .expect("review should succeed");

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply))
        .await
        .expect("apply should succeed");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply JSON");
    assert_eq!(applied["routing_mutated"], json!(true));

    let mut recommend = task_params("recommend");
    recommend.task = Some("fix dispatch policy proposal flow bug".to_string());
    recommend.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(recommend))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["recommended_profile"], json!("opencode_builder"));
    assert!(rec["evidence_note"]
        .as_str()
        .is_some_and(|note| note.contains("route_policy_weighted")));
    assert!(rec["route_policy_rules"]["applied"]
        .as_array()
        .is_some_and(|rules| rules
            .iter()
            .any(|rule| rule["proposal_id"] == json!(proposal_id))));
    let opencode_candidate = rec["candidates"]
        .as_array()
        .and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["profile"] == json!("opencode_builder"))
        })
        .expect("opencode candidate should be present");
    assert!(
        opencode_candidate["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("approved_route_policy_rule")))),
        "approved route policy rule should be visible in candidate reasons: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_skips_route_policy_rules_blocked_by_risk() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    "dispatch_route_policy_rules",
                    "route_policy:test_request:codex_53_fast",
                    &json!({
                        "proposal_id": "route_policy:test_request:codex_53_fast",
                        "kind": "route_policy",
                        "status": "applied",
                        "review": {
                            "status": "approved",
                            "reviewed_at": Utc::now().to_rfc3339(),
                        },
                        "policy": "cost_sensitive",
                        "task_type": "test_request",
                        "proposed_profile": "codex_53_fast",
                        "score_delta": 99.0,
                        "policy_rule": {
                            "when_task_type": "test_request",
                            "prefer_profile": "codex_53_fast",
                            "policy": "cost_sensitive",
                            "fallback_to_current_profile": "codex_55_review",
                        },
                        "evidence": {
                            "source": "test",
                            "row_count": 5,
                            "proposed": {
                                "samples": 5
                            }
                        }
                    })
                    .to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed blocked route policy rule");

    let mut params = task_params("recommend");
    params.task = Some("test vault crypto migration regression".to_string());
    params.doc_paths =
        vec!["docs/engineering/architecture/agent-credential-surfaces.md".to_string()];
    params.spec_paths = vec!["crates/memory-server/src/vault_crypto.rs".to_string()];
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_ne!(rec["recommended_profile"], json!("codex_53_fast"));
    assert!(rec["route_policy_rules"]["applied"]
        .as_array()
        .is_some_and(|rules| rules.is_empty()));
    assert!(rec["route_policy_rules"]["skipped"]
        .as_array()
        .is_some_and(|rules| rules.iter().any(|rule| rule["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("blocked_by_risk_classifier")))));
}

#[tokio::test]
async fn tachi_task_recommend_escalates_risk_from_doc_and_spec_paths() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.doc_paths =
        vec!["docs/engineering/architecture/agent-credential-surfaces.md".to_string()];
    params.spec_paths = vec!["crates/memory-server/src/vault_crypto.rs".to_string()];
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed with file context");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["risk"], serde_json::json!("high"));
    assert!(rec["risk_reasons"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| reason
            .as_str()
            .is_some_and(|s| s == "touches vault/secrets boundary"))));
    assert!(rec["blocked_profiles"]
        .as_array()
        .is_some_and(|profiles| profiles.iter().any(|profile| profile == "codex_53_fast")));
    assert!(
        rec["recommended_profile"] != serde_json::json!("codex_53_fast"),
        "high-risk path context must not recommend the fast lane: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_routes_low_risk_review_to_fast_checker_without_live_rows() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("review low-risk docs wording".to_string());
    params.risk = Some("low".to_string());
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["task_type"], json!("review_request"));
    assert_eq!(rec["risk"], json!("low"));
    assert_eq!(rec["recommended_profile"], json!("codex_53_fast"));
    assert!(rec["required_profiles"]
        .as_array()
        .expect("required profiles")
        .contains(&json!("codex_53_fast")));
    assert!(rec["blocked_profiles"]
        .as_array()
        .expect("blocked profiles")
        .is_empty());
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
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-194".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec!["human review".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval routing change".to_string());
    params.risk = Some("high".to_string());
    params.limit = Some(50);
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
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-role-guard".to_string()),
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
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
