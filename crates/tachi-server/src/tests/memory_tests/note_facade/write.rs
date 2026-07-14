use super::*;

#[tokio::test]
async fn tachi_save_note_writes_markdown_file_and_normalizes_scope() {
    let (server, _temp_home) = make_server_with_temp_home();

    let saved = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "中文 note body for UTF-8 slug safety".to_string(),
            id: None,
            kind: None,
            title: Some("中文 Note 标题".to_string()),
            summary: Some("note summary".to_string()),
            path: Some("brainstorm/demo.md".to_string()),
            importance: Some(0.7),
            category: None,
            keywords: vec!["brainstorm".to_string()],
            entities: Vec::new(),
            scope: Some("note".to_string()),
            project: None,
            project_explicit: false,
            domain: None,
            retention_policy: None,
            force: true,
            references: Vec::new(),
            topic: Some("notes-test".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            files: Vec::new(),
            format: None,
        }))
        .await
        .expect("tachi_save note should succeed");
    let json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let note_file = json["note_file"].as_str().expect("note file returned");
    let note_path = json["note_path"].as_str().expect("note path returned");
    assert_eq!(note_path, "brainstorm/demo.md");
    let md = std::fs::read_to_string(note_file).expect("note markdown should exist");
    assert!(md.contains("title: \"中文 Note 标题\""));
    assert!(md.contains("中文 note body for UTF-8 slug safety"));

    let id = json["id"].as_str().expect("memory id returned").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get note memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(
        fetched_json["path"],
        serde_json::json!("/notes/brainstorm/demo.md")
    );
    assert_ne!(fetched_json["scope"], serde_json::json!("note"));
}
