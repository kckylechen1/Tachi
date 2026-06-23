use super::*;

#[tokio::test]
async fn save_memory_allows_curated_tier_metadata() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Curated trading lessons should enter the lifecycle as consolidated knowledge."
                .to_string(),
            summary: "Curated lifecycle tier".to_string(),
            path: "/trading/equity/lessons/tier-test".to_string(),
            importance: 0.85,
            category: "experience".to_string(),
            topic: "memory-lifecycle".to_string(),
            keywords: vec!["tier".to_string()],
            persons: vec![],
            entities: vec!["Tachi".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("equity_trading".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({"tier": "consolidated"})),
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("id").to_string();

    let tier = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT tier FROM memories WHERE id = ?1", [&id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| e.to_string())
        })
        .expect("read tier");
    assert_eq!(tier, "consolidated");
}

#[tokio::test]
async fn save_memory_clamps_importance_into_valid_range() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "importance clamp regression".to_string(),
            summary: "importance clamp".to_string(),
            path: "/project/tests".to_string(),
            importance: 9.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["importance".to_string()],
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

    let saved_json: serde_json::Value =
        serde_json::from_str(&saved).expect("save should be valid JSON");
    let id = saved_json["id"].as_str().expect("save should return id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value =
        serde_json::from_str(&fetched).expect("get should be valid JSON");
    assert_eq!(fetched_json["importance"], json!(1.0));
}

#[tokio::test]
async fn save_memory_noise_rejection_returns_structured_json() {
    let server = make_server();

    let response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "hello".to_string(),
            summary: String::new(),
            path: "/scratch/tests/noise".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: false,
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
        .expect("noise rejection should be a normal JSON response");

    let json: Value = serde_json::from_str(&response).expect("noise JSON");
    assert_eq!(json["saved"], json!(false));
    assert_eq!(json["noise"], json!(true));
}

#[test]
fn strip_code_fence_uses_last_closing_fence() {
    let raw = "```json\n{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}\n```";
    let stripped = crate::llm::LlmClient::strip_code_fence(raw);
    assert_eq!(
        stripped,
        "{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}"
    );
}
