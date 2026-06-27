use super::*;

#[tokio::test]
async fn tachi_complete_writes_eval_ledger_and_returns_review_bundle() {
    let server = make_server();

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("smoke-test-001".to_string()),
            task: "Refactor auth middleware".to_string(),
            agent: "claude-code".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: Some("claude_plan".to_string()),
            risk: Some("medium".to_string()),
            duration_ms: Some(5420),
            skills_used: vec!["skill:superpowers".to_string()],
            cost_tokens: Some(1234),
            cost_usd: Some(0.0812),
            quality_score: Some(0.9),
            notes: Some("All tests green.".to_string()),
            trajectory: None,
            diff: Some("diff --git a/foo b/foo\n+bar\n".to_string()),
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "architect".to_string(),
                agent: "kimi".to_string(),
                model: Some("kimi-for-coding".to_string()),
                task: Some("Review eval-ledger architecture".to_string()),
                task_type: Some("plan_request".to_string()),
                outcome: Some("useful".to_string()),
                usefulness_score: Some(0.82),
                failure_mode: None,
                verification_impact: Some("changed_plan".to_string()),
                verification_present: true,
                evaluator: Some("leader".to_string()),
                plan_delta: Some("modified".to_string()),
                human_override: false,
                retry_count: 0,
                notes: Some(
                    "Recommended concise structured summaries over raw transcripts.".to_string(),
                ),
                latency_ms: Some(2100),
                input_tokens: Some(1200),
                output_tokens: Some(240),
                cost_tokens: Some(321),
                cost_usd: None,
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-194".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch_profile --lib".to_string()],
            diff_present: None,
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("tachi_complete should succeed");

    let bundle: serde_json::Value = serde_json::from_str(&resp).expect("bundle JSON");
    assert_eq!(bundle["recorded"], serde_json::json!(true));
    assert_eq!(bundle["task_id"], serde_json::json!("smoke-test-001"));
    assert_eq!(bundle["outcome"], serde_json::json!("success"));
    let next_steps = bundle["next_steps"].as_array().expect("next_steps array");
    assert!(
        next_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|s| s.contains("no approve_merge step is implied"))),
        "no-worktree completion should not imply approve_merge: {bundle:#}"
    );
    let path = bundle["path"].as_str().expect("path present");
    assert!(
        path.starts_with("/eval/"),
        "eval entry path should be under /eval, got {path}"
    );
    assert!(
        path.ends_with("smoke-test-001"),
        "path should include task_id, got {path}"
    );

    let eval_entry = &bundle["eval_entry"];
    let id = eval_entry["id"]
        .as_str()
        .expect("eval entry should return memory id")
        .to_string();

    // Confirm the memory entry is actually retrievable with correct metadata.
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: serde_json::Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    assert_eq!(fetched["category"], serde_json::json!("eval"));
    let keywords = fetched["keywords"].as_array().expect("keywords array");
    assert!(keywords.iter().any(|k| k == "eval"));
    let metadata = &fetched["metadata"];
    assert_eq!(metadata["agent"], serde_json::json!("claude-code"));
    assert_eq!(metadata["outcome"], serde_json::json!("success"));
    assert_eq!(metadata["task_type"], serde_json::json!("fix_request"));
    assert_eq!(metadata["profile"], serde_json::json!("claude_plan"));
    assert_eq!(metadata["risk"], serde_json::json!("medium"));
    assert_eq!(metadata["flow_id"], serde_json::json!("flow-194"));
    assert_eq!(
        metadata["issue_ref"],
        serde_json::json!("kckylechen1/tachi#194")
    );
    assert_eq!(metadata["diff_present"], serde_json::json!(true));
    assert_eq!(
        metadata["tests_run"][0],
        serde_json::json!("cargo test -p memory-server dispatch_profile --lib")
    );
    assert_eq!(metadata["cost_tokens"], serde_json::json!(1234));
    assert_eq!(metadata["subagent_eval"], serde_json::json!(true));
    assert_eq!(metadata["subagent_count"], serde_json::json!(1));
    assert_eq!(
        metadata["subagent_roles"][0],
        serde_json::json!("architect")
    );
    assert_eq!(
        metadata["subagent_models"][0],
        serde_json::json!("kimi-for-coding")
    );
    assert_eq!(metadata["subagents"][0]["agent"], serde_json::json!("kimi"));
    assert_eq!(
        metadata["subagents"][0]["task_type"],
        serde_json::json!("plan_request")
    );
    assert_eq!(
        metadata["subagents"][0]["evaluator"],
        serde_json::json!("leader")
    );
    assert_eq!(
        metadata["subagents"][0]["latency_ms"],
        serde_json::json!(2100)
    );
    assert_eq!(
        metadata["skills_used"][0],
        serde_json::json!("skill:superpowers")
    );
    assert!(metadata["diff"].as_str().unwrap().contains("+bar"));

    let default_search = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "Refactor auth middleware".to_string(),
            query_vec: None,
            top_k: 10,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }))
        .await
        .expect("default search should succeed");
    let default_rows: Vec<serde_json::Value> =
        serde_json::from_str(&default_search).expect("default search JSON");
    assert!(
        default_rows.iter().all(|row| row["id"] != id),
        "ordinary search should exclude eval entries by default"
    );

    let eval_search = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "Refactor auth middleware".to_string(),
            query_vec: None,
            top_k: 10,
            path_prefix: Some("/eval".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }))
        .await
        .expect("eval search should succeed");
    let eval_rows: Vec<serde_json::Value> =
        serde_json::from_str(&eval_search).expect("eval search JSON");
    assert!(
        eval_rows.iter().any(|row| row["id"] == id),
        "explicit /eval search should include eval entries"
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
    assert_eq!(aggregate["source"], serde_json::json!("live_memory"));
    assert_eq!(aggregate["row_count"], serde_json::json!(1));
    assert_eq!(
        aggregate["subagent_scores"][0]["agent"],
        serde_json::json!("kimi")
    );
    assert_eq!(
        aggregate["subagent_scores"][0]["task_type"],
        serde_json::json!("plan_request")
    );
    assert!(
        aggregate["performance_matrix"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["scope"] == "leader"
                && row["agent"] == "claude-code"
                && row["profile"] == "claude_plan"
                && row["avg_latency_ms"] == 5420.0
                && row["avg_cost_tokens"] == 1234.0
                && row["avg_cost_usd"] == 0.0812
                && row["avg_quality_score"] == 0.9)),
        "aggregate_live should include leader performance telemetry: {aggregate:#}"
    );
    assert!(
        aggregate["performance_matrix"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["scope"] == "subagent"
                && row["agent"] == "kimi"
                && row["role"] == "architect"
                && row["useful_rate"] == 1.0
                && row["avg_input_tokens"] == 1200.0
                && row["avg_cost_tokens"] == 321.0)),
        "aggregate_live should include subagent performance telemetry: {aggregate:#}"
    );

    let telemetry = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            action: "telemetry".to_string(),
            fixture_path: None,
            limit: Some(50),
        }))
        .await
        .expect("telemetry alias should succeed");
    let telemetry: serde_json::Value = serde_json::from_str(&telemetry).expect("telemetry JSON");
    assert_eq!(telemetry["source"], serde_json::json!("live_memory"));
    assert!(telemetry["performance_matrix"].is_array());

    let task_events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memory_core::TachiEventQuery {
                    event_type: Some("task.outcome".to_string()),
                    session_id: Some("smoke-test-001".to_string()),
                    limit: 10,
                    ..memory_core::TachiEventQuery::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("list task outcome events");
    assert_eq!(task_events.len(), 1);
    assert_eq!(
        task_events[0].payload["outcome"],
        serde_json::json!("success")
    );
    assert_eq!(
        task_events[0].projection_hints,
        vec![
            memory_core::ProjectionKind::Outcome,
            memory_core::ProjectionKind::ProjectCycle
        ]
    );

    let subagent_events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memory_core::TachiEventQuery {
                    event_type: Some("subagent.evaluated".to_string()),
                    session_id: Some("smoke-test-001".to_string()),
                    limit: 10,
                    ..memory_core::TachiEventQuery::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("list subagent eval events");
    assert_eq!(subagent_events.len(), 1);
    assert_eq!(
        subagent_events[0].payload["subagent"]["agent"],
        serde_json::json!("kimi")
    );
    assert_eq!(
        subagent_events[0].payload["subagent"]["verification_impact"],
        serde_json::json!("changed_plan")
    );
}
