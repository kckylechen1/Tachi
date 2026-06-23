use super::*;

#[tokio::test]
async fn server_seeds_builtin_capabilities_and_mcp_policies() {
    let server = make_server();

    let trajectory = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:trajectory-distiller")
                .map_err(|e| e.to_string())
        })
        .expect("lookup trajectory builtin");
    let coding = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:coding-architecture-decision")
                .map_err(|e| e.to_string())
        })
        .expect("lookup coding builtin");
    let trading = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:trading-position-snapshot")
                .map_err(|e| e.to_string())
        })
        .expect("lookup trading builtin");
    let mcp = server
        .with_global_store_read(|store| store.hub_get("mcp:web-search").map_err(|e| e.to_string()))
        .expect("lookup mcp builtin");
    let zread = server
        .with_global_store_read(|store| store.hub_get("mcp:zread").map_err(|e| e.to_string()))
        .expect("lookup zread builtin");
    let vision = server
        .with_global_store_read(|store| store.hub_get("mcp:vision").map_err(|e| e.to_string()))
        .expect("lookup vision builtin");
    let superpowers_execute = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:superpowers-executing-plans")
                .map_err(|e| e.to_string())
        })
        .expect("lookup superpowers executing builtin");
    let subagent_driven = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:superpowers-subagent-driven-development")
                .map_err(|e| e.to_string())
        })
        .expect("lookup subagent-driven builtin");
    let verification = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:superpowers-verification-before-completion")
                .map_err(|e| e.to_string())
        })
        .expect("lookup verification builtin");
    let waza_check = server
        .with_global_store_read(|store| {
            store.hub_get("skill:waza-check").map_err(|e| e.to_string())
        })
        .expect("lookup waza check builtin");

    let trajectory = trajectory.expect("trajectory-distiller builtin should exist");
    let coding = coding.expect("coding builtin should exist");
    let trading = trading.expect("trading builtin should exist");
    let mcp = mcp.expect("mcp builtin should exist");
    let zread = zread.expect("zread builtin should exist");
    let vision = vision.expect("vision builtin should exist");
    let superpowers_execute =
        superpowers_execute.expect("superpowers executing builtin should exist");
    let subagent_driven = subagent_driven.expect("subagent-driven builtin should exist");
    let verification = verification.expect("verification builtin should exist");
    let waza_check = waza_check.expect("waza check builtin should exist");

    let trajectory_def: Value =
        serde_json::from_str(&trajectory.definition).expect("trajectory definition json");
    let coding_def: Value =
        serde_json::from_str(&coding.definition).expect("coding definition json");
    let trading_def: Value =
        serde_json::from_str(&trading.definition).expect("trading definition json");
    let mcp_def: Value = serde_json::from_str(&mcp.definition).expect("mcp definition json");
    let zread_def: Value = serde_json::from_str(&zread.definition).expect("zread definition json");
    let vision_def: Value =
        serde_json::from_str(&vision.definition).expect("vision definition json");
    let superpowers_execute_def: Value = serde_json::from_str(&superpowers_execute.definition)
        .expect("superpowers executing definition json");
    let subagent_driven_def: Value =
        serde_json::from_str(&subagent_driven.definition).expect("subagent-driven definition json");
    let verification_def: Value =
        serde_json::from_str(&verification.definition).expect("verification definition json");
    let waza_check_def: Value =
        serde_json::from_str(&waza_check.definition).expect("waza check definition json");

    assert_eq!(trajectory_def["retention_policy"], "permanent");
    assert_eq!(coding_def["retention_policy"], "permanent");
    assert_eq!(trading_def["retention_policy"], "ephemeral");
    assert_eq!(superpowers_execute_def["retention_policy"], "permanent");
    assert_eq!(
        superpowers_execute_def["policy"]["visibility"],
        "discoverable"
    );
    assert!(superpowers_execute_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Executing Plans")));
    assert!(subagent_driven_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Subagent-Driven Development")));
    assert!(verification_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Verification Before Completion")));
    assert_eq!(waza_check_def["retention_policy"], "permanent");
    assert_eq!(waza_check_def["policy"]["visibility"], "discoverable");
    assert!(waza_check_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Review Before You Ship")));
    assert_eq!(mcp_def["auto_ingest"], true);
    assert!(mcp_def.get("auth_header").is_none());
    assert_eq!(mcp_def["auth"]["type"], "bearer");
    assert_eq!(
        mcp_def["auth"]["token"],
        "ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY"
    );
    assert_eq!(
        mcp_def["url"],
        "https://open.bigmodel.cn/api/mcp/web_search_prime/mcp"
    );
    assert_eq!(
        zread_def["url"],
        "https://open.bigmodel.cn/api/mcp/zread/mcp"
    );
    assert_eq!(vision_def["transport"], "stdio");
    assert_eq!(vision_def["command"], "npx");
    assert_eq!(vision_def["args"][0], "-y");
    assert_eq!(vision_def["args"][1], "@z_ai/mcp-server@latest");
    assert_eq!(
        vision_def["env"]["Z_AI_API_KEY"],
        "${vault:ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY}"
    );
    assert_eq!(vision_def["env"]["Z_AI_MODE"], "ZAI");

    let policy = server
        .with_global_store_read(|store| {
            store
                .get_sandbox_policy("mcp:web-search")
                .map_err(|e| e.to_string())
        })
        .expect("lookup builtin sandbox policy");
    assert!(policy.is_some(), "builtin MCP should seed sandbox policy");
}

#[tokio::test]
async fn distill_trajectory_creates_permanent_snapshot_and_skill() {
    let server = make_server();

    server
        .with_global_store(|store| {
            let mut cap = store
                .hub_get("skill:trajectory-distiller")
                .map_err(|e| e.to_string())?
                .expect("trajectory distiller should exist");
            let mut def: Value =
                serde_json::from_str(&cap.definition).map_err(|e| e.to_string())?;
            def["mock_response"] = json!(
                "# 适用场景\n- recurring task\n\n# 核心步骤\n- step\n\n# 踩坑记录\n- none\n\n# 验证标准\n- tests pass\n\n# 适用域标签\n- coding"
            );
            cap.definition = serde_json::to_string(&def).map_err(|e| e.to_string())?;
            store.hub_register(&cap).map_err(|e| e.to_string())
        })
        .expect("inject mock response");

    let response = server
        .distill_trajectory(Parameters(DistillTrajectoryParams {
            task_description: "Fix a flaky test".to_string(),
            execution_trace: vec![json!({"step":"reproduced"}), json!({"step":"fixed"})],
            final_outcome: json!({"success": true, "score": 0.92}),
            agent_id: "codex".to_string(),
            skill_path: "/skills/coding/flaky-test-fix".to_string(),
            skill_id: Some("skill:flaky-test-fix".to_string()),
            importance: Some(0.9),
            domain: Some("coding".to_string()),
            project: None,
            scope: "global".to_string(),
        }))
        .await
        .expect("distill_trajectory should succeed");
    let response_json: Value = serde_json::from_str(&response).expect("distill response json");

    let snapshot_id = response_json["snapshot_id"]
        .as_str()
        .expect("snapshot id")
        .to_string();
    let snapshot = server
        .with_global_store_read(|store| store.get(&snapshot_id).map_err(|e| e.to_string()))
        .expect("load distilled snapshot")
        .expect("snapshot should exist");
    assert_eq!(snapshot.retention_policy.as_deref(), Some("permanent"));

    let distilled_cap = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:flaky-test-fix")
                .map_err(|e| e.to_string())
        })
        .expect("load distilled cap")
        .expect("distilled skill should exist");
    let distilled_def: Value =
        serde_json::from_str(&distilled_cap.definition).expect("distilled definition json");
    assert_eq!(distilled_def["retention_policy"], "permanent");
    assert_eq!(distilled_cap.avg_rating, 0.5);
}

#[tokio::test]
async fn ingest_source_chunks_content_and_builds_graph_edges() {
    let server = make_server();

    server
        .with_global_store(|store| {
            let entry = MemoryEntry {
                id: "existing-edge-target".to_string(),
                path: "/wiki/coding/reference".to_string(),
                summary: "cargo workspace chunking".to_string(),
                text: "cargo workspace chunking graph edge reference".to_string(),
                importance: 0.8,
                timestamp: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_until: None,
                category: "fact".to_string(),
                topic: "reference".to_string(),
                keywords: vec![],
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
                retention_policy: None,
                domain: Some("coding".to_string()),
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            };
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed comparable memory");

    let response = server
        .ingest_source(Parameters(IngestSourceParams {
            content:
                "cargo workspace chunking graph edge reference\nsecond paragraph for another chunk"
                    .to_string(),
            source_url: Some("https://example.com/docs".to_string()),
            source: Some("docs".to_string()),
            path_prefix: Some("/wiki/coding/test-ingest".to_string()),
            auto_chunk: true,
            auto_summarize: false,
            auto_link: true,
            importance: 0.75,
            scope: "global".to_string(),
            project: None,
            domain: Some("coding".to_string()),
            chunk_size_chars: 32,
            chunk_overlap_chars: 0,
            metadata: None,
        }))
        .await
        .expect("ingest_source should succeed");
    let response_json: Value =
        serde_json::from_str(&response).expect("ingest_source response json");
    let saved = response_json["chunks_saved"].as_u64().unwrap_or(0);
    assert!(saved >= 2, "expected chunked ingest, got {response_json}");

    let ids: Vec<String> = serde_json::from_value(response_json["ids"].clone()).expect("ids");
    let edges = server
        .with_global_store_read(|store| {
            store
                .get_edges(&ids[0], "outgoing", Some("similar_to"))
                .map_err(|e| e.to_string())
        })
        .expect("load related edges");
    assert!(
        edges
            .iter()
            .any(|edge| edge.target_id == "existing-edge-target"),
        "expected auto-linked edge to seeded reference"
    );
}

#[tokio::test]
async fn auto_ingest_hook_persists_mcp_text_results() {
    let server = make_server();
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "reader output for auto ingest"}],
        "isError": false
    }))
    .expect("build tool result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": "/wiki/general/auto-ingest-test"
    });
    let arguments =
        serde_json::Map::from_iter([("url".to_string(), json!("https://example.com/article"))]);

    crate::pipeline_ops::schedule_auto_ingest_from_mcp(
        &server,
        "mcp:web-reader",
        "webReader",
        &definition,
        Some(&arguments),
        &result,
    );

    tokio::time::sleep(Duration::from_millis(50)).await;

    let entries = server
        .with_global_store_read(|store| {
            store
                .list_by_path("/wiki/general/auto-ingest-test", 10, false)
                .map_err(|e| e.to_string())
        })
        .expect("load auto-ingested entries");
    assert!(
        !entries.is_empty(),
        "auto_ingest hook should persist MCP text results"
    );
}

#[tokio::test]
async fn ingest_source_empty_content_records_skip_audit() {
    let server = make_server();

    let response = server
        .ingest_source(Parameters(IngestSourceParams {
            content: "   ".to_string(),
            source_url: Some("https://example.com/empty".to_string()),
            source: Some("empty-source".to_string()),
            path_prefix: Some("/wiki/general/empty".to_string()),
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            importance: 0.7,
            scope: "global".to_string(),
            project: None,
            domain: Some("general".to_string()),
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            metadata: None,
        }))
        .await
        .expect("empty ingest_source should return skipped response");

    let json: Value = serde_json::from_str(&response).expect("json");
    assert_eq!(json["status"], "skipped");

    let audits = server
        .with_global_store_read(|store| {
            store
                .audit_log_list(20, Some("ingest"))
                .map_err(|e| e.to_string())
        })
        .expect("audit list");
    assert!(audits.iter().any(|entry| {
        entry["tool_name"] == "ingest_source" && entry["error_kind"] == "empty_source_content"
    }));
}
