use super::*;

#[test]
fn route_policy_simulation_sinks_non_finite_scores() {
    let rows = vec![
        AgentPerformanceMatrixRow {
            scope: "leader".to_string(),
            profile: Some("claude_plan".to_string()),
            role: Some("planner".to_string()),
            agent: "claude".to_string(),
            task_type: "review_request".to_string(),
            samples: 10,
            success_rate: Some(0.95),
            verification_rate: 1.0,
            avg_quality_score: Some(f64::NAN),
            ..Default::default()
        },
        AgentPerformanceMatrixRow {
            scope: "leader".to_string(),
            profile: Some("codex_55_review".to_string()),
            role: Some("reviewer".to_string()),
            agent: "codex".to_string(),
            task_type: "review_request".to_string(),
            samples: 10,
            success_rate: Some(0.90),
            verification_rate: 1.0,
            avg_quality_score: Some(0.90),
            ..Default::default()
        },
    ];

    let summary = simulate_route_policy("quality_first", &rows, None);

    assert_eq!(summary.route_choices.len(), 1);
    assert_eq!(summary.route_choices[0].profile, "codex_55_review");
    assert!(summary.route_choices[0].score.is_finite());
}

#[test]
fn load_route_policy_rule_loadout_classifies_persisted_rules() {
    let db_path = std::env::temp_dir().join(format!(
        "dispatch-route-policy-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("test memory server");
    let min_samples = tachi_dispatch::MIN_ROUTE_POLICY_RULE_SAMPLES;

    for (id, task_type, prefer_profile, samples) in [
        (
            "route_policy:fix_request:opencode_builder",
            "fix_request",
            "opencode_builder",
            min_samples,
        ),
        (
            "route_policy:review_request:codex_55_review",
            "review_request",
            "codex_55_review",
            min_samples,
        ),
        (
            "route_policy:fix_request:glm_51_impl_sparse",
            "fix_request",
            "glm_51_impl",
            0,
        ),
        (
            "route_policy:fix_request:codex_53_fast_blocked",
            "fix_request",
            "codex_53_fast",
            min_samples,
        ),
    ] {
        let rule = serde_json::json!({
            "proposal_id": id,
            "kind": "route_policy",
            "status": "applied",
            "review": {
                "status": "approved",
            },
            "policy": "cost_sensitive",
            "task_type": task_type,
            "proposed_profile": prefer_profile,
            "score_delta": 12.5,
            "policy_rule": {
                "when_task_type": task_type,
                "prefer_profile": prefer_profile,
                "policy": "cost_sensitive",
                "fallback_to_current_profile": "claude_plan",
            },
            "evidence": {
                "source": "test",
                "proposed": {
                    "samples": samples,
                },
            },
        });
        server
            .with_global_store(|store| {
                store
                    .set_state(ROUTE_POLICY_RULE_NS, id, &rule.to_string())
                    .map_err(|err| err.to_string())
            })
            .expect("seed route policy rule");
    }

    let risk = DispatchRisk {
        task_type: "fix_request".to_string(),
        risk: "critical".to_string(),
        reasons: vec!["test fixture".to_string()],
        required_profiles: Vec::new(),
        blocked_profiles: vec!["codex_53_fast".to_string()],
    };

    let loadout = load_route_policy_rule_loadout(&server, &risk).expect("load route policy rules");

    assert_eq!(loadout.applied.len(), 1, "{loadout:#?}");
    assert_eq!(
        loadout.applied[0].proposal_id,
        "route_policy:fix_request:opencode_builder"
    );
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:review_request:codex_55_review"
        && rule.reason.contains("task_type_mismatch:review_request")));
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:fix_request:glm_51_impl_sparse"
        && rule.reason.contains("insufficient_samples:0<")));
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:fix_request:codex_53_fast_blocked"
        && rule
            .reason
            .contains("blocked_by_risk_classifier:codex_53_fast")));
}
