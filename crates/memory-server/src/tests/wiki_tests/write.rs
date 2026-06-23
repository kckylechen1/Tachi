use super::*;

#[tokio::test]
async fn tachi_wiki_write_stores_and_rejects_invalid_references() {
    let server = make_server();

    let bad = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Bad refs".to_string(),
            text: "Should not persist.".to_string(),
            path: None,
            topic: Some("bad-refs".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.5,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec!["relative/path.md".to_string()],
        }))
        .await
        .expect_err("relative path should fail validation");
    assert!(bad.contains("references[0]"), "err: {bad}");

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "Good refs".to_string(),
            text: "Lesson with links.".to_string(),
            path: None,
            topic: Some("good-refs".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec![],
            entities: vec![],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![
                "https://github.com/kckylechen1/tachi/issues/149".to_string(),
                "kckylechen1/tachi#149".to_string(),
            ],
        }))
        .await
        .expect("valid references should write");

    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    let refs = entry["metadata"]["source_refs"]
        .as_array()
        .expect("source_refs array");
    assert_eq!(refs.len(), 2);
}

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
            metadata: None,
            force: false,
            references: vec![],
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
            metadata: None,
            force: false,
            references: vec![],
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
            metadata: None,
            force: true,
            references: vec![],
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
            metadata: None,
            force: true,
            references: vec![],
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
            metadata: None,
            force: true,
            references: vec![],
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
async fn tachi_wiki_write_supports_explicit_markdown_format() {
    let server = make_server();

    let response = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "write".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            category: Some("experience".to_string()),
            top_k: None,
            limit: None,
            title: Some("Facade wiki markdown write".to_string()),
            text: Some("Facade wiki write should still support human-readable output.".to_string()),
            path: Some("/wiki/agent/tachi/facade-markdown-write".to_string()),
            topic: Some("facade-markdown-write".to_string()),
            summary: Some("Facade wiki markdown write".to_string()),
            keywords: Vec::new(),
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: None,
            project: None,
            domain: Some("engineering".to_string()),
            metadata: None,
            force: true,
        }))
        .await
        .expect("tachi_wiki markdown write should succeed");

    assert!(response.starts_with("## Tachi wiki write"), "{response}");
    assert!(response.contains("path:"), "{response}");
    assert!(
        response.contains("/wiki/agent/tachi/facade-markdown-write"),
        "{response}"
    );
}

#[tokio::test]
async fn tachi_wiki_write_preserves_guide_path_and_applies_to_metadata() {
    let server = make_server();

    let response = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "write".to_string(),
            format: None,
            query: None,
            category: Some("guide".to_string()),
            top_k: None,
            limit: None,
            title: Some("AgentReview guide".to_string()),
            text: Some(
                "AgentReview outputs should be routed by destination layer and promotion intent."
                    .to_string(),
            ),
            path: Some("/guide/global/workflows/agent-review".to_string()),
            topic: Some("agent-review-guide".to_string()),
            summary: Some("AgentReview routing guide".to_string()),
            keywords: vec!["agent-review".to_string()],
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: Some("global".to_string()),
            project: None,
            domain: Some("docs".to_string()),
            metadata: Some(json!({
                "layer": "caller-should-not-override",
                "applies_to": {
                    "task_type": ["agent_review"],
                    "profiles": ["codex_55_review"],
                    "stage": ["review"]
                }
            })),
            force: true,
        }))
        .await
        .expect("guide wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("write JSON");
    assert_eq!(
        json["wiki_path"],
        json!("/guide/global/workflows/agent-review")
    );
    let id = json["id"].as_str().expect("wiki id").to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get guide memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry JSON");
    assert_eq!(entry["path"], json!("/guide/global/workflows/agent-review"));
    assert_eq!(entry["metadata"]["layer"], json!("guide"));
    assert_eq!(entry["metadata"]["scope"], json!("global"));
    assert_eq!(entry["metadata"]["authority"], json!("playbook"));
    assert_eq!(
        entry["metadata"]["applies_to"]["task_type"],
        json!(["agent_review"])
    );
    assert_eq!(
        entry["metadata"]["applies_to"]["profiles"],
        json!(["codex_55_review"])
    );
    assert_eq!(entry["metadata"]["applies_to"]["stage"], json!(["review"]));
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
            references: vec![
                "https://example.com/spec".to_string(),
                "kckylechen1/tachi#149".to_string(),
            ],
            topic: Some("routing-boundary".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            files: Vec::new(),
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
    assert_eq!(
        fetched_json["metadata"]["source_refs"],
        json!(["https://example.com/spec", "kckylechen1/tachi#149"])
    );
}
