use super::*;

#[tokio::test]
async fn tachi_memory_save_persists_programming_agent_fields() {
    let server = make_server();
    let mem_id = "mcp-agent-fields-001";

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "save".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: Some("fact".to_string()),
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: Some("Refactored MCP save path for coding agents.".to_string()),
            title: None,
            summary: Some("MCP agent fields".to_string()),
            topic: Some("mcp".to_string()),
            keywords: vec!["rust".to_string(), "mcp".to_string()],
            entities: vec!["memory-server".to_string(), "sigil".to_string()],
            importance: Some(0.75),
            retention_policy: None,
            kind: Some("memory".to_string()),
            path: Some("/project/sigil/mcp".to_string()),
            id: Some(mem_id.to_string()),
            force: true,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: Some("rust".to_string()),
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
        },
    )
    .await
    .expect("save should succeed");

    assert!(body.contains("Saved ->"));
    assert!(body.contains(&format!("`{mem_id}`")));

    let db_path = server.global_db_path_buf();
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    let (keywords, entities, domain, path): (String, String, Option<String>, String) = conn
        .query_row(
            "SELECT keywords, entities, domain, path FROM memories WHERE id=?1",
            [mem_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row exists");
    assert_eq!(keywords, r#"["rust","mcp"]"#);
    assert_eq!(entities, r#"["memory-server","sigil"]"#);
    assert_eq!(domain.as_deref(), Some("rust"));
    assert_eq!(path, "/project/sigil/mcp");
}
