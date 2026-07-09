use super::*;

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
