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
            duration_ms: Some(5420),
            skills_used: vec!["skill:superpowers".to_string()],
            cost_tokens: Some(1234),
            cost_usd: Some(0.0812),
            quality_score: Some(0.9),
            notes: Some("All tests green.".to_string()),
            trajectory: None,
            diff: Some("diff --git a/foo b/foo\n+bar\n".to_string()),
            worktree: None,
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
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: serde_json::Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    assert_eq!(fetched["category"], serde_json::json!("experience"));
    let keywords = fetched["keywords"].as_array().expect("keywords array");
    assert!(keywords.iter().any(|k| k == "eval"));
    let metadata = &fetched["metadata"];
    assert_eq!(metadata["agent"], serde_json::json!("claude-code"));
    assert_eq!(metadata["outcome"], serde_json::json!("success"));
    assert_eq!(metadata["cost_tokens"], serde_json::json!(1234));
    assert_eq!(
        metadata["skills_used"][0],
        serde_json::json!("skill:superpowers")
    );
    assert!(metadata["diff"].as_str().unwrap().contains("+bar"));
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
