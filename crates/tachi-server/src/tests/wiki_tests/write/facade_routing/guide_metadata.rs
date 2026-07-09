use super::*;

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
