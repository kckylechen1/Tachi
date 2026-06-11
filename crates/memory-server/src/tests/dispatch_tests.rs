use super::*;

fn dispatch_params(agent: Option<&str>, task: &str) -> TachiDispatchParams {
    TachiDispatchParams {
        agent: agent.map(str::to_string),
        profile: None,
        task: task.to_string(),
        cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
        permission_profile: None,
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command: Vec::new(),
        harness_transport: None,
        harness_server_url: None,
        project: None,
        stage: None,
        credential_profiles: Vec::new(),
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
    }
}

fn task_params(action: &str) -> TachiTaskParams {
    TachiTaskParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        task: None,
        agent_id: None,
        domain: None,
        path_prefix: None,
        top_k: None,
        doc_paths: Vec::new(),
        related_issues: Vec::new(),
        spec_paths: Vec::new(),
        include_global: false,
        compact: None,
        agent: None,
        outcome: None,
        task_id: None,
        task_type: None,
        duration_ms: None,
        skills_used: Vec::new(),
        cost_tokens: None,
        cost_usd: None,
        quality_score: None,
        notes: None,
        trajectory: None,
        diff: None,
        subagents: Vec::new(),
        feedback_rules_applied: Vec::new(),
        evidence_refs: Vec::new(),
        tests_run: Vec::new(),
        diff_present: None,
        scope: None,
        cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: None,
        permission_profile: None,
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command: Vec::new(),
        harness_transport: None,
        harness_server_url: None,
        project: None,
        stage: None,
        profile: None,
        credential_profiles: Vec::new(),
        repo: None,
        number: None,
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        dispatch_id: None,
        risk: None,
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        state_filter: None,
        limit: None,
        proposal_id: None,
        review_status: None,
        worktree: None,
        branch: None,
        strategy: None,
        merge_policy: None,
        delete_worktree: true,
        confirm: false,
        wiki_title: None,
        wiki_text: None,
        wiki_path: None,
        wiki_topic: None,
        wiki_summary: None,
        wiki_category: None,
        wiki_keywords: Vec::new(),
        wiki_entities: Vec::new(),
        wiki_importance: None,
        wiki_scope: None,
        wiki_domain: None,
        force: false,
    }
}

fn memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        query: None,
        scope: None,
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
        synthesize: false,
        model: None,
        text: None,
        title: None,
        summary: None,
        topic: None,
        keywords: Vec::new(),
        entities: Vec::new(),
        importance: None,
        retention_policy: None,
        kind: None,
        path: None,
        id: None,
        force: false,
        source: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        files: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: None,
        compact: false,
    }
}

async fn save_grep_evidence_feedback_rule(server: &MemoryServer) -> String {
    let mut params = memory_params("save");
    params.kind = Some("feedback_rule".to_string());
    params.title = Some("Subagent audit prompts require explicit search evidence".to_string());
    params.topic = Some("Subagent audit prompts require explicit search evidence".to_string());
    params.path = Some("/feedback/subagent/code-audit/grep-evidence".to_string());
    params.category = Some("prompt_rule".to_string());
    params.text = Some(
        "When dispatching subagents for code audits or dead-code scans, give exact search patterns and require grep/ripgrep evidence in reports."
            .to_string(),
    );
    params.keywords = vec![
        "subagent".to_string(),
        "code_audit".to_string(),
        "dead_code".to_string(),
        "grep".to_string(),
        "false_positive".to_string(),
    ];
    params.force = true;
    params.metadata = Some(json!({
        "applies_to": {
            "task_type": ["review_request", "code_audit", "dead_code_scan"],
            "profiles": ["codex_55_review", "codex_53_fast", "deepseek_explore"],
            "stage": ["review", "explore"]
        },
        "trigger_keywords": ["unused", "no callers", "dead code", "grep", "search repo"],
        "prompt_patch": "For any 'unused', 'no callers', or dead-code claim: search both identifier form and call form; include exact grep/ripgrep commands; list paths searched; report uncertainty if the search scope is incomplete.",
        "evidence_contract": [
            "grep_commands",
            "paths_searched",
            "matching_files_or_none",
            "confidence",
            "uncertainty_notes"
        ]
    }));

    let raw = crate::facade_memory_ops::handle_tachi_memory(server, params)
        .await
        .expect("feedback rule save should succeed");
    serde_json::from_str::<Value>(&raw)
        .expect("save JSON")
        .get("id")
        .and_then(Value::as_str)
        .expect("saved rule id")
        .to_string()
}

#[tokio::test]
async fn tachi_task_facade_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = make_server();

    let mut json_params = task_params("profiles");
    json_params.format = None;
    let json_body = server
        .tachi_task(Parameters(json_params))
        .await
        .expect("default profiles should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default profiles JSON");
    assert!(parsed["dispatch_profiles"].as_array().is_some());

    let mut markdown_params = task_params("profiles");
    markdown_params.format = Some("markdown".to_string());
    let markdown = server
        .tachi_task(Parameters(markdown_params))
        .await
        .expect("markdown profiles should succeed");
    assert!(markdown.starts_with("## Tachi task profiles"), "{markdown}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_wait_returns_terminal_dispatch_status() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let dispatch_id = "dispatch-wait-complete";
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "wait for completed dispatch",
            "state": "TASK_STATE_COMPLETED",
            "exit_code": 0,
            "updated_at": Utc::now().to_rfc3339(),
        }))
        .expect("status json"),
    )
    .expect("write status");
    std::fs::write(run_dir.join("result.md"), "done").expect("result");

    let mut params = task_params("wait");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(0);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("wait should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("wait JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["terminal"], json!(true));
    assert_eq!(parsed["state"], json!("TASK_STATE_COMPLETED"));
    assert_eq!(parsed["task"]["result_written"], json!(true));
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.original.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

async fn wait_for_dispatch_result(run_dir: &std::path::Path) -> String {
    let result_path = run_dir.join("result.md");
    for _ in 0..40 {
        if let Ok(raw) = std::fs::read_to_string(&result_path) {
            return raw;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    std::fs::read_to_string(&result_path).expect("dispatch result.md should be written")
}

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

#[tokio::test]
async fn tachi_task_proposals_include_reviewable_loadout_evolution_candidates() {
    let server = make_server();

    for idx in 0..10 {
        let agent = if idx < 5 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-proposal-plan-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: agent.to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec![
                    "skill:superpowers-writing-plans".to_string(),
                    "skill:planning-ux-review".to_string(),
                ],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed loadout evolution proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-evolution-proposal".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed loadout eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    assert!(proposals["proposal_kinds"]
        .as_array()
        .expect("proposal kinds")
        .contains(&json!("loadout_evolution")));
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["skill_id"] == json!("skill:superpowers-writing-plans")
        }),
        "existing profile skills should not generate loadout evolution proposals: {proposal_items:?}"
    );
    let proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["skill_id"] == json!("skill:planning-ux-review")
            })
        })
        .expect("loadout evolution proposal");
    assert_eq!(proposal["status"], json!("pending"));
    assert_eq!(proposal["requires_human_approval"], json!(true));
    assert_eq!(
        proposal["operation"],
        json!("promote_observed_skill_to_signature")
    );
    assert_eq!(proposal["evidence"]["profile_samples"], json!(10));
    assert_eq!(proposal["evidence"]["skill_hits"], json!(10));
    assert_eq!(
        proposal["proposed_patch"]["add_signature_skills"][0],
        json!("skill:planning-ux-review")
    );
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let passive_proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_backed_passive_trait")
                    && proposal["trait_id"] == json!("evidence_backed_planning")
            })
        })
        .expect("passive trait evolution proposal");
    assert_eq!(
        proposal_items
            .iter()
            .filter(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_backed_passive_trait")
                    && proposal["trait_id"] == json!("evidence_backed_planning")
            })
            .count(),
        1,
        "duplicate agent/model matrix rows should not emit duplicate passive trait proposals: {proposal_items:?}"
    );
    assert_eq!(
        passive_proposal["proposed_patch"]["add_passive_traits"][0],
        json!("evidence_backed_planning")
    );
    let passive_proposal_id = passive_proposal["proposal_id"]
        .as_str()
        .expect("passive proposal id")
        .to_string();
    let evidence_proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_contract_required")
                    && proposal["evidence_id"] == json!("acceptance_criteria")
            })
        })
        .expect("evidence contract evolution proposal");
    assert_eq!(
        proposal_items
            .iter()
            .filter(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_contract_required")
                    && proposal["evidence_id"] == json!("acceptance_criteria")
            })
            .count(),
        1,
        "duplicate agent/model matrix rows should not emit duplicate evidence contract proposals: {proposal_items:?}"
    );
    assert_eq!(
        evidence_proposal["proposed_patch"]["add_evidence_required"][0],
        json!("acceptance_criteria")
    );
    let evidence_proposal_id = evidence_proposal["proposal_id"]
        .as_str()
        .expect("evidence proposal id")
        .to_string();

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("Human approved loadout evolution candidate.".to_string());
    let reviewed_raw = server
        .tachi_task(Parameters(review))
        .await
        .expect("review should succeed");
    let reviewed: serde_json::Value = serde_json::from_str(&reviewed_raw).expect("review JSON");
    assert_eq!(reviewed["proposal"]["status"], json!("approved"));
    assert_eq!(reviewed["proposal"]["kind"], json!("loadout_evolution"));

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply))
        .await
        .expect("approved loadout evolution should project");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply JSON");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["profile_card_mutated"], json!(true));
    assert_eq!(
        applied["projection_namespace"],
        json!("dispatch_profile_card_overlays")
    );
    assert_eq!(applied["proposal"]["status"], json!("applied"));
    assert_eq!(
        applied["proposal"]["projection"]["status"],
        json!("applied_profile_card_overlay")
    );

    let mut second_apply = task_params("apply_proposals");
    second_apply.proposal_id = Some(proposal_id.clone());
    second_apply.confirm = true;
    let err = server
        .tachi_task(Parameters(second_apply))
        .await
        .expect_err("applied proposal should require a fresh approved proposal");
    assert!(err.contains("must be approved before apply"), "{err}");

    let mut passive_review = task_params("review_proposal");
    passive_review.proposal_id = Some(passive_proposal_id.clone());
    passive_review.review_status = Some("approved".to_string());
    passive_review.notes = Some("Human approved passive trait projection.".to_string());
    let passive_reviewed_raw = server
        .tachi_task(Parameters(passive_review))
        .await
        .expect("passive review should succeed");
    let passive_reviewed: serde_json::Value =
        serde_json::from_str(&passive_reviewed_raw).expect("passive review JSON");
    assert_eq!(
        passive_reviewed["proposal"]["operation"],
        json!("add_evidence_backed_passive_trait")
    );

    let mut passive_apply = task_params("apply_proposals");
    passive_apply.proposal_id = Some(passive_proposal_id);
    passive_apply.confirm = true;
    let passive_applied_raw = server
        .tachi_task(Parameters(passive_apply))
        .await
        .expect("approved passive trait should project");
    let passive_applied: serde_json::Value =
        serde_json::from_str(&passive_applied_raw).expect("passive apply JSON");
    assert_eq!(
        passive_applied["proposal"]["projection"]["added_passive_traits"][0],
        json!("evidence_backed_planning")
    );

    let mut evidence_review = task_params("review_proposal");
    evidence_review.proposal_id = Some(evidence_proposal_id.clone());
    evidence_review.review_status = Some("approved".to_string());
    evidence_review.notes = Some("Human approved evidence contract projection.".to_string());
    let evidence_reviewed_raw = server
        .tachi_task(Parameters(evidence_review))
        .await
        .expect("evidence review should succeed");
    let evidence_reviewed: serde_json::Value =
        serde_json::from_str(&evidence_reviewed_raw).expect("evidence review JSON");
    assert_eq!(
        evidence_reviewed["proposal"]["operation"],
        json!("add_evidence_contract_required")
    );

    let mut evidence_apply = task_params("apply_proposals");
    evidence_apply.proposal_id = Some(evidence_proposal_id);
    evidence_apply.confirm = true;
    let evidence_applied_raw = server
        .tachi_task(Parameters(evidence_apply))
        .await
        .expect("approved evidence contract should project");
    let evidence_applied: serde_json::Value =
        serde_json::from_str(&evidence_applied_raw).expect("evidence apply JSON");
    assert_eq!(
        evidence_applied["proposal"]["projection"]["added_evidence_required"][0],
        json!("acceptance_criteria")
    );

    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("plan a dispatch loadout evolution slice".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
            profile: Some("claude_plan".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(2),
            pack_limit: Some(1),
            include_section: Some(false),
        }))
        .await
        .expect("loadout should include projected skill");
    let loadout: serde_json::Value = serde_json::from_str(&loadout_raw).expect("loadout JSON");
    assert!(loadout["resolved_skills"]
        .as_array()
        .expect("resolved skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(loadout["skill_loadout"]["projected_signature_skills"]
        .as_array()
        .expect("projected signature skills")
        .contains(&json!("skill:planning-ux-review")));
    assert_eq!(
        loadout["skill_loadout"]["projection"]["status"],
        json!("applied_overlay")
    );
    assert!(loadout["skill_loadout"]["passive_traits"]
        .as_array()
        .expect("passive traits")
        .contains(&json!("evidence_backed_planning")));
    assert!(loadout["skill_loadout"]["projected_passive_traits"]
        .as_array()
        .expect("projected passive traits")
        .contains(&json!("evidence_backed_planning")));
    assert!(
        loadout["mbit_card"]["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("mbit projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(loadout["evidence_required"]
        .as_array()
        .expect("loadout evidence required")
        .contains(&json!("acceptance_criteria")));
    assert!(loadout["evidence_contract"]["projected_required"]
        .as_array()
        .expect("loadout projected evidence")
        .contains(&json!("acceptance_criteria")));
    assert!(
        loadout["mbit_card"]["evidence_contract"]["projected_required"]
            .as_array()
            .expect("mbit projected evidence")
            .contains(&json!("acceptance_criteria"))
    );

    let profiles_raw = server
        .tachi_task(Parameters(task_params("profiles")))
        .await
        .expect("profiles should include projected loadout");
    let profiles: serde_json::Value = serde_json::from_str(&profiles_raw).expect("profiles JSON");
    let claude_profile = profiles["dispatch_profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan profile");
    assert!(claude_profile["skill_loadout"]["signature_skills"]
        .as_array()
        .expect("signature skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(
        claude_profile["mbit_card"]["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("profile mbit projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(claude_profile["evidence_contract"]["projected_required"]
        .as_array()
        .expect("profile projected evidence")
        .contains(&json!("acceptance_criteria")));
    assert!(
        claude_profile["mbit_card"]["evidence_contract"]["projected_required"]
            .as_array()
            .expect("profile mbit projected evidence")
            .contains(&json!("acceptance_criteria"))
    );

    let mut recommend_params = task_params("recommend");
    recommend_params.task = Some("Plan a dispatch loadout evolution slice".to_string());
    recommend_params.limit = Some(50);
    let recommend_raw = server
        .tachi_task(Parameters(recommend_params))
        .await
        .expect("recommend should include projected loadout");
    let recommend: serde_json::Value =
        serde_json::from_str(&recommend_raw).expect("recommend JSON");
    assert!(recommend["resolved_skills"]
        .as_array()
        .expect("recommend resolved skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(
        recommend["resolved_skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("recommend projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(
        recommend["mbit_card"]["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("recommend mbit projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(recommend["evidence_required"]
        .as_array()
        .expect("recommend evidence required")
        .contains(&json!("acceptance_criteria")));
    assert!(recommend["evidence_contract"]["projected_required"]
        .as_array()
        .expect("recommend projected evidence")
        .contains(&json!("acceptance_criteria")));
    assert!(
        recommend["mbit_card"]["evidence_contract"]["projected_required"]
            .as_array()
            .expect("recommend mbit projected evidence")
            .contains(&json!("acceptance_criteria"))
    );

    let agents_raw = server
        .tachi_agents(Parameters(TachiAgentsParams {
            action: "profiles".to_string(),
            intent: None,
            task: None,
        }))
        .await
        .expect("legacy agents registry should include projected loadout");
    let agents: serde_json::Value = serde_json::from_str(&agents_raw).expect("agents JSON");
    let agent_claude_profile = agents["dispatch_profiles"]
        .as_array()
        .expect("agent profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan in agent registry");
    assert!(agent_claude_profile["skill_loadout"]["signature_skills"]
        .as_array()
        .expect("agent signature skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(
        agent_claude_profile["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("agent projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(
        agent_claude_profile["evidence_contract"]["projected_required"]
            .as_array()
            .expect("agent projected evidence")
            .contains(&json!("acceptance_criteria"))
    );
}

#[tokio::test]
async fn tachi_task_proposals_requires_loadout_evolution_sample_threshold() {
    let server = make_server();

    for idx in 0..9 {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-proposal-below-threshold-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: "claude".to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec![
                    "skill:superpowers-writing-plans".to_string(),
                    "skill:planning-ux-review".to_string(),
                ],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed below-threshold loadout proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-evolution-threshold".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed below-threshold loadout eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["skill_id"] == json!("skill:planning-ux-review")
        }),
        "loadout evolution proposal should require at least 10 profile samples: {proposal_items:?}"
    );
}

#[tokio::test]
async fn tachi_task_proposals_project_card_weakness_and_demotion_targets() {
    let server = make_server();

    for idx in 0..3 {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("card-risk-plan-failure-{idx}")),
                task: "Plan a dispatch card risk slice".to_string(),
                agent: "claude".to_string(),
                outcome: "failure".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:superpowers-writing-plans".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.20),
                notes: Some("Seed card weakness and demotion proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-card-risk-evolution".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: Vec::new(),
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed card risk eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    let weakness_proposal = proposal_items
        .iter()
        .find(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["operation"] == json!("add_card_weakness")
                && proposal["weakness_id"] == json!("plan_request")
        })
        .expect("card weakness proposal");
    let demotion_proposal = proposal_items
        .iter()
        .find(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["operation"] == json!("mark_skill_demotion_target")
                && proposal["skill_id"] == json!("skill:superpowers-writing-plans")
        })
        .expect("skill demotion proposal");
    assert_eq!(
        weakness_proposal["proposed_patch"]["add_weak_against"][0],
        json!("plan_request")
    );
    assert_eq!(
        demotion_proposal["proposed_patch"]["demotion_targets"][0],
        json!("skill:superpowers-writing-plans")
    );

    for proposal in [weakness_proposal, demotion_proposal] {
        let proposal_id = proposal["proposal_id"].as_str().expect("proposal id");
        let mut review = task_params("review_proposal");
        review.proposal_id = Some(proposal_id.to_string());
        review.review_status = Some("approved".to_string());
        review.notes = Some("Human approved card risk projection.".to_string());
        server
            .tachi_task(Parameters(review))
            .await
            .expect("review should succeed");

        let mut apply = task_params("apply_proposals");
        apply.proposal_id = Some(proposal_id.to_string());
        apply.confirm = true;
        server
            .tachi_task(Parameters(apply))
            .await
            .expect("card risk projection should apply");
    }

    let profiles_raw = server
        .tachi_task(Parameters(task_params("profiles")))
        .await
        .expect("profiles should include card risk projections");
    let profiles: serde_json::Value = serde_json::from_str(&profiles_raw).expect("profiles JSON");
    let claude_profile = profiles["dispatch_profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan profile");
    assert!(
        claude_profile["mbit_card"]["stats"]["risk_control"]
            .as_i64()
            .expect("risk_control stat")
            > 0
    );
    assert!(claude_profile["weak_against"]
        .as_array()
        .expect("merged weak_against")
        .contains(&json!("plan_request")));
    assert!(claude_profile["mbit_card"]["projected_weak_against"]
        .as_array()
        .expect("projected weak_against")
        .contains(&json!("plan_request")));
    assert!(claude_profile["mbit_card"]["demotion_targets"]
        .as_array()
        .expect("demotion targets")
        .contains(&json!("skill:superpowers-writing-plans")));

    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("Plan a dispatch card risk slice".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
            profile: Some("claude_plan".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(2),
            pack_limit: Some(1),
            include_section: Some(false),
        }))
        .await
        .expect("loadout should include projected weakness");
    let loadout: serde_json::Value = serde_json::from_str(&loadout_raw).expect("loadout JSON");
    assert_eq!(
        loadout["weak_against"], loadout["mbit_card"]["weak_against"],
        "top-level loadout weak_against should match the MBIT card"
    );
    assert!(loadout["weak_against"]
        .as_array()
        .expect("loadout weak_against")
        .contains(&json!("plan_request")));

    let mut recommend_params = task_params("recommend");
    recommend_params.task = Some("Plan a dispatch card risk slice".to_string());
    recommend_params.limit = Some(50);
    let recommend_raw = server
        .tachi_task(Parameters(recommend_params))
        .await
        .expect("recommend should include weak_against penalty");
    let recommend: serde_json::Value =
        serde_json::from_str(&recommend_raw).expect("recommend JSON");
    let claude_candidate = recommend["candidates"]
        .as_array()
        .expect("candidate list")
        .iter()
        .find(|candidate| candidate["profile"] == json!("claude_plan"))
        .expect("claude_plan candidate");
    assert!(claude_candidate["reasons"]
        .as_array()
        .expect("candidate reasons")
        .contains(&json!("weak_against_signal:plan_request")));
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

#[test]
fn dispatch_run_cleanup_only_for_completed_success() {
    assert!(crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_COMPLETED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_FAILED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_INPUT_REQUIRED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(1),
        Some("TASK_STATE_COMPLETED")
    ));
}

#[tokio::test]
async fn tachi_task_brief_uses_wiki_hits_for_debug_checklist() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .upsert(&MemoryEntry {
                    id: "wiki-debug-mcp-args".to_string(),
                    path: "/wiki/debug/mcp-args".to_string(),
                    summary: "Debug MCP argument serialization bug".to_string(),
                    text: "Debug MCP argument serialization bug checklist:\n- Verify schema -> client serialization -> server deserialization before editing transport.\n- Add a failing boundary test at the API boundary before retrying the same layer.\n- Stop after two failed patches in the same layer and ask another agent.\n\nThis note exists specifically for an MCP argument serialization bug that looks tempting to misdiagnose as a transport issue.".to_string(),
                    importance: 0.9,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "experience".to_string(),
                    topic: "mcp_args".to_string(),
                    keywords: vec!["mcp".to_string(), "debugging".to_string()],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: Some("permanent".to_string()),
                    domain: Some("coding".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                })
                .map_err(|e| e.to_string())
        })
        .expect("seed wiki debugging note");

    let response = server
        .tachi_task_brief(Parameters(TaskBriefParams {
            task: "Debug MCP argument serialization bug".to_string(),
            agent_id: Some("copilot".to_string()),
            project: None,
            path_prefix: None,
            domain: Some("coding".to_string()),
            top_k: 3,
        }))
        .await
        .expect("tachi_task_brief should succeed");

    let json: Value = serde_json::from_str(&response).expect("task brief response json");
    assert!(
        json["wiki_hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "expected wiki hits for matching task, got: {json}"
    );
    assert!(json["debug_checklist"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| {
            item.as_str().is_some_and(|text| {
                text.contains("schema -> client serialization -> server deserialization")
            })
        })));
    assert_eq!(
        json["suggested_next_tools"],
        json!([
            "tachi_wiki(action='search')",
            "tachi_skill(action='discover')",
            "tachi_task(action='plan')",
            "tachi_task(action='board')"
        ])
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_returns_feature_scoped_handoff_board() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000000Z_feature_briefing_test";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("valid flow id");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("instruction.md"),
        format!("# Instruction\n\nflow_id: {flow_id}\n"),
    )
    .expect("write instruction");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&json!({
            "flow_id": flow_id,
            "stage": "dispatch",
            "updated_at": Utc::now().to_rfc3339(),
        }))
        .expect("status json"),
    )
    .expect("write status");

    server
        .with_global_store(|store| {
            let mut wiki = make_entry("wiki-feature-briefing");
            wiki.path = "/wiki/agent/tachi/feature-briefing".to_string();
            wiki.summary = "Feature briefing should separate docs wiki and memory".to_string();
            wiki.text =
                "FeatureBriefingNeedle durable wiki lesson for handoff board layering.".to_string();
            wiki.category = "experience".to_string();
            wiki.topic = "feature_briefing".to_string();
            wiki.scope = "global".to_string();
            wiki.retention_policy = Some("permanent".to_string());
            store.upsert(&wiki).map_err(|e| e.to_string())?;

            let mut unrelated = make_entry("global-unrelated-feature-briefing");
            unrelated.path = "/scratch/other/global-memory-dump".to_string();
            unrelated.summary = "FeatureBriefingNeedle unrelated global fragment".to_string();
            unrelated.text = "This global memory would appear in a broad memory dump.".to_string();
            unrelated.scope = "global".to_string();
            store.upsert(&unrelated).map_err(|e| e.to_string())?;

            let mut kanban = make_entry("kanban-feature-briefing");
            kanban.path = "/kanban/tasks/feature-briefing".to_string();
            kanban.summary = format!("{flow_id} worker is running");
            kanban.text = "kanban dispatch task feature briefing".to_string();
            kanban.category = "kanban".to_string();
            kanban.metadata = json!({
                "dispatch_id": "dispatch-feature-briefing",
                "agent": "custom",
                "a2a_state": "TASK_STATE_WORKING",
                "updated_at": Utc::now().to_rfc3339(),
            });
            store.upsert(&kanban).map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .expect("seed briefing fixtures");

    let mut params = task_params("briefing");
    params.format = Some("json".to_string());
    params.task = Some(
        "FeatureBriefingNeedle implement docs/engineering/architecture/subagent-eval-system.md"
            .to_string(),
    );
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];
    params.top_k = Some(5);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("feature briefing should succeed");
    let briefing: Value = serde_json::from_str(&raw).expect("briefing JSON");

    assert_eq!(briefing["kind"], json!("feature_briefing"));
    assert_eq!(briefing["scope"]["flow_id"], json!(flow_id));
    assert!(briefing["canonical_docs"]
        .as_array()
        .is_some_and(|docs| docs.iter().any(|doc| {
            doc["path"] == json!("docs/engineering/architecture/subagent-eval-system.md")
                && doc["exists"] == json!(true)
        })));
    assert!(briefing["run_artifacts"]
        .as_array()
        .is_some_and(|artifacts| artifacts.iter().any(|artifact| {
            artifact["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("instruction.md"))
                && artifact["exists"] == json!(true)
        })));
    assert!(briefing["board_state"]["tasks"]
        .as_array()
        .is_some_and(|tasks| tasks.iter().any(|task| {
            task["dispatch_id"] == json!("dispatch-feature-briefing")
                && task["state"] == json!("TASK_STATE_WORKING")
        })));
    assert!(briefing["wiki_hits"].as_array().is_some_and(|hits| hits
        .iter()
        .any(|hit| { hit["path"] == json!("/wiki/agent/tachi/feature-briefing") })));
    assert!(briefing["route_recommendation"]["recommended_profile"]
        .as_str()
        .is_some_and(|profile| !profile.is_empty()));
    assert!(briefing["relevant_profiles"]
        .as_array()
        .is_some_and(|profiles| {
            profiles.iter().any(|profile| {
                profile["profile"] == briefing["route_recommendation"]["recommended_profile"]
            })
        }));
    assert_eq!(
        briefing["suggested_dispatch"]["tool"],
        json!("tachi_task"),
        "feature briefing should tell leaders which facade to call next: {briefing:#}"
    );
    assert_eq!(
        briefing["suggested_dispatch"]["arguments"]["action"],
        json!("dispatch")
    );
    assert_eq!(
        briefing["suggested_dispatch"]["arguments"]["issue_ref"],
        json!("kckylechen1/tachi#194")
    );
    assert_eq!(
        briefing["suggested_dispatch"]["arguments"]["flow_id"],
        json!(flow_id)
    );
    assert!(briefing["suggested_dispatch"]["arguments"]["profile"]
        .as_str()
        .is_some_and(|profile| !profile.is_empty()));
    assert!(
        briefing["memory_fragments"]
            .as_array()
            .is_some_and(|hits| hits.is_empty()),
        "feature briefing should not include broad global memory fragments by default: {briefing:#}"
    );
    assert!(
        briefing["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("Poll tachi_task(action='board')")),
        "working board task should drive next action: {briefing:#}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_supports_markdown_layered_sections() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();

    let mut params = task_params("briefing");
    params.format = Some("markdown".to_string());
    params.task = Some("Prepare feature handoff".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];

    let body = server
        .tachi_task(Parameters(params))
        .await
        .expect("markdown briefing should succeed");

    assert!(body.starts_with("# Feature Briefing"), "{body}");
    for section in [
        "## Canonical Docs / Specs",
        "## Run Artifacts",
        "## Board State",
        "## Guide / SOP",
        "## Recommended Dispatch",
        "## Relevant Skills / Profiles",
        "## Wiki Decisions / Lessons",
        "## Memory Fragments / Checkpoints",
        "## Eval Evidence",
        "## Next Action",
    ] {
        assert!(body.contains(section), "missing {section}: {body}");
    }
    assert!(body.contains("Dispatch args:"), "{body}");
}

#[test]
fn tachi_task_intake_parses_issue_refs_without_accepting_pr_urls() {
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref("kckylechen1/tachi#194", None),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref(
            "https://github.com/kckylechen1/tachi/issues/194/",
            None
        ),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref("#194", Some("kckylechen1/tachi")),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref(
            "https://github.com/kckylechen1/tachi/pull/194",
            None
        ),
        None
    );
}

#[tokio::test]
async fn tachi_task_intake_requires_issue_target_before_github_access() {
    let server = make_server();
    let params = task_params("intake");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing issue target should fail before GitHub access");
    assert_eq!(
        err,
        "intake requires either repo+number or issue_ref='owner/repo#123' / GitHub issue URL"
    );
}

#[tokio::test]
async fn tachi_task_link_pr_requires_flow_id_before_github_access() {
    let server = make_server();
    let mut params = task_params("link_pr");
    params.pr_ref = Some("kckylechen1/tachi#229".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing flow_id should fail before GitHub access");
    assert_eq!(err, "flow_id is required for link_pr");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_intake_and_link_pr_artifacts_feed_briefing() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000001Z_intake_link_pr_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: Vec::new(),
    };
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
    )
    .expect("write intake artifacts");

    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 229,
        title: "Add task PR status preview".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/229".to_string(),
        head_ref: Some("feat/task-pr-status".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status json");
    assert_eq!(status["issue_ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(status["pr_ref"], json!("kckylechen1/tachi#229"));
    assert_eq!(status["github"]["issue_number"], json!(194));
    assert_eq!(status["github"]["pr_number"], json!(229));
    assert_eq!(status["github"]["merge_state"], json!("merged"));
    assert!(run_dir.join("instruction.md").exists());
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("github_issue_linked"), "{events}");
    assert!(events.contains("github_pr_updated"), "{events}");
    assert_eq!(
        crate::task_lifecycle::resolve_link_pr_issue_ref(flow_id, None).expect("inherited issue"),
        Some("kckylechen1/tachi#194".to_string())
    );
    assert!(
        crate::task_lifecycle::resolve_link_pr_issue_ref(flow_id, Some("other/repo#999"))
            .expect_err("mismatched issue_ref should be rejected")
            .contains("link_pr issue_ref mismatch")
    );

    let mut params = task_params("briefing");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("briefing should read flow docs");
    let briefing: Value = serde_json::from_str(&raw).expect("briefing JSON");
    assert!(briefing["canonical_docs"]
        .as_array()
        .is_some_and(|docs| docs.iter().any(|doc| {
            doc["kind"] == json!("flow_doc")
                && doc["path"] == json!("docs/engineering/architecture/subagent-eval-system.md")
        })));
    assert!(briefing["run_artifacts"]
        .as_array()
        .is_some_and(|artifacts| artifacts.iter().any(|artifact| {
            artifact["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("instruction.md"))
                && artifact["exists"] == json!(true)
        })));
}

#[tokio::test]
async fn tachi_task_build_references_reuses_workflow_closure() {
    let server = make_server();
    let mut params = task_params("build_references");
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/agent-flow.md".to_string()];
    params.related_issues = vec!["#153".to_string(), "kckylechen1/tachi#194".to_string()];

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("task build_references should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("task response JSON");
    assert_eq!(
        parsed["references"],
        json!([
            "kckylechen1/tachi#194",
            "docs/engineering/architecture/agent-flow.md",
            "#153"
        ])
    );
}

#[tokio::test]
async fn tachi_task_close_loop_writes_wiki_with_references() {
    let server = make_server();
    let mut params = task_params("close_loop");
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/agent-flow.md".to_string()];
    params.related_issues = vec!["#153".to_string()];
    params.wiki_title = Some("Task closure facade smoke".to_string());
    params.wiki_text = Some("Closed loop lesson through tachi_task facade.".to_string());
    params.wiki_topic = Some("task-closure-facade".to_string());
    params.force = true;

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("task close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("task response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("close_loop"));
    let wiki_id = parsed["wiki"]["id"].as_str().expect("wiki id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: wiki_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki entry");
    let entry: Value = serde_json::from_str(&fetched).expect("entry JSON");
    assert_eq!(
        entry["metadata"]["source_refs"],
        json!([
            "kckylechen1/tachi#194",
            "docs/engineering/architecture/agent-flow.md",
            "#153"
        ])
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_close_loop_marks_flow_complete_for_ux_matrix() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000006Z_close_loop_marker_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 239,
        title: "Persist close_loop marker".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/239".to_string(),
        doc_paths: vec!["docs/engineering/architecture/credential-adapters-cleanup.md".to_string()],
        spec_paths: Vec::new(),
    };
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Persist close_loop marker",
        &issue,
    )
    .expect("write intake artifacts");

    let mut close_params = task_params("close_loop");
    close_params.flow_id = Some(flow_id.to_string());
    close_params.issue_ref = Some("kckylechen1/tachi#239".to_string());
    close_params.wiki_title = Some("Close loop marker smoke".to_string());
    close_params.wiki_text = Some("Close loop should mark the flow complete.".to_string());
    close_params.wiki_topic = Some("close-loop-marker".to_string());
    close_params.force = true;
    let raw = server
        .tachi_task(Parameters(close_params))
        .await
        .expect("close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("close_loop response JSON");
    assert_eq!(parsed["ok"], json!(true));

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    assert!(run_dir.join("close_loop.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("closed_loop"));
    assert!(status["artifacts"]["close_loop"]
        .as_str()
        .is_some_and(|path| path.ends_with("close_loop.json")));

    let mut ux_params = task_params("ux_matrix");
    ux_params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(ux_params))
        .await
        .expect("ux_matrix should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["overall"], json!("complete"));
    let matrix = parsed["matrix"].as_array().expect("matrix array");
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("close_loop") && step["status"] == json!("passed") }));
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_marker_updates_flow_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260608T000007Z_dispatch_marker_test";
    let dispatch_id = "20260608T000007Z-custom-marker";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch idempotently");
    assert!(
        crate::task_lifecycle::mark_task_dispatch(flow_id, "../bad", json!({})).is_err(),
        "dispatch marker ids must stay filename-safe"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("dispatch"));
    assert_eq!(status["state"], json!("dispatched"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    assert!(
        std::path::Path::new(card_path).exists(),
        "dispatch card should exist: {status:#}"
    );
    assert_eq!(
        status["dispatch_cards"].as_array().map(Vec::len),
        Some(1),
        "dispatch card list should not duplicate entries: {status:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("\"event\":\"dispatch_linked\""), "{events}");
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_completion_marker_updates_card_and_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000001Z_dispatch_completion_marker_test";
    let dispatch_id = "20260609T000001Z-custom-complete";

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
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
            "tests_run": ["cargo test -p memory-server dispatch_tests"],
        }),
    )
    .expect("mark completion");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
        }),
    )
    .expect("mark completion idempotently");

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["eval_memory_id"],
        json!("memory-eval-001")
    );
    assert_eq!(
        status["artifacts"]["dispatch_completions"][dispatch_id]["outcome"],
        json!("success")
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(
        card["completion"]["eval_path"],
        json!("/eval/2026-06-09/eval-link-001")
    );
    assert_eq!(
        card["completion_history"].as_array().map(Vec::len),
        Some(1),
        "same eval should not duplicate completion history: {card:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains("\"event\":\"dispatch_completed\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_dispatch_with_flow_id_records_dispatch_card() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (server, _temp_home) = make_server_with_temp_home();
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = "flow_20260608T000008Z_dispatch_card_test";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke flow dispatch marker");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert!(
        status["dispatch_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(dispatch_id))),
        "flow status should include dispatch id {dispatch_id}: {status:#}"
    );
    assert!(
        status["artifacts"]["dispatches"][dispatch_id]
            .as_str()
            .is_some_and(|path| path.ends_with(".json")),
        "flow status should link compact dispatch card: {status:#}"
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["suggested_complete"]["tool"], json!("tachi_task"));
    assert_eq!(
        card["suggested_complete"]["arguments"]["action"],
        json!("complete")
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["dispatch_id"],
        json!(dispatch_id)
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["flow_id"],
        json!(flow_id)
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains(dispatch_id) && events.contains("\"event\":\"dispatch_linked\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_links_eval_to_flow_dispatch_card_and_ux_matrix() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
async fn tachi_task_board_filters_to_flow_dispatch_ids() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000005Z_board_flow_filter";
    let dispatch_id = "20260609T000005Z-codex-flow";
    let other_dispatch_id = "20260609T000006Z-codex-other";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "codex",
            "profile": "codex_53_fast",
            "task": "flow task",
        }),
    )
    .expect("mark dispatch");
    for (id, task) in [
        (dispatch_id, "flow task"),
        (other_dispatch_id, "other task"),
    ] {
        let run_dir = temp_home.path().join("runs").join(id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        if id == dispatch_id {
            std::fs::write(run_dir.join("result.md"), "worker completed").expect("result");
        }
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::to_string_pretty(&json!({
                "dispatch_id": id,
                "agent": "codex",
                "task": task,
                "state": "TASK_STATE_COMPLETED",
                "exit_code": 0,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .expect("status json"),
        )
        .expect("write status");
    }

    let mut params = task_params("board");
    params.flow_id = Some(flow_id.to_string());
    params.limit = Some(20);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("board should succeed");
    let board: Value = serde_json::from_str(&raw).expect("board JSON");
    assert_eq!(board["flow_id"], json!(flow_id), "{board:#}");
    assert_eq!(board["run_count"], json!(1), "{board:#}");
    let tasks = board["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1, "{board:#}");
    assert_eq!(tasks[0]["dispatch_id"], json!(dispatch_id), "{board:#}");
    assert_eq!(
        tasks[0]["state"],
        json!("TASK_STATE_COMPLETED"),
        "{board:#}"
    );
    assert_eq!(tasks[0]["exit_code"], json!(0), "{board:#}");
    assert_eq!(tasks[0]["result_written"], json!(true), "{board:#}");
    assert_eq!(tasks[0]["state_source"], json!("run"), "{board:#}");
    assert!(
        tasks[0]["run_dir"]
            .as_str()
            .is_some_and(|path| path.ends_with(dispatch_id)),
        "{board:#}"
    );
}

#[test]
fn tachi_task_pr_status_parses_repo_number_and_pr_ref() {
    let mut params = task_params("pr_status");
    params.repo = Some("kckylechen1/tachi".to_string());
    params.number = Some(228);
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("repo+number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.repo = None;
    params.number = None;
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("owner/repo#number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some("https://github.com/kckylechen1/tachi/pull/228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some(" https://github.com/kckylechen1/tachi/pull/228/ ".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("trimmed github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );
}

#[test]
fn tachi_task_pr_status_rejects_ambiguous_pr_refs() {
    for pr_ref in [
        "",
        "kckylechen1/tachi#",
        "kckylechen1/tachi/extra#228",
        "https://github.com/kckylechen1/tachi/issues/228",
        "https://github.com/kckylechen1/tachi/pull/228/files",
    ] {
        let mut params = task_params("pr_status");
        params.pr_ref = Some(pr_ref.to_string());
        assert!(
            crate::tools::resolve_task_pr_status_target(&params).is_err(),
            "unexpectedly accepted pr_ref={pr_ref:?}"
        );
    }
}

#[test]
fn tachi_task_pr_status_builds_safe_merge_preview_params() {
    let mut params = task_params("pr_status");
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    params.flow_id = Some("flow_pr_status".to_string());
    params.merge_policy = Some("strict".to_string());
    params.strategy = Some("squash".to_string());
    params.confirm = true;

    let gh_params =
        crate::tools::build_task_pr_status_gh_params(&params).expect("pr_status params");
    assert_eq!(gh_params.action, "safe_merge");
    assert_eq!(gh_params.repo, "kckylechen1/tachi");
    assert_eq!(gh_params.number, Some(228));
    assert_eq!(gh_params.dry_run, Some(true));
    assert!(!gh_params.confirm);
    assert_eq!(gh_params.flow_id.as_deref(), Some("flow_pr_status"));
    assert_eq!(gh_params.merge_policy.as_deref(), Some("strict"));
    assert_eq!(gh_params.merge_strategy, None);
}

#[tokio::test]
async fn tachi_task_pr_status_requires_repo_number_or_parseable_ref() {
    let server = make_server();
    let params = task_params("pr_status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(
        err,
        "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[tokio::test]
async fn tachi_task_release_note_requires_flow_or_pr_ref_before_github_access() {
    let server = make_server();
    let params = task_params("release_note");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing release note target should fail before GitHub access");
    assert_eq!(
        err,
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_writes_flow_artifact_with_refs() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000002Z_release_note_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: vec!["docs/engineering/specs/dispatch-policy.md".to_string()],
    };
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: Some("feat/task-intake-link-pr".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "command": "cargo test -p memory-server tachi_task_release_note", "status": "passed" },
                { "kind": "gitleaks", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut params = task_params("release_note");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("release_note"));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    assert_eq!(parsed["issue_ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(parsed["pr_ref"], json!("kckylechen1/tachi#230"));
    assert_eq!(parsed["inputs"]["verification_present"], json!(true));
    let note_path = parsed["release_note_path"]
        .as_str()
        .expect("release note path");
    assert!(note_path.ends_with("release_note.md"), "{note_path}");
    assert!(run_dir.join("release_note.md").exists());
    let note = std::fs::read_to_string(run_dir.join("release_note.md")).expect("release note");
    for expected in [
        "# Release Note",
        "Issue: `kckylechen1/tachi#194`",
        "PR: `kckylechen1/tachi#230`",
        "Merge state: `merged`",
        "spec: `docs/engineering/specs/dispatch-policy.md`",
        "doc: `docs/engineering/architecture/subagent-eval-system.md`",
        "Overall: `passed`",
        "`passed` cargo test -p memory-server tachi_task_release_note",
    ] {
        assert!(note.contains(expected), "missing {expected}: {note}");
    }
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("release_note_generated"));
    assert!(status["release_note_path"]
        .as_str()
        .is_some_and(|path| path.ends_with("release_note.md")));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_rejects_mismatched_pr_ref_for_cached_flow_pr() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000003Z_release_note_mismatch";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: None,
        base_ref: None,
        review_decision: None,
        mergeable: None,
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let mut params = task_params("release_note");
    params.flow_id = Some(flow_id.to_string());
    params.pr_ref = Some("kckylechen1/tachi#999".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("mismatched pr_ref should be rejected");
    assert!(
        err.contains("release_note pr_ref mismatch"),
        "unexpected error: {err}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_skips_empty_optional_github_fields() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000004Z_release_note_empty_fields";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: None,
        base_ref: None,
        review_decision: Some(String::new()),
        mergeable: Some(String::new()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let mut params = task_params("release_note");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    let note = parsed["release_note"].as_str().expect("release note text");
    assert!(!note.contains("Review: ``"), "{note}");
    assert!(!note.contains("Mergeable: ``"), "{note}");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_ux_matrix_writes_feature_workflow_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000005Z_ux_matrix_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: vec!["docs/engineering/specs/dispatch-policy.md".to_string()],
    };
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: Some("feat/task-intake-link-pr".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::shell_ops::merge_github_status(
        &run_dir,
        json!({
            "merge_state": "ready",
            "policy": "standard",
            "requested_mode": "preview",
            "will_merge": false,
        }),
    )
    .expect("merge github status");
    let mut status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    status["dispatch_ids"] = json!(["dispatch-ux-matrix"]);
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&status).expect("status json"),
    )
    .expect("write status");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "id": "gitleaks", "kind": "gitleaks", "status": "passed", "required": true }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut release_params = task_params("release_note");
    release_params.flow_id = Some(flow_id.to_string());
    server
        .tachi_task(Parameters(release_params))
        .await
        .expect("release note should be generated");

    let mut params = task_params("ux_matrix");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("ux_matrix"));
    assert_eq!(parsed["overall"], json!("ready_for_close_loop"));
    let matrix = parsed["matrix"].as_array().expect("matrix array");
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("dispatch") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("verification") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("pr_status") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("release_note") && step["status"] == json!("passed") }));
    let ux_path = parsed["ux_matrix_path"]
        .as_str()
        .expect("ux matrix artifact path");
    assert!(ux_path.ends_with("ux_matrix.json"), "{ux_path}");
    assert!(run_dir.join("ux_matrix.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert!(status["artifacts"]["ux_matrix"]
        .as_str()
        .is_some_and(|path| path.ends_with("ux_matrix.json")));
}

#[tokio::test]
async fn tachi_task_ux_matrix_without_flow_is_read_only_starting_checklist() {
    let server = make_server();
    let mut params = task_params("ux_matrix");
    params.task = Some("Review a new Tachi feature request".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should work before intake flow exists");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("ux_matrix"));
    assert_eq!(parsed["flow_id"], Value::Null);
    assert_eq!(parsed["ux_matrix_path"], Value::Null);
    assert_eq!(parsed["overall"], json!("needs_action"));
    assert!(parsed["matrix"].as_array().is_some_and(|matrix| matrix
        .iter()
        .any(|step| step["id"] == json!("intake") && step["status"] == json!("ready"))));
    assert!(parsed["matrix"].as_array().is_some_and(|matrix| matrix
        .iter()
        .any(|step| step["id"] == json!("canonical_docs") && step["status"] == json!("pending"))));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_ux_matrix_creates_new_flow_directory() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let flow_id = "flow_20260609T000007Z_ux_matrix_new_flow";
    let mut params = task_params("ux_matrix");
    params.flow_id = Some(flow_id.to_string());
    params.task = Some("Start a new UX matrix before intake".to_string());

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should create a new flow dir");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    let path = parsed["ux_matrix_path"].as_str().expect("ux matrix path");
    assert!(path.ends_with("ux_matrix.json"), "{path}");
    assert!(std::path::Path::new(path).exists());
}

#[tokio::test]
async fn dispatch_prompt_does_not_require_self_complete_without_tachi_mcp() {
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "read-only worker");
    params.inject_tachi_mcp = Some(false);
    params.mcp_access = Some(DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["tachi_memory".to_string()],
        allowed_mcp_servers: Vec::new(),
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });

    let prompt = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params)
        .await
        .prompt;
    assert!(
        prompt.contains("leader will call `tachi_task(action=\"complete\")`"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("- Call `tachi_task(action=\"complete\")` when done"),
        "{prompt}"
    );

    params.inject_tachi_mcp = Some(true);
    let prompt = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params)
        .await
        .prompt;
    assert!(
        prompt.contains("- Call `tachi_task(action=\"complete\")` when done"),
        "{prompt}"
    );
}

#[tokio::test]
async fn dispatch_prompt_includes_task_route_overlay() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &dispatch_params(Some("codex"), "帮我编译二进制并且跑起来验证功能"),
    )
    .await;

    assert!(prompt.contains("## Tachi task route"), "{prompt}");
    assert!(prompt.contains("intent: test_request"), "{prompt}");
    assert!(prompt.contains("skill:coding-test-strategy"), "{prompt}");
    assert!(prompt.contains("## Required skill invocation"), "{prompt}");
    assert!(prompt.contains("tachi_progress_check(check)"), "{prompt}");
}

#[tokio::test]
async fn dispatch_prompt_injects_applicable_feedback_rules_separately() {
    let server = make_server();
    let rule_id = save_grep_evidence_feedback_rule(&server).await;

    let mut params = dispatch_params(
        Some("codex"),
        "Review the repo for unused functions and dead code claims.",
    );
    params.profile = Some("codex_55_review".to_string());
    params.stage = Some("review".to_string());

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;
    let prompt = assembly.prompt;
    assert!(prompt.contains("## Applicable feedback rules"), "{prompt}");
    assert!(prompt.contains("Subagent audit prompts require explicit search evidence"));
    assert!(prompt.contains("grep_commands"));
    assert!(prompt.contains("paths_searched"));
    assert!(!prompt.contains("## Relevant context from Tachi memory/wiki"));
    assert_eq!(assembly.feedback_rules["status"], json!("applied"));
    assert_eq!(assembly.feedback_rules["rules"][0]["id"], json!(rule_id));
}

#[tokio::test]
async fn dispatch_prompt_invokes_stage_and_waza_skills_for_execute_slice() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: Some("claude".to_string()),
            profile: None,
            task: "修好 memory-server 报错，先找根因再改".to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: Some("execute:runtime".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        },
    )
    .await;

    assert!(prompt.contains("## Required skill invocation"), "{prompt}");
    assert!(
        prompt.contains("Using skills: <ids>"),
        "worker should be told to declare skill usage: {prompt}"
    );
    assert!(
        prompt.contains("### skill:superpowers-executing-plans"),
        "stage skill should be injected for execute:* stage: {prompt}"
    );
    assert!(
        prompt.contains("### skill:waza-hunt"),
        "debug task should inject Waza hunt: {prompt}"
    );
    assert!(
        prompt.contains("embedded_contract"),
        "child prompt should include fallback contract when tachi_skill MCP is unavailable: {prompt}"
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

#[tokio::test]
async fn dispatch_prompt_invokes_native_subagent_factory_for_dispatch_stage() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: Some("codex".to_string()),
            profile: None,
            task: "Split this implementation plan into worker slices and run review gates"
                .to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: Some("dispatch".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        },
    )
    .await;

    assert!(
        prompt.contains("### skill:superpowers-subagent-driven-development"),
        "dispatch stage should invoke the worker factory skill: {prompt}"
    );
    assert!(
        prompt.contains("### skill:superpowers-executing-plans"),
        "dispatch stage should still carry the execution skill: {prompt}"
    );
    assert!(
        prompt.contains("### skill:waza-tachi"),
        "dispatch stage should carry the Tachi workflow skill: {prompt}"
    );
}

#[tokio::test]
async fn dispatch_prompt_injects_sft_examples_as_style_only_context() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-dispatch-memory");
            live.path = "/scratch/sigil/live-dispatch".to_string();
            live.text = "fix dispatch routing regression current implementation fact.".to_string();
            live.summary = "Current dispatch fact".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut sft = make_entry("sft-dispatch-example");
            sft.path = "/sft/v4/strict/engineering/42".to_string();
            sft.text = "[结论] fix dispatch routing regression historical answer shape.\n[根因] Historical root cause.\n[方案] Historical proposal.\n[反方案] Historical anti-pattern.\n[验证] Historical verification.".to_string();
            sft.summary = "SFT dispatch example".to_string();
            sft.topic = "sft-memory".to_string();
            sft.importance = 1.0;
            store.upsert(&sft).map_err(|e| e.to_string())
        })
        .expect("seed dispatch SFT example");

    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &dispatch_params(Some("codex"), "Fix dispatch routing regression"),
    )
    .await;

    assert!(
        prompt.contains("## Relevant context from Tachi memory/wiki"),
        "{prompt}"
    );
    assert!(
        prompt.contains("/scratch/sigil/live-dispatch")
            && prompt.contains("current implementation fact"),
        "live memory should still be normal context: {prompt}"
    );
    assert!(
        prompt.contains("## SFT gold examples (style only, not live facts)"),
        "{prompt}"
    );
    assert!(
        prompt.contains("/sft/v4/strict/engineering/42"),
        "SFT example should be isolated under the SFT section: {prompt}"
    );
    assert!(
        prompt.contains("Do not treat historical SFT samples as current project truth"),
        "{prompt}"
    );
}

#[tokio::test]
async fn dispatch_prompt_includes_profile_overlay_and_capability_bundle() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    "dispatch_profile_card_overlays",
                    "claude_plan",
                    &json!({
                        "kind": "profile_card_loadout_overlay",
                        "profile": "claude_plan",
                        "add_signature_skills": ["skill:planning-ux-review"],
                        "add_passive_traits": ["evidence_backed_planning"],
                        "add_evidence_required": ["acceptance_criteria"],
                        "add_weak_against": ["plan_request"],
                        "demotion_targets": ["skill:superpowers-writing-plans"],
                        "source_proposal_ids": ["proposal-fixture"],
                    })
                    .to_string(),
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed profile/card overlay");
    let mut params = dispatch_params(Some("claude"), "Plan profile-based MCP access");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());
    params.tool_profile = Some("delegate".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.flow_id = Some("flow-194".to_string());
    params.auto_capability_bundle = Some(true);
    params.mcp_access = Some(DispatchMcpAccessParams {
        inject_tachi_mcp: Some(true),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["tachi_memory".to_string(), "tachi_wiki".to_string()],
        allowed_mcp_servers: Vec::new(),
        github_read: Some(true),
        write_actions: Some(false),
        issue_refs: vec!["kckylechen1/tachi#194".to_string()],
        pr_refs: Vec::new(),
        fallback: Some("report unavailable context instead of guessing".to_string()),
    });

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("## Dispatch profile"), "{prompt}");
    assert!(prompt.contains("profile: claude_plan"), "{prompt}");
    assert!(prompt.contains("tachi_tool_profile: delegate"), "{prompt}");
    assert!(
        prompt.contains("issue_ref: kckylechen1/tachi#194"),
        "{prompt}"
    );
    assert!(prompt.contains("- skill_loadout:"), "{prompt}");
    assert!(
        prompt.contains("skill:superpowers-subagent-driven-development"),
        "{prompt}"
    );
    assert!(
        prompt.contains("skill:coding-architecture-decision"),
        "{prompt}"
    );
    assert!(prompt.contains("skill:planning-ux-review"), "{prompt}");
    assert!(
        prompt.contains("projected_signature_skills: skill:planning-ux-review"),
        "{prompt}"
    );
    assert!(
        prompt.contains("projection_status: applied_overlay"),
        "{prompt}"
    );
    assert!(
        prompt.contains("projected_passive_traits: evidence_backed_planning"),
        "{prompt}"
    );
    assert!(
        prompt.contains("passive_traits: plan_before_execute"),
        "{prompt}"
    );
    assert!(prompt.contains("- evidence_contract:"), "{prompt}");
    assert!(
        prompt.contains("required: plan, risks, validation_plan, acceptance_criteria"),
        "{prompt}"
    );
    assert!(
        prompt.contains("projected_required: acceptance_criteria"),
        "{prompt}"
    );
    assert!(
        prompt.contains("evidence_projection_status: applied_overlay"),
        "{prompt}"
    );
    assert!(prompt.contains("- mbit_card_evolution:"), "{prompt}");
    assert!(
        prompt.contains("projected_weak_against: plan_request"),
        "{prompt}"
    );
    assert!(
        prompt.contains("demotion_targets: skill:superpowers-writing-plans"),
        "{prompt}"
    );
    assert!(prompt.contains("## Capability Bundle"), "{prompt}");
}

#[tokio::test]
async fn dispatch_prompt_trace_records_capability_bundle_injection() {
    let server = make_server();
    let mut params = dispatch_params(Some("claude"), "Plan profile-based MCP access");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());
    params.auto_capability_bundle = Some(true);

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;

    assert!(
        assembly.prompt.contains("## Capability Bundle"),
        "{}",
        assembly.prompt
    );
    assert_eq!(assembly.capability_bundle["requested"], json!(true));
    assert_eq!(assembly.capability_bundle["status"], json!("injected"));
    assert_eq!(assembly.capability_bundle["source"], json!("params"));
    assert_eq!(assembly.capability_bundle["disabled"], json!(false));
    assert_eq!(assembly.capability_bundle["injected"], json!(true));
    assert!(
        assembly.capability_bundle["section"]["block"]
            .as_str()
            .is_some_and(|block| block.contains("## Capability Bundle")),
        "trace should retain the injected section: {}",
        assembly.capability_bundle
    );
}

#[tokio::test]
async fn dispatch_prompt_trace_records_capability_bundle_disabled() {
    let server = make_server();
    let mut params = dispatch_params(Some("claude"), "Plan profile-based MCP access");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());
    params.auto_capability_bundle = Some(false);

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;

    assert!(
        !assembly.prompt.contains("## Capability Bundle"),
        "disabled bundle should not be injected: {}",
        assembly.prompt
    );
    assert_eq!(assembly.capability_bundle["requested"], json!(false));
    assert_eq!(assembly.capability_bundle["status"], json!("disabled"));
    assert_eq!(assembly.capability_bundle["source"], json!("params"));
    assert_eq!(assembly.capability_bundle["disabled"], json!(true));
    assert_eq!(assembly.capability_bundle["injected"], json!(false));
    assert_eq!(
        assembly.capability_bundle["reason"],
        json!("auto_capability_bundle=false")
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_response_and_flow_card_link_capability_bundle_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = "flow_20260609T000003Z_capability_bundle_card";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "status": "active",
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some(flow_id.to_string());
    params.auto_capability_bundle = Some(true);

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    assert_eq!(response["capability_bundle"]["requested"], json!(true));
    assert_eq!(response["capability_bundle"]["status"], json!("injected"));
    assert_eq!(response["capability_bundle"]["injected"], json!(true));
    assert_eq!(response["feedback_rules"]["status"], json!("none"));
    let artifact_file = response["capability_bundle_file"]
        .as_str()
        .expect("capability bundle file");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(artifact_file).expect("artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["requested"], json!(true));
    assert_eq!(artifact["status"], json!("injected"));
    assert_eq!(artifact["injected"], json!(true));
    assert_eq!(artifact["feedback_rules"]["status"], json!("none"));

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["capability_bundle"]["requested"], json!(true));
    assert_eq!(card["capability_bundle"]["injected"], json!(true));
    assert_eq!(
        card["capability_bundle_file"].as_str(),
        Some(artifact_file),
        "flow card should link the same capability bundle artifact"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_explicit_false_writes_disabled_capability_bundle_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke disabled capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.auto_capability_bundle = Some(false);

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["capability_bundle"]["status"], json!("disabled"));
    assert_eq!(response["capability_bundle"]["requested"], json!(false));
    assert_eq!(response["capability_bundle"]["disabled"], json!(true));
    assert_eq!(response["capability_bundle"]["injected"], json!(false));

    let artifact_file = response["capability_bundle_file"]
        .as_str()
        .expect("capability bundle file");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(artifact_file).expect("artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["status"], json!("disabled"));
    assert_eq!(artifact["source"], json!("params"));

    let prompt_file = response["prompt_file"].as_str().expect("prompt file");
    let prompt = std::fs::read_to_string(prompt_file).expect("prompt");
    assert!(
        !prompt.contains("## Capability Bundle"),
        "disabled dispatch should not inject bundle section: {prompt}"
    );
}

#[test]
fn dispatch_ids_are_unique_within_same_second() {
    let now = chrono::Utc::now();
    let id_a = crate::dispatch_ops::new_dispatch_id(now, "my agent/here");
    let id_b = crate::dispatch_ops::new_dispatch_id(now, "my agent/here");
    assert_ne!(
        id_a, id_b,
        "same second + same agent must still produce unique IDs"
    );
    let prefix = format!("{}-my-agent-here", now.format("%Y%m%dT%H%M%SZ"));
    assert!(
        id_a.starts_with(&prefix),
        "id_a should start with expected prefix: {id_a}"
    );
    assert!(
        id_b.starts_with(&prefix),
        "id_b should start with expected prefix: {id_b}"
    );
}

#[tokio::test]
async fn dispatch_rejects_unknown_agent_with_fleet_hint() {
    let server = make_server();
    let err = crate::dispatch_ops::handle_tachi_dispatch(
        &server,
        TachiDispatchParams {
            agent: Some("gemini".to_string()),
            profile: None,
            task: "noop".to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: None,
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        },
    )
    .await
    .expect_err("gemini should not be in the fleet");
    assert!(err.contains("Unknown agent"), "err: {err}");
    assert!(err.contains("claude"), "err: {err}");
    assert!(err.contains("grok"), "err: {err}");
}

#[tokio::test]
async fn custom_dispatch_rejects_mcp_injection() {
    let server = make_server();
    let mut params = dispatch_params(Some("custom"), "should fail before subprocess");
    params.inject_tachi_mcp = Some(true);
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("custom backend must reject MCP injection");
    assert!(
        err.contains("custom backend"),
        "unexpected custom injection error: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_response_includes_suggested_complete_payload() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke custom dispatch completion skeleton");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some("flow-complete-skeleton".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let suggested = &response["suggested_complete_command"];

    assert_eq!(suggested["tool"], serde_json::json!("tachi_task"));
    assert_eq!(
        suggested["arguments"]["action"],
        serde_json::json!("complete")
    );
    assert_eq!(
        suggested["arguments"]["dispatch_id"], response["dispatch_id"],
        "completion skeleton should carry dispatch_id"
    );
    assert_eq!(
        suggested["arguments"]["profile"],
        serde_json::json!("glm_51_impl")
    );
    assert_eq!(
        suggested["arguments"]["flow_id"],
        serde_json::json!("flow-complete-skeleton")
    );
    assert!(suggested["arguments"]["tests_run"].is_array());
    assert!(suggested["arguments"]["evidence_refs"].is_array());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_injects_env_without_response_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "dispatch_env_profile": {
                    "provider": "test",
                    "entries": {
                        "api_key": "DISPATCH_PROFILE_SECRET"
                    },
                    "allowed_consumers": {
                        "agents": ["custom"]
                    },
                    "materializers": [
                        {
                            "type": "env",
                            "source": "api_key",
                            "target": "PROFILE_ENV_SECRET"
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "dispatch-profile-secret-value";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DISPATCH_PROFILE_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke credential dispatch env");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.credential_profiles = vec!["dispatch_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('PROFILE_ENV_SECRET') == 'dispatch-profile-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with materialized credential");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["steps"][0]["output"],
        serde_json::json!("env:PROFILE_ENV_SECRET")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["status"],
        serde_json::json!("prepared_env")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["redacted"],
        serde_json::json!(true)
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("present"),
        "subprocess should receive credential env without printing it; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"credentials_materialized\""),
        "trajectory should record redacted credential materialization: {trajectory}"
    );
    assert!(
        !trajectory.contains(secret_value),
        "trajectory must not leak secret value: {trajectory}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_injects_config_overlay_env_without_response_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "opencode_config_profile": {
                    "provider": "opencode",
                    "entries": {
                        "api_key": "OPENCODE_ROUTER_SECRET"
                    },
                    "allowed_consumers": {
                        "agents": ["custom"]
                    },
                    "materializers": [
                        {
                            "type": "config_overlay",
                            "source": "api_key",
                            "target": "OPENCODE_CONFIG_CONTENT",
                            "template": {
                                "provider": "openai",
                                "apiKey": "{{secret}}"
                            }
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "opencode-router-secret-value";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch config credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_ROUTER_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch config credential test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke credential config overlay dispatch");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.credential_profiles = vec!["opencode_config_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import json, os; cfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}')); print('config-present' if cfg.get('apiKey') == 'opencode-router-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with materialized config overlay");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["steps"][0]["output"],
        serde_json::json!("config_overlay:OPENCODE_CONFIG_CONTENT")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["status"],
        serde_json::json!("prepared_config_env")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["redacted"],
        serde_json::json!(true)
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("config-present"),
        "subprocess should receive rendered config without printing it; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"credentials_materialized\""),
        "trajectory should record redacted credential materialization: {trajectory}"
    );
    assert!(
        !trajectory.contains(secret_value),
        "trajectory must not leak secret value: {trajectory}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_materialize_without_explicit_params() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "opencode_shared": {
                    "provider": "opencode",
                    "entries": {
                        "api_key": "OPENCODE_SHARED_TEST_SECRET"
                    },
                    "allowed_consumers": {
                        "profiles": ["opencode_builder"]
                    },
                    "materializers": [
                        {
                            "type": "config_overlay",
                            "source": "api_key",
                            "target": "OPENCODE_CONFIG_CONTENT",
                            "template": {
                                "provider": "openai",
                                "apiKey": "{{secret}}"
                            }
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "opencode-shared-profile-secret";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch profile credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_SHARED_TEST_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch profile credential binding test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "smoke profile-declared credential dispatch");
    params.profile = Some("opencode_builder".to_string());
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import json, os; cfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}')); print('profile-config-present' if cfg.get('apiKey') == 'opencode-shared-profile-secret' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with profile-declared credential");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["profile"]["credential_profiles"][0],
        serde_json::json!("opencode_shared")
    );
    assert_eq!(
        response["credentials"][0]["profile"],
        serde_json::json!("opencode_shared")
    );
    assert_eq!(
        response["credentials"][0]["consumer"],
        serde_json::json!("opencode_builder")
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("profile-config-present"),
        "subprocess should receive profile-declared credential config; result={result}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_respect_profile_allowlist() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        r#"{
          "credential_profiles": {
            "opencode_shared": {
              "entries": { "api_key": "OPENCODE_SHARED_DENIED_SECRET" },
              "allowed_consumers": { "profiles": ["opencode_builder"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "OPENCODE_DENIED_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch profile denied password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_SHARED_DENIED_SECRET".to_string(),
            value: "denied-profile-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch profile allowlist denial test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "should fail before spawn");
    params.profile = Some("glm_51_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["opencode_shared".to_string()];
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("non-allowed selected profile should be denied before spawn");
    assert!(err.contains("denied_consumer"), "unexpected error: {err}");
    assert!(err.contains("glm_51_impl"), "unexpected error: {err}");
    assert!(
        !err.contains("denied-profile-secret-value"),
        "error must not leak secret value: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_credentials_can_allow_backend_agent_name() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "backend_agent_profile": {
              "entries": { "api_key": "BACKEND_AGENT_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "BACKEND_AGENT_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch backend password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "BACKEND_AGENT_SECRET".to_string(),
            value: "backend-agent-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "backend agent allowlist test".to_string(),
            allowed_agents: Some(vec!["custom".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "profile selected but agent allowlist is backend");
    params.profile = Some("glm_51_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["backend_agent_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('BACKEND_AGENT_ENV') == 'backend-agent-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("backend agent allowlist should work even with selected dispatch profile");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["consumer"],
        serde_json::json!("custom"),
        "credential consumer should be backend agent when allowed_consumers.agents matches"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(result.contains("present"), "result={result}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_requires_unlocked_vault_before_spawn() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "locked_env_profile": {
              "entries": { "api_key": "LOCKED_DISPATCH_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "LOCKED_ENV_SECRET" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch locked password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "LOCKED_DISPATCH_SECRET".to_string(),
            value: "locked-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential lock test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let mut params = dispatch_params(Some("custom"), "should fail before spawn");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["locked_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("locked vault should fail dispatch before spawn");
    assert!(err.contains("requires unlocked Vault secret"), "err: {err}");
    assert!(err.contains("Vault is locked"), "err: {err}");
    let runs_dir = temp_home.path().join("runs");
    let run_dir = std::fs::read_dir(&runs_dir)
        .expect("runs dir exists")
        .next()
        .expect("failed dispatch should leave run status")
        .expect("run dir entry")
        .path();
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status should exist"),
    )
    .expect("status JSON");
    assert_eq!(status["state"], serde_json::json!("TASK_STATE_FAILED"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_denies_consumer_before_decrypting_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "denied_env_profile": {
              "entries": { "api_key": "DENIED_DISPATCH_SECRET" },
              "allowed_consumers": { "agents": ["other-agent"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "DENIED_ENV_SECRET" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch denied password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DENIED_DISPATCH_SECRET".to_string(),
            value: "denied-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential deny test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let mut params = dispatch_params(Some("custom"), "should fail before decrypt");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["denied_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("denied consumer should fail dispatch before decrypt");
    assert!(
        err.contains("not ready for consumer 'custom'"),
        "err: {err}"
    );
    assert!(err.contains("denied_consumer"), "err: {err}");
    assert!(
        !err.contains("Vault is locked"),
        "denied profile should fail before decrypting/unlocking secret: {err}"
    );
    assert!(
        !err.contains("denied-secret-value"),
        "redacted readiness error must not leak secret: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_legacy_vault_env_binding_still_injects_without_credential_profile() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    std::fs::create_dir_all(project.path().join(".tachi")).expect("create .tachi dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        project.path().join(".tachi/vault.env"),
        "LEGACY_DISPATCH_ENV=vault:LEGACY_DISPATCH_SECRET\n",
    )
    .expect("write vault.env");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch legacy password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "LEGACY_DISPATCH_SECRET".to_string(),
            value: "legacy-dispatch-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "legacy dispatch env test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke legacy vault env dispatch");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('LEGACY_DISPATCH_ENV') == 'legacy-dispatch-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("legacy env dispatch should start");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert!(
        response["credentials"]
            .as_array()
            .is_some_and(|v| v.is_empty()),
        "legacy env path should not synthesize credential reports: {response:#}"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("present"),
        "legacy vault env should still reach subprocess; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"legacy_vault_env_injected\""),
        "legacy env injection should be auditable without values: {trajectory}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_surfaces_dispatch_run_ledger() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "99991231T235959Z-test-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create run ledger fixture");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "custom",
            "task": "smoke board run ledger",
            "state": "TASK_STATE_WORKING",
            "updated_at": "9999-12-31T23:59:59Z",
            "exit_code": null,
            "result_written": false,
        }))
        .expect("serialize status fixture"),
    )
    .expect("write status fixture");

    let board_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("board should render");
    let board: serde_json::Value = serde_json::from_str(&board_raw).expect("board JSON");
    assert!(
        board["run_count"].as_u64().unwrap_or(0) >= 1,
        "board must include run-ledger rows even if kanban search misses: {board:#}"
    );
    assert!(
        board["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| { task["dispatch_id"].as_str() == Some(dispatch_id.as_str()) }),
        "board tasks should include dispatch {dispatch_id}: {board:#}"
    );
    assert!(
        board["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task.get("run_dir").and_then(|v| v.as_str()).is_some()
        }),
        "dispatch should carry run_dir from run ledger: {board:#}"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_marks_abandoned_working_run_as_failed() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260608T000000Z-stale-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create stale run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "stale run should not clog working board",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": 5,
        }))
        .expect("serialize stale status fixture"),
    )
    .expect("write stale status fixture");

    let failed_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("failed".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("failed board should render");
    let failed: serde_json::Value = serde_json::from_str(&failed_raw).expect("failed board JSON");
    assert!(
        failed["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["state"].as_str() == Some("TASK_STATE_FAILED")
                && task["stale"].as_bool() == Some(true)
                && task["stale_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("WORKING"))
        }),
        "failed board should include stale derived run: {failed:#}"
    );

    let working_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("working".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("working board should render");
    let working: serde_json::Value =
        serde_json::from_str(&working_raw).expect("working board JSON");
    assert!(
        !working["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(dispatch_id.as_str())),
        "stale run should not remain on working board: {working:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_caps_corrupt_huge_timeout_before_duration_math() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260608T000001Z-huge-timeout-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create huge-timeout run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "corrupt huge timeout must not panic board",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": i64::MAX,
        }))
        .expect("serialize huge-timeout status fixture"),
    )
    .expect("write huge-timeout status fixture");

    let failed_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("failed".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("board should not panic on corrupt huge timeout");
    let failed: serde_json::Value = serde_json::from_str(&failed_raw).expect("failed board JSON");
    assert!(
        failed["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["state"].as_str() == Some("TASK_STATE_FAILED")
                && task["stale"].as_bool() == Some(true)
        }),
        "capped huge timeout should still allow stale classification: {failed:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

// ─── Phase 6: Dispatch V2 two-stage smoke test ──────────────────────────────
//
// Spawns the full V2 flow against a fake `claude` binary that emits a
// canned plan envelope. Marked `#[ignore]` because:
//   * it writes under a temp `TACHI_HOME` and shells out to `bash`;
//   * it requires `bash` on PATH and a writable temp dir;
//   * it mutates env vars (CLAUDE_BIN, DISPATCH_V2_ENABLED, TACHI_HOME)
//     so it must not run concurrently with other env-sensitive tests.
//
// Run explicitly via:
//     cargo test -p memory-server --lib v2_two_stage_smoke -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn v2_two_stage_smoke() {
    use std::io::Write;

    // Isolated TACHI_HOME so run files don't pollute real one.
    let temp_home = std::env::temp_dir().join(format!("tachi-v2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_home).expect("create temp tachi home");

    // Fake claude binary: prints a JSON envelope matching the pool's
    // expected `{"result": "..."}` shape, embedding a valid plan.
    let fake_claude = temp_home.join("claude-test");
    {
        let mut f = std::fs::File::create(&fake_claude).expect("create fake claude");
        // The pool invokes `claude -p --output-format json --dangerously-skip-permissions`
        // with the prompt on stdin. We ignore stdin and emit a fixed envelope.
        writeln!(
            f,
            "#!/usr/bin/env bash\ncat <<'JSON'\n{{\"result\":\"## Goal\\nDo the smoke test.\\n\\n## Steps\\n1. inspect\\n2. ship\\n\\n## Files\\n- src/lib.rs\\n\\n## Validation\\n- cargo test\\n\"}}\nJSON"
        )
        .unwrap();
    }
    let mut perms = std::fs::metadata(&fake_claude).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_claude, perms).unwrap();

    // Activate V2 + isolate.
    std::env::set_var("TACHI_HOME", &temp_home);
    std::env::set_var("CLAUDE_BIN", &fake_claude);
    std::env::set_var("DISPATCH_V2_ENABLED", "true");
    std::env::set_var("DISPATCH_V2_PLAN_REVIEW", "false");

    let server = make_server();

    // We can't easily call the private handle_tachi_dispatch from
    // outside the crate, but tests live inside the crate so the
    // `pub(crate)` visibility is accessible via crate path.
    let mut params = dispatch_params(Some("custom"), "smoke v2");
    // Execute stage uses a no-op command so the test doesn't need
    // a working claude/codex CLI for Stage 2.
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let resp_json = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("v2 dispatch should succeed");

    let resp: serde_json::Value = serde_json::from_str(&resp_json).expect("v2 response JSON");
    assert_eq!(resp["v2"], serde_json::json!(true), "response: {resp:#}");
    let run_dir = resp["run_dir"].as_str().expect("run_dir present");
    let run_dir = std::path::PathBuf::from(run_dir);

    // Stage 1 artifacts exist immediately.
    let plan = std::fs::read_to_string(run_dir.join("plan.md")).expect("plan.md written");
    assert!(plan.contains("## Goal"), "plan.md content: {plan}");
    assert!(plan.contains("## Validation"), "plan.md content: {plan}");

    let trajectory_path = run_dir.join("trajectory.jsonl");
    let mut trajectory = std::fs::read_to_string(&trajectory_path).expect("trajectory present");
    for _ in 0..30 {
        if trajectory.contains("\"event\":\"execute_started\"") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        trajectory = std::fs::read_to_string(&trajectory_path).expect("trajectory present");
    }
    assert!(trajectory.contains("\"event\":\"dispatch_started\""));
    assert!(trajectory.contains("\"event\":\"plan_generated\""));
    assert!(trajectory.contains("\"event\":\"execute_started\""));
    let progress =
        std::fs::read_to_string(run_dir.join("progress.jsonl")).expect("progress present");
    assert!(progress.contains("\"event\":\"dispatch_started\""));
    assert!(progress.contains("\"event\":\"plan_generated\""));

    // Wait briefly for the spawned stage-2 task to write final status.json.
    for _ in 0..30 {
        if let Ok(raw) = std::fs::read_to_string(run_dir.join("status.json")) {
            if let Ok(status) = serde_json::from_str::<serde_json::Value>(&raw) {
                if status["duration_ms_plan"].as_u64().is_some()
                    && status["duration_ms_execute"].as_u64().is_some()
                {
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status.json written"),
    )
    .expect("status.json valid");
    assert_eq!(status["v2"], serde_json::json!(true));
    assert!(status["duration_ms_plan"].as_u64().is_some());
    assert_eq!(status["plan_review_status"], serde_json::json!("approved"));

    // Cleanup.
    std::env::remove_var("CLAUDE_BIN");
    std::env::remove_var("DISPATCH_V2_ENABLED");
    std::env::remove_var("DISPATCH_V2_PLAN_REVIEW");
    std::env::remove_var("TACHI_HOME");
    let _ = std::fs::remove_dir_all(&temp_home);
}
