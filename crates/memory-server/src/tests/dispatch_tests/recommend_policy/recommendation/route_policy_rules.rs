use super::*;

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
                format: None,
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
