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

#[tokio::test]
async fn tachi_memory_ask_returns_evidence_contract() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "ask".to_string(),
            format: Some("markdown".to_string()),
            query: Some("what did we implement".to_string()),
            scope: None,
            top_k: 3,
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
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("ask should succeed");

    assert!(body.starts_with("## Tachi ask"));
    assert!(body.contains("status: completed"));
    assert!(body.contains("evidence:"));
    // evidence count may be 0 in CI without embedding API; verify format only
    assert!(body.contains("hit(s)"));
}

#[tokio::test]
async fn tachi_memory_ask_can_return_compact_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "ask",
        "format": "json",
        "query": "what did we implement",
        "top_k": 3
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("ask json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("what did we implement"));
    assert!(parsed["evidence"].is_array());
    assert!(parsed["thinking"].is_object());
}

#[tokio::test]
async fn tachi_memory_ask_keeps_controlled_probe_evidence_aligned_with_search() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut alpha = make_entry("ask-parity-alpha");
            alpha.path = "/scratch/tachi/ask-parity-alpha".to_string();
            alpha.summary = "Ask parity alpha".to_string();
            alpha.text =
                "RECALL_PROBE_ALPHA_ASK_20260607 clean-cli bridge dry-run force-delete behavior"
                    .to_string();
            alpha.keywords = vec!["recall-probe".to_string(), "clean-cli".to_string()];
            store.upsert(&alpha).map_err(|e| e.to_string())?;

            for idx in 0..12 {
                let mut distractor = make_entry(&format!("ask-parity-distractor-{idx}"));
                distractor.path = format!("/scratch/tachi/ask-parity-distractor-{idx}");
                distractor.summary = format!("Ask parity distractor {idx}");
                distractor.text =
                    format!("RECALL_PROBE_BETA_ASK_20260607 clean-cli bridge candidate {idx}");
                distractor.keywords = vec!["recall-probe".to_string(), "clean-cli".to_string()];
                store.upsert(&distractor).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed ask/search parity entries");

    let mut search_params = tachi_memory_params("search");
    search_params.format = Some("json".to_string());
    search_params.query = Some("RECALL_PROBE_ALPHA_ASK_20260607".to_string());
    search_params.scope = Some("memory".to_string());
    search_params.top_k = 3;
    let search_body = crate::facade_memory_ops::handle_tachi_memory(&server, search_params)
        .await
        .expect("search should succeed");
    let search_json: Value = serde_json::from_str(&search_body).expect("search JSON");
    let search_ids = search_json["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .flat_map(|section| {
            section["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["id"].as_str())
        })
        .collect::<Vec<_>>();

    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some("RECALL_PROBE_ALPHA_ASK_20260607".to_string());
    ask_params.scope = Some("memory".to_string());
    ask_params.top_k = 3;
    ask_params.enable_rerank = false;
    let ask_body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed");
    let ask_json: Value = serde_json::from_str(&ask_body).expect("ask JSON");
    let evidence = ask_json["evidence"].as_array().expect("ask evidence");
    let ask_ids = evidence
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(search_ids.first().copied(), Some("ask-parity-alpha"));
    assert_eq!(ask_ids.first().copied(), Some("ask-parity-alpha"));
    assert!(
        ask_ids
            .iter()
            .take(3)
            .any(|id| search_ids.iter().take(3).any(|search_id| search_id == id)),
        "ask evidence should overlap controlled search evidence: search={search_ids:?} ask={ask_ids:?}"
    );
    assert!(
        evidence
            .first()
            .is_some_and(|row| row.get("rerank_policy").is_none()),
        "ask should not force rerank when enable_rerank=false: {ask_json}"
    );
}

#[tokio::test]
async fn tachi_memory_readiness_can_return_operational_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "readiness",
        "format": "json"
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("readiness json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("readiness response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert!(parsed["runtime"].is_object());
    assert!(parsed["health"].is_object());
    assert!(parsed["tools"].is_array());
    assert!(parsed["tool_visibility_summary"].is_object());
    assert!(parsed["suggestions"].is_array());
    assert!(parsed["vector_health"].is_object());
    assert!(parsed["readiness_warnings"].is_array());

    let tools = server.tachi_tools().await.expect("tachi_tools");
    let tools_count = tools
        .lines()
        .find_map(|line| line.strip_prefix("count: "))
        .expect("tachi_tools count line")
        .parse::<u64>()
        .expect("numeric tachi_tools count");
    assert_eq!(
        parsed["tool_visibility_summary"]["visible_count"],
        json!(tools_count),
        "readiness visible_count should match tachi_tools output"
    );
}

#[tokio::test]
async fn tachi_memory_alerts_and_compact_briefing_report_same_wiki_counts() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut entry = make_entry("briefing-alerts-wiki-orphan");
            entry.path = "/wiki/test/briefing-alerts-orphan".to_string();
            entry.summary = "Briefing alerts wiki orphan".to_string();
            entry.text =
                "BriefingAlertsWikiCountNeedle should be counted by wiki hygiene.".to_string();
            entry.domain = Some("wiki".to_string());
            entry.metadata = json!({"wiki": true});
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed wiki hygiene row");

    let mut briefing_params = tachi_memory_params("briefing");
    briefing_params.format = Some("json".to_string());
    briefing_params.query = Some("BriefingAlertsWikiCountNeedle".to_string());
    briefing_params.compact = true;
    let briefing_body = crate::facade_memory_ops::handle_tachi_memory(&server, briefing_params)
        .await
        .expect("briefing should succeed");
    let briefing_json: Value = serde_json::from_str(&briefing_body).expect("briefing JSON");

    let mut alerts_params = tachi_memory_params("alerts");
    alerts_params.format = Some("json".to_string());
    let alerts_body = crate::facade_memory_ops::handle_tachi_memory(&server, alerts_params)
        .await
        .expect("alerts should succeed");
    let alerts_json: Value = serde_json::from_str(&alerts_body).expect("alerts JSON");

    assert_eq!(
        briefing_json["health"]["wiki"], alerts_json["wiki_counts"],
        "alerts and compact briefing should report the same wiki hygiene counts"
    );
    assert!(
        alerts_json["wiki_counts"]["orphans"].as_u64().unwrap_or(0) >= 1,
        "fixture should produce a visible orphan count: {alerts_json}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_memory_progress_writes_append_only_jsonl() {
    let (server, temp_home) = make_server_with_temp_home();
    let run_root = temp_home.temp_home.join("runs");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "progress".to_string(),
            format: Some("markdown".to_string()),
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
            text: Some("step completed with api_key=test-secret-value-1234567890".to_string()),
            title: Some("Progress step".to_string()),
            summary: Some("one step done".to_string()),
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
            flow_id: Some("flow_progress_test".to_string()),
            event: Some("validation".to_string()),
            state: Some("running".to_string()),
            project: None,
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("progress should record");

    assert!(body.starts_with("## Tachi progress"));
    assert!(body.contains("status: recorded"));
    assert!(body.contains("secret_redactions: 1"));
    let log = std::fs::read_to_string(run_root.join("flow_progress_test/progress.jsonl"))
        .expect("progress jsonl");
    assert!(log.contains("validation"));
    assert!(!log.contains("test-secret-value-1234567890"));
    if let Some(original) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", original);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
