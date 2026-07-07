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
            signatures: Vec::new(),
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

#[tokio::test]
async fn aggregate_live_uses_harness_native_mirror_eval_without_owning_lifecycle() {
    let server = make_server();

    let completed = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("mirror-native-subagent-001".to_string()),
            task: "Review harness-native subagent evidence".to_string(),
            agent: "codex-controller".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("medium".to_string()),
            duration_ms: Some(1800),
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.88),
            notes: Some(
                "Recorded native worker evidence without owning its lifecycle.".to_string(),
            ),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![
                TachiSubagentEvalParams {
                    role: "verifier".to_string(),
                    agent: "codex".to_string(),
                    model: Some("gpt-5.5".to_string()),
                    task: Some("Cold-review daemon split evidence".to_string()),
                    task_type: Some("review_request".to_string()),
                    outcome: Some("useful".to_string()),
                    usefulness_score: Some(0.9),
                    failure_mode: None,
                    verification_impact: Some("changed_plan".to_string()),
                    verification_present: true,
                    evaluator: Some("leader".to_string()),
                    plan_delta: Some("modified".to_string()),
                    human_override: false,
                    retry_count: 0,
                    notes: None,
                    latency_ms: Some(2200),
                    input_tokens: Some(1400),
                    output_tokens: Some(320),
                    cost_tokens: Some(1720),
                    cost_usd: None,
                    execution_origin: Some("harness_native".to_string()),
                    lifecycle_owner: Some("codex".to_string()),
                    harness: Some("codex".to_string()),
                    native_agent_id: Some("native-agent-123".to_string()),
                    tachi_dispatch_id: None,
                    result_collected: Some(true),
                    evidence_usable: Some(true),
                    used_in_final_claim: Some(false),
                    next_prompt_delta: Some(
                        "Ask verifier for line-cited contradiction attempts.".to_string(),
                    ),
                },
                TachiSubagentEvalParams {
                    role: "critic".to_string(),
                    agent: "tachi".to_string(),
                    model: None,
                    task: Some("Metadata-only dispatch bookkeeping".to_string()),
                    task_type: Some("review_request".to_string()),
                    outcome: Some("failed".to_string()),
                    usefulness_score: Some(0.0),
                    failure_mode: Some("metadata_only".to_string()),
                    verification_impact: Some("none".to_string()),
                    verification_present: false,
                    evaluator: Some("leader".to_string()),
                    plan_delta: Some("rejected".to_string()),
                    human_override: false,
                    retry_count: 0,
                    notes: None,
                    latency_ms: None,
                    input_tokens: None,
                    output_tokens: None,
                    cost_tokens: None,
                    cost_usd: None,
                    execution_origin: Some("tachi_dispatch".to_string()),
                    lifecycle_owner: Some("tachi".to_string()),
                    harness: None,
                    native_agent_id: None,
                    tachi_dispatch_id: Some("dispatch-metadata-only".to_string()),
                    result_collected: Some(false),
                    evidence_usable: Some(false),
                    used_in_final_claim: Some(false),
                    next_prompt_delta: Some(
                        "Do not count run metadata as worker evidence.".to_string(),
                    ),
                },
            ],
            feedback_rules_applied: Vec::new(),
            evidence_refs: vec!["native-agent-123 result.md".to_string()],
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            flow_id: None,
            dispatch_id: None,
            issue_ref: Some("kckylechen1/tachi#768".to_string()),
            pr_ref: None,
            project: None,
            format: None,
            signatures: Vec::new(),
        }))
        .await
        .expect("mirror eval should save without dispatch_id");
    let completed: serde_json::Value = serde_json::from_str(&completed).expect("completion JSON");
    assert_eq!(
        completed["pipeline"]["kanban_update"],
        serde_json::json!("skipped (no dispatch_id)"),
        "mirror eval must not imply Tachi can manage the native worker lifecycle"
    );

    let memory_id = completed["eval_entry"]["id"].as_str().expect("memory id");
    let entry = server
        .with_global_store_read(|store| {
            store
                .get(memory_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "missing eval entry".to_string())
        })
        .expect("eval entry");
    assert_eq!(
        entry.metadata["subagents"][0]["execution_origin"],
        serde_json::json!("harness_native")
    );
    assert_eq!(
        entry.metadata["subagents"][0]["lifecycle_owner"],
        serde_json::json!("codex")
    );
    assert_eq!(
        entry.metadata["subagents"][0]["native_agent_id"],
        serde_json::json!("native-agent-123")
    );
    assert_eq!(
        entry.metadata["subagents"][1]["evidence_usable"],
        serde_json::json!(false)
    );

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
    assert!(
        aggregate["subagent_scores"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["agent"] == "codex"
                && row["role"] == "verifier"
                && row["task_type"] == "review_request"
                && row["useful_rate"] == 1.0)),
        "usable harness-native mirror eval should feed subagent scores: {aggregate:#}"
    );
    assert!(
        !aggregate["subagent_scores"]
            .as_array()
            .is_some_and(|rows| rows
                .iter()
                .any(|row| row["agent"] == "tachi" && row["role"] == "critic")),
        "metadata-only dispatch bookkeeping must not feed subagent scores: {aggregate:#}"
    );
    assert!(
        aggregate["performance_matrix"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["scope"] == "subagent"
                && row["agent"] == "codex"
                && row["role"] == "verifier"
                && row["avg_input_tokens"] == 1400.0)),
        "usable harness-native mirror eval should feed the performance matrix: {aggregate:#}"
    );
}
