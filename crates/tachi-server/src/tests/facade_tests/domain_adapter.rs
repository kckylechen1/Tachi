use super::*;

fn adapter_params(action: &str) -> TachiDomainAdapterParams {
    TachiDomainAdapterParams {
        action: action.to_string(),
        project: None,
        domain: None,
        actor: None,
        session_id: None,
        character: None,
        entries: Vec::new(),
        project_events: false,
        dry_run: false,
    }
}

fn event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

#[tokio::test]
async fn lorebook_adapter_imports_romanbath_entries_as_worldbook_context() {
    let (server, home) = make_server_with_temp_home();
    let db_path = home
        .temp_home
        .join(".tachi/projects/romanbath-test/memory.db");
    std::fs::create_dir_all(db_path.parent().expect("named project parent"))
        .expect("create named project dir");
    let db_str = db_path.to_str().expect("utf-8 db path");
    memcore::MemoryStore::open_with_label(db_str, "romanbath-test")
        .expect("create named project DB");

    let mut import = adapter_params("lorebook_import");
    import.project = Some("romanbath-test".to_string());
    import.domain = Some("agent_os".to_string());
    import.character = Some("Mara".to_string());
    import.project_events = true;
    import.entries = vec![json!({
        "id": "entry-1",
        "keys": ["rain"],
        "secondary_keys": ["harbor"],
        "content": "Rain at the harbor changes Mara's route.",
        "enabled": true,
        "selective": true,
        "position": "before_char",
        "token_budget": 80,
        "priority": 12,
        "recursive": false
    })];

    let imported = crate::domain_adapter_ops::handle_tachi_domain_adapter(&server, import)
        .await
        .expect("import lorebook");
    let imported_json: Value = serde_json::from_str(&imported).expect("import JSON");
    assert_eq!(imported_json["adapter"], json!("lorebook_worldbook"));
    assert_eq!(imported_json["imported_count"], json!(1));
    assert_eq!(imported_json["projection"]["projected_count"], json!(1));

    let mut context = event_params("context");
    context.project = Some("romanbath-test".to_string());
    context.projection_hints = vec!["world_book".to_string()];
    let body = crate::event_ops::handle_tachi_event(&server, context)
        .await
        .expect("worldbook context");
    let parsed: Value = serde_json::from_str(&body).expect("context JSON");
    assert_eq!(parsed["lorebook"][0]["lorebook"]["keys"], json!(["rain"]));
    assert_eq!(
        parsed["lorebook"][0]["lorebook"]["secondary_keys"],
        json!(["harbor"])
    );
    assert_eq!(parsed["lorebook"][0]["lorebook"]["token_budget"], json!(80));
}
