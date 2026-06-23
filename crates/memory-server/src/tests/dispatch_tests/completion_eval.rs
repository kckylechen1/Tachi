use super::super::{make_entry, make_server, make_server_with_temp_home};
use super::{save_grep_evidence_feedback_rule, task_params};
use crate::tool_params::{
    GetMemoryParams, SearchMemoryParams, TachiAgentEvalParams, TachiCompleteParams,
    TachiSubagentEvalParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
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
}

#[tokio::test]
async fn tachi_complete_failure_with_notes_saves_lesson_hook() {
    let server = make_server();

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("lesson-hook-001".to_string()),
            task: "Fix a brittle integration path".to_string(),
            agent: "codex".to_string(),
            outcome: "failure".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: Some("medium".to_string()),
            duration_ms: Some(900),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: Some("The attempted fix lacked a regression test.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("failure complete should still record eval");

    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert_eq!(
        bundle["pipeline"]["post_complete_hooks"],
        json!("lesson_saved")
    );

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("lesson-hook-002".to_string()),
            task: "Fix".to_string(),
            agent: "codex".to_string(),
            outcome: "failure".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: Some("medium".to_string()),
            duration_ms: Some(900),
            skills_used: vec!["skill:other".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: Some("A short task name should not dedup against a longer task.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("distinct short task should still record a lesson");

    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert_eq!(
        bundle["pipeline"]["post_complete_hooks"],
        json!("lesson_saved")
    );
}

#[tokio::test]
async fn tachi_complete_scrubs_secretish_eval_metadata() {
    let server = make_server();
    let secret = "eval-redaction-fixture-token";

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("eval-secret-redaction".to_string()),
            task: "Verify eval secret hygiene".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(100),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.8),
            notes: Some(format!("Do not persist api_key={secret}")),
            trajectory: Some(json!([
                {
                    "step": "run",
                    "env": {
                        "OPENAI_API_KEY": secret
                    }
                }
            ])),
            diff: Some(format!("+OPENAI_API_KEY={secret}\n")),
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "reviewer".to_string(),
                agent: "kimi".to_string(),
                model: Some("kimi-for-coding".to_string()),
                task: Some("Review secret hygiene".to_string()),
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
                notes: Some(format!("Saw token={secret} in draft evidence")),
                latency_ms: Some(10),
                input_tokens: None,
                output_tokens: None,
                cost_tokens: None,
                cost_usd: None,
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec![format!("evidence token={secret}")],
            tests_run: vec![format!("cargo test # token={secret}")],
            diff_present: None,
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("tachi_complete should succeed");

    assert!(!resp.contains(secret), "response leaked secret: {resp}");
    assert!(
        resp.contains("[REDACTED]"),
        "response should show redaction"
    );
    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert!(
        bundle["secret_redactions"].as_u64().unwrap_or(0) > 0,
        "redaction count should be reported: {bundle:#}"
    );
    let memory_id = bundle["eval_entry"]["id"].as_str().expect("memory id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: memory_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("eval memory should be readable");
    assert!(
        !fetched.contains(secret),
        "eval memory leaked secret: {fetched}"
    );
    assert!(
        fetched.contains("[REDACTED]"),
        "eval memory should persist redacted evidence"
    );
}

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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_links_eval_to_flow_dispatch_card_and_ux_matrix() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260609T000002Z_complete_link_test";
    let dispatch_id = "20260609T000002Z-custom-complete-link";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_51_impl",
            "task": "implementation",
        }),
    )
    .expect("mark dispatch");

    let mut complete_params = task_params("complete");
    complete_params.task = Some("Implement dispatch completion linkage".to_string());
    complete_params.agent = Some("glm".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-link-002".to_string());
    complete_params.task_type = Some("fix_request".to_string());
    complete_params.profile = Some("glm_51_impl".to_string());
    complete_params.risk = Some("medium".to_string());
    complete_params.duration_ms = Some(1200);
    complete_params.skills_used = vec!["skill:superpowers-executing-plans".to_string()];
    complete_params.cost_tokens = Some(123);
    complete_params.quality_score = Some(0.88);
    complete_params.notes = Some("Linked eval back to dispatch card.".to_string());
    complete_params.diff = Some("diff --git a/x b/x\n+y\n".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.flow_id = Some(flow_id.to_string());
    complete_params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    complete_params.evidence_refs = vec!["crates/memory-server/src/complete_ops.rs".to_string()];
    complete_params.tests_run = vec!["cargo test -p memory-server dispatch_tests".to_string()];
    complete_params.scope = Some("project".to_string());
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should succeed");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    assert_eq!(
        bundle["pipeline"]["dispatch_completion_link"]["recorded"],
        json!(true),
        "{bundle:#}"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["completion"]["task_id"], json!("eval-link-002"));
    assert_eq!(card["completion"]["outcome"], json!("success"));
    assert_eq!(
        card["completion"]["verification_present"],
        json!(true),
        "{card:#}"
    );

    let mut ux_params = task_params("ux_matrix");
    ux_params.flow_id = Some(flow_id.to_string());
    ux_params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    let ux_raw = server
        .tachi_task(Parameters(ux_params))
        .await
        .expect("ux_matrix should succeed");
    let ux: Value = serde_json::from_str(&ux_raw).expect("ux JSON");
    assert!(
        ux["matrix"].as_array().is_some_and(|steps| {
            steps.iter().any(|step| {
                step["id"] == json!("complete_eval")
                    && step["status"] == json!("passed")
                    && step["tool"]
                        == json!("tachi_task(action='complete', dispatch_id=..., flow_id=...)")
            })
        }),
        "{ux:#}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_infers_task_agent_and_profile_from_dispatch_card() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260609T000004Z_complete_defaults_test";
    let dispatch_id = "20260609T000004Z-custom-defaults";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_51_impl",
            "task": "implementation from card",
        }),
    )
    .expect("mark dispatch");

    let mut complete_params = task_params("complete");
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-link-004".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.flow_id = Some(flow_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should infer dispatch defaults");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    assert_eq!(bundle["recorded"], json!(true), "{bundle:#}");
    assert_eq!(bundle["agent"], json!("custom"), "{bundle:#}");
    assert_eq!(
        bundle["task"],
        json!("implementation from card"),
        "{bundle:#}"
    );
    assert_eq!(bundle["dispatch_id"], json!(dispatch_id), "{bundle:#}");
    assert_eq!(bundle["profile"], json!("glm_51_impl"), "{bundle:#}");
    assert_eq!(
        bundle["pipeline"]["dispatch_completion_link"]["recorded"],
        json!(true),
        "{bundle:#}"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["agent"],
        json!("custom"),
        "{status:#}"
    );
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["task"],
        json!("implementation from card"),
        "{status:#}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_surfaces_warning_when_kanban_card_is_missing() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260615T000008Z-kanban-warning";

    let mut complete_params = task_params("complete");
    complete_params.task = Some("Write completion while kanban is stale".to_string());
    complete_params.agent = Some("codex".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-kanban-warning".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should still succeed");

    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    let warning = bundle["warning"]
        .as_str()
        .expect("kanban update warning should be surfaced");
    assert!(warning.contains("kanban card missing"), "{warning}");
    assert!(warning.contains(dispatch_id), "{warning}");
    assert!(warning.contains("eval-kanban-warning"), "{warning}");
    assert!(
        warning.contains("Write completion while kanban is stale"),
        "{warning}"
    );
    assert_eq!(
        bundle["pipeline"]["kanban_update"]["status"],
        json!("missing")
    );
    assert_eq!(
        bundle["pipeline"]["kanban_update"]["dispatch_id"],
        json!(dispatch_id)
    );
}

#[tokio::test]
async fn tachi_complete_records_applied_feedback_rules_for_eval_aggregation() {
    let server = make_server();
    let rule_id = save_grep_evidence_feedback_rule(&server).await;
    let raw = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("feedback-rule-complete".to_string()),
            task: "Review unused code with grep evidence".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("code_audit".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("medium".to_string()),
            duration_ms: Some(42),
            skills_used: vec!["skill:waza-check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: Some("Applied grep evidence feedback rule.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: vec![rule_id.clone()],
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec!["grep output".to_string()],
            tests_run: vec!["cargo test feedback rules".to_string()],
            diff_present: Some(false),
            scope: None,
            project: None,
        }))
        .await
        .expect("completion should succeed");
    let completed: Value = serde_json::from_str(&raw).expect("complete JSON");
    let memory_id = completed["eval_entry"]["id"].as_str().expect("memory id");
    let fetched = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: memory_id.to_string(),
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("eval memory should be readable");
    let eval: Value = serde_json::from_str(&fetched).expect("eval JSON");
    assert_eq!(eval["metadata"]["feedback_rules_applied"], json!([rule_id]));
}
