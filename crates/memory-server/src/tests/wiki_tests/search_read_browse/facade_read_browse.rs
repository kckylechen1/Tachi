use super::*;

#[tokio::test]
async fn tachi_wiki_read_and_browse_default_to_json() {
    let mut entry = make_entry("wiki-facade-json-read");
    entry.path = "/wiki/engineering/json-read".to_string();
    entry.summary = "JSON wiki read summary".to_string();
    entry.text = "JsonReadFacadeNeedle should be structured.".to_string();
    entry.keywords = vec!["json-read".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let read = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "read".to_string(),
            format: None,
            query: None,
            category: None,
            top_k: None,
            limit: None,
            title: None,
            text: None,
            path: Some("/wiki/engineering/json-read".to_string()),
            topic: None,
            summary: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: None,
            project: None,
            domain: None,
            metadata: None,
            force: false,
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki read should succeed");
    let read_json: Value = serde_json::from_str(&read).expect("read JSON");
    assert_eq!(read_json["status"], json!("found"));
    assert_eq!(
        read_json["entry"]["text"],
        json!("JsonReadFacadeNeedle should be structured.")
    );

    let browse = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "browse".to_string(),
            format: None,
            query: None,
            category: Some("engineering".to_string()),
            top_k: None,
            limit: Some(10),
            title: None,
            text: None,
            path: None,
            topic: None,
            summary: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: None,
            project: None,
            domain: None,
            metadata: None,
            force: false,
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki browse should succeed");
    let browse_json: Value = serde_json::from_str(&browse).expect("browse JSON");
    assert_eq!(browse_json["status"], json!("completed"));
    assert_eq!(browse_json["kind"], json!("category"));
    assert!(browse_json["entries"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["path"] == json!("/wiki/engineering/json-read"))
    }));
}
