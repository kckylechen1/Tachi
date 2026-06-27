use super::*;

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
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
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
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
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
            emit_continuity: false,
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
