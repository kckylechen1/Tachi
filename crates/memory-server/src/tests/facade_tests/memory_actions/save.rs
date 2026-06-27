use super::*;

#[tokio::test]
async fn tachi_memory_checkpoint_saves_agent_checkpoint() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "checkpoint".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: Some("project".to_string()),
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
            text: Some("Implemented status diagnostics; next run cargo test.".to_string()),
            title: Some("Status diagnostics".to_string()),
            summary: Some("Status diagnostics checkpoint".to_string()),
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
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: Some("engineering".to_string()),
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
    .expect("checkpoint should save");

    assert!(body.contains("Saved ->"));
    assert!(body.contains("status: saved"));
    assert!(body.contains("id: `"));
    assert!(body.contains("Summary: Status diagnostics checkpoint"));
}

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

#[tokio::test]
async fn tachi_memory_save_kind_wiki_routes_to_wiki() {
    let server = make_server();
    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.kind = Some("wiki".to_string());
    params.title = Some("Memory facade wiki route".to_string());
    params.text = Some(
        "The memory facade should accept explicit kind=wiki and route to the wiki writer."
            .to_string(),
    );
    params.summary = Some("Memory facade wiki route".to_string());
    params.path = Some("/wiki/agent/tachi/memory-facade-wiki-route".to_string());
    params.category = Some("experience".to_string());
    params.keywords = vec!["facade".to_string(), "wiki".to_string()];
    params.scope = Some("global".to_string());
    params.retention_policy = Some("permanent".to_string());
    params.force = true;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("explicit kind=wiki should route through wiki write");
    let parsed: Value = serde_json::from_str(&body).expect("wiki save JSON");

    assert_eq!(
        parsed["wiki_path"],
        json!("/wiki/agent/tachi/memory-facade-wiki-route")
    );
}

#[tokio::test]
async fn tachi_memory_save_emit_continuity_returns_event() {
    let server = make_server();
    let mem_id = "facade-continuity-save-001";

    let mut save = tachi_memory_params("save");
    save.format = Some("json".to_string());
    save.scope = Some("project".to_string());
    save.text = Some("Facade memory saves can opt into continuity event emission.".to_string());
    save.summary = Some("Facade continuity save".to_string());
    save.category = Some("preference".to_string());
    save.path = Some("/user/patterns/facade-continuity-save".to_string());
    save.id = Some(mem_id.to_string());
    save.force = true;
    save.emit_continuity = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, save)
        .await
        .expect("save should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("save response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert_eq!(
        parsed["continuity_event"]["event_type"],
        json!("memory.saved")
    );
    assert_eq!(
        parsed["continuity_event"]["projection_hints"],
        json!(["pattern"])
    );

    let events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memory_core::TachiEventQuery {
                    event_type: Some("memory.saved".to_string()),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("read continuity events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["memory_id"], json!(mem_id));
}

#[tokio::test]
async fn tachi_memory_get_action_returns_saved_entry() {
    let server = make_server();
    let mem_id = "facade-get-action-001";

    let mut save = tachi_memory_params("save");
    save.format = Some("json".to_string());
    save.scope = Some("project".to_string());
    save.text = Some("Facade get action should return the full saved memory text.".to_string());
    save.summary = Some("Facade get action".to_string());
    save.path = Some("/scratch/tachi/facade-get-action".to_string());
    save.id = Some(mem_id.to_string());
    save.force = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, save)
        .await
        .expect("save should succeed");

    let mut get = tachi_memory_params("get");
    get.format = Some("json".to_string());
    get.id = Some(mem_id.to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, get)
        .await
        .expect("get should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("get response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert_eq!(parsed["path"], json!("/scratch/tachi/facade-get-action"));
    assert!(
        parsed["text"]
            .as_str()
            .is_some_and(|text| text.contains("full saved memory text")),
        "expected full text in get response: {body}"
    );
}
