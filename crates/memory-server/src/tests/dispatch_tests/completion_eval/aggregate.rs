use super::*;

#[tokio::test]
async fn aggregate_live_filters_auto_synthesized_watchdog_rows() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("real-eval-001".to_string()),
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
            quality_score: Some(0.9),
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            evidence_refs: Vec::new(),
            tests_run: vec!["cargo test -p memory-server dispatch_tests".to_string()],
            diff_present: Some(true),
            scope: Some("project".to_string()),
            flow_id: None,
            dispatch_id: None,
            issue_ref: None,
            pr_ref: None,
            project: None,
            format: None,
        }))
        .await
        .expect("real eval should save");

    server
        .with_global_store(|store| {
            let mut synthesized = make_entry("auto-synth-eval-row");
            synthesized.path = "/eval/auto-synth-eval-row".to_string();
            synthesized.category = "eval".to_string();
            synthesized.summary = "Auto-synthesized watchdog failure".to_string();
            synthesized.metadata = json!({
                "agent": "watchdog",
                "profile": "claude_plan",
                "outcome": "failure",
                "task_type": "review_request",
                "verification_present": false,
                "auto_synthesized": true
            });
            store.upsert(&synthesized).map_err(|e| e.to_string())
        })
        .expect("seed synthesized eval");

    let aggregate_live = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            action: "aggregate_live".to_string(),
            fixture_path: None,
            limit: Some(50),
        }))
        .await
        .expect("aggregate_live should succeed");
    let aggregate: serde_json::Value =
        serde_json::from_str(&aggregate_live).expect("aggregate_live JSON");
    assert_eq!(aggregate["row_count"], serde_json::json!(1));
    assert!(
        aggregate["performance_matrix"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["agent"] == "codex")),
        "real eval row should remain in matrix: {aggregate:#}"
    );
    assert!(
        !aggregate["performance_matrix"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["agent"] == "watchdog")),
        "auto-synthesized watchdog row should be filtered from matrix: {aggregate:#}"
    );
}
