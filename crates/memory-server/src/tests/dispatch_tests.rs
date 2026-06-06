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
            dispatch_id: None,
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("tachi_complete should succeed");

    let bundle: serde_json::Value = serde_json::from_str(&resp).expect("bundle JSON");
    assert_eq!(bundle["recorded"], serde_json::json!(true));
    assert_eq!(bundle["task_id"], serde_json::json!("smoke-test-001"));
    assert_eq!(bundle["outcome"], serde_json::json!("success"));
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
async fn dispatch_prompt_includes_task_route_overlay() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: "codex".to_string(),
            task: "帮我编译二进制并且跑起来验证功能".to_string(),
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
            project: None,
            stage: None,
        },
    )
    .await;

    assert!(prompt.contains("## Tachi task route"), "{prompt}");
    assert!(prompt.contains("intent: test_request"), "{prompt}");
    assert!(prompt.contains("skill:coding-test-strategy"), "{prompt}");
    assert!(prompt.contains("## Required skill invocation"), "{prompt}");
    assert!(prompt.contains("tachi_progress_check(check)"), "{prompt}");
}

#[tokio::test]
async fn dispatch_prompt_invokes_stage_and_waza_skills_for_execute_slice() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: "claude".to_string(),
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
            project: None,
            stage: Some("execute:runtime".to_string()),
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
async fn dispatch_prompt_invokes_native_subagent_factory_for_dispatch_stage() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: "codex".to_string(),
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
            project: None,
            stage: Some("dispatch".to_string()),
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
        &TachiDispatchParams {
            agent: "codex".to_string(),
            task: "Fix dispatch routing regression".to_string(),
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
            project: None,
            stage: None,
        },
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
            agent: "gemini".to_string(),
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
            project: None,
            stage: None,
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
    let params = TachiDispatchParams {
        agent: "custom".to_string(),
        task: "should fail before subprocess".to_string(),
        cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
        permission_profile: None,
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: Some(true),
        inject_hub_mcps: None,
        command: vec!["true".to_string()],
        project: None,
        stage: None,
    };

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("custom backend must reject MCP injection");
    assert!(
        err.contains("custom backend"),
        "unexpected custom injection error: {err}"
    );
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

    let server = make_server();

    // Isolated TACHI_HOME so run files don't pollute real one.
    let temp_home = std::env::temp_dir().join(format!("tachi-v2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_home).expect("create temp tachi home");

    // Fake claude binary: prints a JSON envelope matching the pool's
    // expected `{"result": "..."}` shape, embedding a valid plan.
    let fake_claude = temp_home.join("fake-claude.sh");
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

    // We can't easily call the private handle_tachi_dispatch from
    // outside the crate, but tests live inside the crate so the
    // `pub(crate)` visibility is accessible via crate path.
    let params = TachiDispatchParams {
        agent: "custom".to_string(),
        task: "smoke v2".to_string(),
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
        // Execute stage uses a no-op command so the test doesn't need
        // a working claude/codex CLI for Stage 2.
        command: vec!["true".to_string()],
        project: None,
        stage: None,
    };

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

    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(trajectory.contains("\"event\":\"dispatch_started\""));
    assert!(trajectory.contains("\"event\":\"plan_generated\""));
    assert!(trajectory.contains("\"event\":\"execute_started\""));
    let progress =
        std::fs::read_to_string(run_dir.join("progress.jsonl")).expect("progress present");
    assert!(progress.contains("\"event\":\"dispatch_started\""));
    assert!(progress.contains("\"event\":\"plan_generated\""));

    // Wait briefly for the spawned stage-2 task to write status.json /
    // result.md / dispatch_finished trajectory line.
    for _ in 0..30 {
        if run_dir.join("status.json").exists() && run_dir.join("result.md").exists() {
            break;
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
