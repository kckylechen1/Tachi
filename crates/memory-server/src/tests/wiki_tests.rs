use super::*;

#[tokio::test]
async fn tachi_wiki_write_allows_wiki_bucket_without_capture_gate_warning() {
    let server = make_server();

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "MCP wiki exposure smoke".to_string(),
            text: "When exposing wiki tools through MCP, keep the lesson under /wiki, attach the correct domain, and write enough concrete detail that the capture gate can stay strict without flagging a valid debugging note. This regression test proves /wiki is a first-class capture bucket now.".to_string(),
            path: None,
            topic: Some("mcp-tool-exposure".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["mcp".to_string(), "wiki".to_string()],
            entities: vec![],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: Some("coding".to_string()),
            project: None,
            force: false,
        }))
        .await
        .expect("tachi_wiki_write should succeed");

    let json: Value = serde_json::from_str(&response).expect("wiki write response json");
    assert_eq!(json["wiki_path"], json!("/wiki/general/mcp-tool-exposure"));
    assert!(
        json.get("capture_gate_warnings").is_none(),
        "expected /wiki writes to avoid capture gate warnings, got: {json}"
    );
}

#[tokio::test]
async fn tachi_wiki_write_generates_readable_cjk_path_without_domain_warning() {
    let server = make_server();

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "MCP hub_call arguments 丢失：从 schema 层排查".to_string(),
            text: "# MCP hub_call arguments 丢失\n\n## 结论\n\n- 先确认 schema 层是否声明 arguments。\n- 再检查 client serialization 是否丢字段。\n- 最后才看 transport。\n\n这是一条结构化 wiki 经验，正文使用 Markdown 是预期行为，不应该触发 raw markdown dump warning。".to_string(),
            path: None,
            topic: None,
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["mcp".to_string(), "hub_call".to_string()],
            entities: vec![],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            force: false,
        }))
        .await
        .expect("tachi_wiki_write should succeed without explicit domain");

    let json: Value = serde_json::from_str(&response).expect("wiki write response json");
    assert_eq!(
        json["wiki_path"],
        json!("/wiki/general/MCP-hub_call-arguments-丢失-从-schema-层排查")
    );
    assert!(
        json.get("capture_gate_warnings").is_none(),
        "expected wiki write defaults to suppress domain/markdown warnings, got: {json}"
    );
}

#[tokio::test]
async fn tachi_wiki_write_updates_existing_path_in_place() {
    let (server, _home) = seed_wiki_project_entries(Vec::new());

    let first = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "TrendLock Rule".to_string(),
            text: "TrendLock should protect a live trend from premature exits when the validation rule still holds.".to_string(),
            path: Some("/wiki/agent/tachi/trendlock".to_string()),
            topic: Some("trendlock".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["trendlock".to_string()],
            entities: vec!["TrendLock".to_string()],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            force: true,
        }))
        .await
        .expect("first wiki write");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    let first_id = first_json["id"].as_str().expect("first id").to_string();

    let second = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "TrendLock Rule".to_string(),
            text: "TrendLock should protect a live trend until the trend invalidation rule actually breaks.".to_string(),
            path: Some("/wiki/agent/tachi/trendlock".to_string()),
            topic: Some("trendlock".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["trendlock".to_string()],
            entities: vec!["TrendLock".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            force: true,
        }))
        .await
        .expect("second wiki write");
    let second_json: Value = serde_json::from_str(&second).expect("second json");

    assert_eq!(second_json["id"], json!(first_id));
    assert_eq!(second_json["wiki_write_mode"], json!("updated"));

    let active_count: i64 = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE path = '/wiki/agent/tachi/trendlock' AND archived = 0 AND superseded_by IS NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("count active wiki rows");
    assert_eq!(active_count, 1);
}

#[tokio::test]
async fn tachi_wiki_write_supersedes_duplicate_topic_rows() {
    let mut canonical = make_entry("canonical-trendlock-row");
    canonical.path = "/wiki/agent/tachi/trendlock".to_string();
    canonical.topic = "trendlock".to_string();
    canonical.text = "TrendLock canonical base rule.".to_string();
    canonical.summary = "TrendLock".to_string();
    canonical.metadata = json!({"wiki": true});
    canonical.domain = Some("wiki".to_string());

    let mut duplicate = make_entry("duplicate-trendlock-row");
    duplicate.path = "/wiki/agent/tachi/trendlock-copy".to_string();
    duplicate.topic = "trendlock-copy".to_string();
    duplicate.text =
        "TrendLock canonical rule should replace older same-topic wiki entries.".to_string();
    duplicate.summary = "Duplicate TrendLock".to_string();
    duplicate.metadata = json!({"wiki": true});
    duplicate.domain = Some("wiki".to_string());

    let (server, _home) = seed_wiki_project_entries(vec![canonical, duplicate]);

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "TrendLock Canonical".to_string(),
            text: "TrendLock canonical rule should replace older same-topic wiki entries."
                .to_string(),
            path: Some("/wiki/agent/tachi/trendlock".to_string()),
            topic: Some("trendlock".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["trendlock".to_string()],
            entities: vec!["TrendLock".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            force: true,
        }))
        .await
        .expect("wiki write should supersede duplicate");
    let json: Value = serde_json::from_str(&response).expect("write json");
    assert_eq!(json["id"], json!("canonical-trendlock-row"));
    assert_eq!(json["wiki_duplicates_superseded"], json!(1));
    let superseded_by: Option<String> = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = 'duplicate-trendlock-row'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("read superseded_by");
    assert_eq!(superseded_by, Some("canonical-trendlock-row".to_string()));
    assert_eq!(json["wiki_write_mode"], json!("updated"));
}

#[tokio::test]
async fn wiki_browse_includes_related_entries_and_logs_operation() {
    let mut alpha = make_entry("wiki-related-alpha");
    alpha.path = "/wiki/engineering/debugging/alpha".to_string();
    alpha.summary = "Alpha debugging".to_string();
    alpha.text = "Alpha debugging lesson for MCP".to_string();
    alpha.entities = vec!["MCP".to_string()];
    alpha.importance = 0.7;

    let mut beta = make_entry("wiki-related-beta");
    beta.path = "/wiki/engineering/debugging/beta".to_string();
    beta.summary = "Beta debugging".to_string();
    beta.text = "Beta debugging lesson for MCP".to_string();
    beta.entities = vec!["MCP".to_string()];
    beta.importance = 0.9;

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .wiki_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/debugging".to_string()),
            limit: 10,
            project: "wiki".to_string(),
        }))
        .await
        .expect("wiki browse should succeed");
    assert!(
        response.contains("/wiki/engineering/debugging/alpha"),
        "browse markdown should contain alpha path"
    );
    assert!(
        response.contains("/wiki/engineering/debugging/beta"),
        "browse markdown should contain beta path"
    );

    let log = server
        .with_named_project_store_read("wiki", |store| {
            store.get("wiki-operation-log").map_err(|e| e.to_string())
        })
        .expect("read wiki log")
        .expect("wiki log should exist");
    assert!(log.text.contains("browse | /wiki/engineering/debugging"));
}

#[tokio::test]
async fn wiki_search_returns_compact_hits_without_related_entries() {
    let mut alpha = make_entry("wiki-search-alpha");
    alpha.path = "/wiki/engineering/debugging/search-alpha".to_string();
    alpha.summary = "MCP schema debugging".to_string();
    alpha.text = "MCP schema debugging requires checking serialization.".to_string();
    alpha.entities = vec!["MCP".to_string()];

    let mut beta = make_entry("wiki-search-beta");
    beta.path = "/wiki/engineering/debugging/search-beta".to_string();
    beta.summary = "MCP transport debugging".to_string();
    beta.text = "MCP transport debugging should come after schema checks.".to_string();
    beta.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .wiki_search(Parameters(WikiSearchParams {
            query: "MCP debugging".to_string(),
            path_prefix: Some("/wiki".to_string()),
            category: None,
            top_k: 5,
            include_archived: false,
            agent_role: None,
            project: Some("wiki".to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            weights: None,
        }))
        .await
        .expect("wiki search should succeed");
    assert!(response.starts_with("## Wiki search:"));
    assert!(
        response.contains("MCP schema debugging") || response.contains("MCP transport debugging")
    );
    assert!(!response.contains("merge_hints"));
}

#[tokio::test]
async fn tachi_search_wiki_scope_honors_explicit_project() {
    let mut entry = make_entry("wiki-default-project-search");
    entry.path = "/wiki/engineering/search-default".to_string();
    entry.summary = "Default wiki project search".to_string();
    entry.text = "UniqueDefaultWikiNeedle should be found in the named wiki project.".to_string();
    entry.entities = vec!["UniqueDefaultWikiNeedle".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueDefaultWikiNeedle".to_string(),
            scope: "wiki".to_string(),
            top_k: 5,
            path_prefix: None,
            project: Some("wiki".to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("tachi_search wiki scope should succeed");

    assert!(
        response.contains("UniqueDefaultWikiNeedle") || response.contains("search-default"),
        "expected explicit project=wiki to search the named wiki DB, got: {response}"
    );
}

#[tokio::test]
async fn tachi_save_title_with_wiki_path_routes_to_wiki() {
    let server = make_server();

    let response = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "A routed wiki entry should be stored as wiki when title and /wiki path are both present.".to_string(),
            id: None,
            kind: None,
            title: Some("Routing Boundary Wiki".to_string()),
            summary: Some("Routing boundary wiki".to_string()),
            path: Some("/wiki/agent/tachi/routing-boundary".to_string()),
            importance: Some(0.85),
            category: Some("experience".to_string()),
            keywords: vec!["routing".to_string()],
            entities: Vec::new(),
            scope: Some("global".to_string()),
            project: None,
            domain: None,
            retention_policy: Some("permanent".to_string()),
            force: true,
            topic: Some("routing-boundary".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("tachi_save wiki route should succeed");
    let json: serde_json::Value = serde_json::from_str(&response).expect("save JSON");
    assert_eq!(
        json["wiki_path"],
        json!("/wiki/agent/tachi/routing-boundary")
    );

    let id = json["id"].as_str().expect("wiki id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(fetched_json["metadata"]["wiki"], json!(true));
    assert_eq!(
        fetched_json["metadata"]["wiki_title"],
        json!("Routing Boundary Wiki")
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes HOME/TACHI_HOME across async mock LLM + REM run
async fn rem_wiki_evolver_writes_pending_drafts_to_wiki_project() {
    let _guard = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());

    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(_body): Json<serde_json::Value>| async {
            Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "{\n  \"title\": \"Recall Gate Pattern\",\n  \"body\": \"## Pattern\\nUse recall diversity before promoting raw notes into durable knowledge.\\n\\n## Gotcha\\nDo not activate drafts without review.\",\n  \"summary\": \"Recall diversity gates promotion.\",\n  \"keywords\": [\"recall\", \"promotion\"],\n  \"entities\": [\"Tachi\"],\n  \"domain\": \"memory\"\n}"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    let original_siliconflow_base = std::env::var_os("SILICONFLOW_BASE_URL");
    let original_reasoning_base = std::env::var_os("REASONING_BASE_URL");
    let original_siliconflow_key = std::env::var_os("SILICONFLOW_API_KEY");
    let original_voyage_key = std::env::var_os("VOYAGE_API_KEY");
    let temp_home = std::env::temp_dir().join(format!("tachi-rem-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp_home.join(".tachi/projects/wiki")).expect("create wiki project");
    std::env::set_var("HOME", &temp_home);
    std::env::set_var("TACHI_HOME", temp_home.join(".tachi"));
    let mock_url = format!("http://127.0.0.1:{port}/chat/completions");
    std::env::set_var("SILICONFLOW_BASE_URL", &mock_url);
    std::env::set_var("REASONING_BASE_URL", &mock_url);
    std::env::set_var("SILICONFLOW_API_KEY", "test-mock-key");
    std::env::set_var("VOYAGE_API_KEY", "test-mock-key");

    let wiki_db = temp_home.join(".tachi/projects/wiki/memory.db");
    MemoryStore::open(wiki_db.to_str().expect("wiki db utf8")).expect("init wiki db");
    let server = MemoryServer::new(
        temp_home.join("global.db"),
        Some(temp_home.join("project.db")),
    )
    .expect("server");
    server.with_project_store(|store| {
        for (id, summary, text) in [
            (
                "pattern-a",
                "Recall diversity gate",
                "Recall diversity should gate raw promotion before durable wiki synthesis. This fixture covers the first retrieval signal and promotion rule.",
            ),
            (
                "pattern-b",
                "Pending review draft gate",
                "Weekly REM synthesis should write drafts as pending review wiki notes. This fixture covers draft routing and review metadata safety.",
            ),
        ] {
            let mut entry = make_entry(id);
            entry.path = format!("/project/tachi/{id}");
            entry.summary = summary.to_string();
            entry.text = text.to_string();
            entry.importance = 0.9;
            entry.topic = "recall-gate".to_string();
            entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
            entry.source = "manual".to_string();
            entry.tier = "pattern".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())?;
        }
        Ok(())
    }).expect("seed patterns");
    let seeded_count: i64 = server
        .with_project_store_read(|store| {
            store.connection().query_row(
            "SELECT COUNT(*) FROM memories WHERE tier = 'pattern' AND topic = 'recall-gate'",
            [],
            |row| row.get(0),
        ).map_err(|e| e.to_string())
        })
        .expect("count seeded patterns");
    assert_eq!(
        seeded_count, 2,
        "expected two pattern memories before REM run"
    );

    let report = crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server)
        .await
        .expect("wiki evolution");
    assert_eq!(report.drafts_written, 1);
    let review_status = server.with_named_project_store_read("wiki", |store| {
        store.connection().query_row(
            "SELECT json_extract(metadata, '$.review_status') FROM memories WHERE path LIKE '/wiki/drafts/%' LIMIT 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        ).map_err(|e| e.to_string())
    }).expect("read wiki draft metadata");
    assert_eq!(review_status.as_deref(), Some("pending"));

    server_task.abort();
    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
    if let Some(value) = original_siliconflow_base {
        std::env::set_var("SILICONFLOW_BASE_URL", value);
    } else {
        std::env::remove_var("SILICONFLOW_BASE_URL");
    }
    if let Some(value) = original_reasoning_base {
        std::env::set_var("REASONING_BASE_URL", value);
    } else {
        std::env::remove_var("REASONING_BASE_URL");
    }
    if let Some(value) = original_siliconflow_key {
        std::env::set_var("SILICONFLOW_API_KEY", value);
    } else {
        std::env::remove_var("SILICONFLOW_API_KEY");
    }
    if let Some(value) = original_voyage_key {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
    let _ = std::fs::remove_dir_all(temp_home);
}

#[tokio::test]
async fn wiki_export_obsidian_writes_markdown_index_and_wikilinks() {
    let mut entry = make_entry("wiki-export-entry");
    entry.path = "/wiki/engineering/debugging/export".to_string();
    entry.summary = "Export MCP lesson".to_string();
    entry.text =
        "MCP export lesson references MCP explicitly; [[MCP]] stays linked; MCPing stays plain."
            .to_string();
    entry.topic = "export-mcp".to_string();
    entry.keywords = vec!["debugging".to_string()];
    entry.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = std::env::temp_dir().join(format!("wiki-export-{}", uuid::Uuid::new_v4()));

    let result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("wiki export should succeed");
    assert_eq!(result["count"], json!(1));
    let md_path = out_dir.join("engineering/debugging/export/export-mcp.md");
    let markdown = std::fs::read_to_string(&md_path).expect("read exported markdown");
    assert!(markdown.contains("tags: [\"debugging\"]"));
    assert!(markdown.contains(
        "[[MCP]] export lesson references [[MCP]] explicitly; [[MCP]] stays linked; MCPing stays plain."
    ));
    let index = std::fs::read_to_string(out_dir.join("_index.md")).expect("read index");
    assert!(index.contains("[[export-mcp]]"));
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn tachi_wiki_ingest_creates_entry_and_related_edge() {
    let mut existing = make_entry("wiki-ingest-existing");
    existing.path = "/wiki/general/existing".to_string();
    existing.summary = "Existing ingest topic".to_string();
    existing.text = "Existing entry for IngestTopic.".to_string();
    existing.entities = vec!["IngestTopic".to_string()];

    let (server, home) = seed_wiki_project_entries(vec![existing]);
    let source_path = home.temp_home.join(".tachi/ingest-source.md");
    std::fs::write(
        &source_path,
        "# Ingest source\nIngestTopic appears in this source.",
    )
    .expect("write ingest source");

    let response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: source_path.to_string_lossy().to_string(),
            topic: Some("IngestTopic".to_string()),
            update_related: true,
        }))
        .await
        .expect("wiki ingest should succeed");
    let json: Value = serde_json::from_str(&response).expect("wiki ingest json");
    let created_id = json["id"].as_str().expect("created id");
    assert_eq!(json["status"], json!("created"));
    assert!(json["related_entries"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["id"] == "wiki-ingest-existing")
    }));

    let edges = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get_edges(created_id, "outgoing", Some("references"))
                .map_err(|e| e.to_string())
        })
        .expect("read ingest edges");
    assert!(edges
        .iter()
        .any(|edge| edge.target_id == "wiki-ingest-existing"));
}

#[tokio::test]
async fn wiki_lint_reports_memory_health_and_skill_quality_guards() {
    let server = make_server();
    let old_ts = (Utc::now() - chrono::Duration::days(120)).to_rfc3339();

    server
        .with_global_store(|store| {
            let entries = vec![
                MemoryEntry {
                    id: "wiki-orphan".to_string(),
                    path: "/wiki/test/orphan".to_string(),
                    summary: "orphan".to_string(),
                    text: "Standalone old note".to_string(),
                    importance: 0.4,
                    timestamp: old_ts.clone(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "orphan".to_string(),
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
                    // Explicit `durable` opts out of the `/wiki*` → permanent
                    // default retention applied by `normalize_for_write`, so
                    // the stale check still flags this fixture.
                    retention_policy: Some("durable".to_string()),
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-always".to_string(),
                    path: "/wiki/test/policy-a".to_string(),
                    summary: "policy a".to_string(),
                    text: "Always use a feature flag for rollout safety.".to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "policy".to_string(),
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
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-never".to_string(),
                    path: "/wiki/test/policy-b".to_string(),
                    summary: "policy b".to_string(),
                    text: "Do not use a feature flag for rollout safety.".to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "policy".to_string(),
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
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-dirty".to_string(),
                    path: "/wiki/test/dirty".to_string(),
                    summary: "dirty <think）leak".to_string(),
                    text: "A leaked <think） tag should be reported.".to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "dirty".to_string(),
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
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-duplicate-a".to_string(),
                    path: "/wiki/test/duplicate-a".to_string(),
                    summary: "duplicate a".to_string(),
                    text: "Duplicate token sequence exact match for wiki lint duplicate detection."
                        .to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "duplicate".to_string(),
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
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "wiki-duplicate-b".to_string(),
                    path: "/wiki/test/duplicate-b".to_string(),
                    summary: "duplicate b".to_string(),
                    text: "Duplicate token sequence exact match for wiki lint duplicate detection."
                        .to_string(),
                    importance: 0.7,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".to_string(),
                    topic: "duplicate".to_string(),
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
                    domain: Some("general".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                },
                MemoryEntry {
                    id: "skill-snapshot-a".to_string(),
                    path: "/skills/coding/merge-a/distilled/20260406T000000".to_string(),
                    summary: "merge a".to_string(),
                    text: "Follow SOP: inspect logs, isolate failure, add regression test."
                        .to_string(),
                    importance: 0.9,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "decision".to_string(),
                    topic: "merge_a".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec!["skill:merge-a".to_string()],
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
                },
                MemoryEntry {
                    id: "skill-snapshot-b".to_string(),
                    path: "/skills/coding/merge-b/distilled/20260406T000100".to_string(),
                    summary: "merge b".to_string(),
                    text: "Follow SOP: inspect logs, isolate failure, add regression test."
                        .to_string(),
                    importance: 0.9,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "decision".to_string(),
                    topic: "merge_b".to_string(),
                    keywords: vec![],
                    persons: vec![],
                    entities: vec!["skill:merge-b".to_string()],
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
                },
            ];
            for entry in entries {
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }

            let skill_a = HubCapability {
                id: "skill:merge-a".to_string(),
                cap_type: "skill".to_string(),
                name: "merge-a".to_string(),
                version: 1,
                description: "merge skill a".to_string(),
                definition: json!({
                    "content": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "prompt": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "policy": {"visibility": "listed"},
                    "skill_path": "/skills/coding/merge-a"
                })
                .to_string(),
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 3,
                successes: 3,
                failures: 0,
                avg_rating: 0.2,
                last_used: Some((Utc::now() - chrono::Duration::days(40)).to_rfc3339()),
                created_at: Utc::now().to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
            };
            let skill_b = HubCapability {
                id: "skill:merge-b".to_string(),
                cap_type: "skill".to_string(),
                name: "merge-b".to_string(),
                version: 1,
                description: "merge skill b".to_string(),
                definition: json!({
                    "content": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "prompt": "Follow SOP: inspect logs, isolate failure, add regression test.",
                    "policy": {"visibility": "listed"},
                    "skill_path": "/skills/coding/merge-b"
                })
                .to_string(),
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 5,
                successes: 5,
                failures: 0,
                avg_rating: 4.5,
                last_used: Some(Utc::now().to_rfc3339()),
                created_at: Utc::now().to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
            };
            store.hub_register(&skill_a).map_err(|e| e.to_string())?;
            store.hub_register(&skill_b).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed wiki lint fixtures");

    let response = server
        .wiki_lint(Parameters(WikiLintParams {
            path_prefix: Some("/wiki/test".to_string()),
            checks: vec![
                "orphans".to_string(),
                "contradictions".to_string(),
                "stale".to_string(),
                "missing_edges".to_string(),
                "dirty_data".to_string(),
                "duplicates".to_string(),
            ],
            limit: 50,
            stale_days: 90,
            missing_edge_threshold: 0.6,
            contradiction_threshold: 0.6,
            include_skill_quality: true,
        }))
        .await
        .expect("wiki_lint should succeed");
    let json: Value = serde_json::from_str(&response).expect("wiki_lint json");
    assert!(
        json["orphans"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "wiki-orphan"),
        "expected orphan node in wiki_lint output"
    );
    assert!(
        json["stale_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "wiki-orphan"),
        "expected stale node in wiki_lint output"
    );
    assert!(
        !json["missing_edge_hints"].as_array().unwrap().is_empty(),
        "expected missing edge hints"
    );
    assert!(
        !json["contradiction_candidates"]
            .as_array()
            .unwrap()
            .is_empty(),
        "expected contradiction candidates"
    );
    assert!(
        json["dirty_data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "wiki-dirty"),
        "expected dirty data finding"
    );
    assert!(
        json["duplicates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| { v["left_id"] == "wiki-duplicate-a" && v["right_id"] == "wiki-duplicate-b" }),
        "expected duplicate finding"
    );

    let archived = server
        .with_global_store_read(|store| store.hub_get("skill:merge-a").map_err(|e| e.to_string()))
        .expect("load archived skill")
        .expect("archived skill should exist");
    let archived_def: Value =
        serde_json::from_str(&archived.definition).expect("archived skill def json");
    assert_eq!(archived_def["quality_guard"]["status"], "archived");
    assert_eq!(archived_def["policy"]["visibility"], "hidden");
    assert!(
        archived_def["quality_guard"]["merge_hints"]
            .as_array()
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "expected merge hints on archived skill"
    );
    let related_edges = server
        .with_global_store_read(|store| {
            store
                .get_edges("skill-snapshot-a", "both", Some("merge_hint"))
                .map_err(|e| e.to_string())
        })
        .expect("load related skill edges");
    assert!(
        !related_edges.is_empty(),
        "expected skill graph merge_hint edge from quality guard"
    );
}

#[tokio::test]
async fn wiki_browse_large_limit_keeps_related_entries_empty() {
    let mut alpha = make_entry("wiki-large-limit-alpha");
    alpha.path = "/wiki/engineering/scale/alpha".to_string();
    alpha.summary = "Alpha scale".to_string();
    alpha.text = "Alpha scale lesson for MCP".to_string();
    alpha.entities = vec!["MCP".to_string()];

    let mut beta = make_entry("wiki-large-limit-beta");
    beta.path = "/wiki/engineering/scale/beta".to_string();
    beta.summary = "Beta scale".to_string();
    beta.text = "Beta scale lesson for MCP".to_string();
    beta.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .wiki_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/scale".to_string()),
            limit: 21,
            project: "wiki".to_string(),
        }))
        .await
        .expect("wiki browse should succeed");
    assert!(
        response.contains("/wiki/engineering/scale/alpha"),
        "browse markdown should contain alpha path"
    );
    assert!(
        response.contains("/wiki/engineering/scale/beta"),
        "browse markdown should contain beta path"
    );
}
