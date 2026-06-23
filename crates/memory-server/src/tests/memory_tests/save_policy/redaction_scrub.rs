use super::*;

#[tokio::test]
async fn save_memory_redacts_obvious_secrets_before_persisting() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "The bearer is Authorization: Bearer test-bearer-token-abcdefghijklmnopqrstuvwxyz and token=secret_token_value_abcdefghijklmnopqrstuvwxyz".to_string(),
            summary: "redaction".to_string(),
            path: "/scratch/redaction".to_string(),
            importance: 0.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
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
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    assert!(saved_json["secret_redactions"].as_u64().unwrap_or(0) >= 2);
    let id = saved_json["id"].as_str().expect("saved id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: Value = serde_json::from_str(&fetched).expect("get JSON");
    let text = fetched_json["text"].as_str().unwrap_or_default();
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains("test-bearer-token-abcdefghijklmnopqrstuvwxyz"));
    assert!(!text.contains("secret_token_value_abcdefghijklmnopqrstuvwxyz"));
}

#[tokio::test]
async fn save_memory_scrubs_think_tags_from_text_and_summary() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Keep this.\n<think>private reasoning\nwith details</think>\nAnd this."
                .to_string(),
            summary: "Summary <think>hidden</think> visible".to_string(),
            path: "/scratch/tests/think-scrub".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["think-scrub".to_string()],
            persons: vec![],
            entities: vec!["memory-server".to_string()],
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
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("saved id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: Value = serde_json::from_str(&fetched).expect("get JSON");
    let text = fetched_json["text"].as_str().unwrap_or_default();
    let summary = fetched_json["summary"].as_str().unwrap_or_default();
    assert!(text.contains("Keep this."));
    assert!(text.contains("And this."));
    assert!(!text.contains("<think>"));
    assert!(!text.contains("private reasoning"));
    assert_eq!(summary, "Summary  visible");
}
