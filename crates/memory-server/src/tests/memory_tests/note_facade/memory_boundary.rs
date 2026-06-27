use super::*;

#[tokio::test]
async fn tachi_memory_save_with_title_stays_memory() {
    let server = make_server();

    let saved = server
        .tachi_memory(Parameters(TachiMemoryParams {
            action: "save".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: None,
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
            text: Some("A one-line memory fact with a title should not become wiki.".to_string()),
            title: Some("Memory title only".to_string()),
            summary: Some("Memory title only".to_string()),
            topic: Some("memory-boundary".to_string()),
            keywords: vec!["memory-boundary".to_string()],
            entities: Vec::new(),
            importance: Some(0.7),
            retention_policy: None,
            kind: None,
            path: Some("/facts/memory-boundary".to_string()),
            id: None,
            force: true,
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
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
        }))
        .await
        .expect("tachi_memory save should succeed");
    assert!(saved.contains("Saved ->"));
    assert!(saved.contains("/facts/memory-boundary"));
    let id = saved
        .split("id: `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("save markdown id")
        .to_string();
    assert!(!id.is_empty());
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(fetched_json["path"], json!("/facts/memory-boundary"));
    assert_ne!(fetched_json["metadata"]["wiki"], json!(true));
}
