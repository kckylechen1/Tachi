use super::*;

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
            format: None,
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
