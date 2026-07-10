use super::*;

#[tokio::test]
async fn save_memory_includes_provenance_for_registered_agent() {
    let server = make_server();

    {
        let mut guard = server.agent_runtime_write();
        guard.agent_profile = Some(AgentProfile {
            agent_id: "claude-code".to_string(),
            display_name: "Claude Code".to_string(),
            capabilities: vec!["code-gen".to_string()],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: None,
            registered_at: Utc::now().to_rfc3339(),
        });
    }

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Investigated the failing OAuth callback edge case.".to_string(),
            summary: "OAuth callback investigation".to_string(),
            path: "/project/auth".to_string(),
            importance: 0.8,
            category: "fact".to_string(),
            topic: "auth".to_string(),
            keywords: vec!["oauth".to_string(), "callback".to_string()],
            persons: vec![],
            entities: vec!["oauth".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"]
        .as_str()
        .expect("save_memory should return id")
        .to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    let provenance = &fetched_json["metadata"]["provenance"];

    assert_eq!(provenance["tool_name"], json!("save_memory"));
    assert_eq!(provenance["source_kind"], json!("memory_write"));
    assert_eq!(provenance["requested_scope"], json!("project"));
    assert_eq!(provenance["db_scope"], json!("global"));
    assert_eq!(provenance["agent"]["agent_id"], json!("claude-code"));
}
