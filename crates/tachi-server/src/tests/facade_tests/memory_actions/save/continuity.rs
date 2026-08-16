use super::*;

#[tokio::test]
async fn tachi_memory_save_cannot_emit_continuity_event() {
    let server = make_server();
    let mem_id = "facade-continuity-save-001";

    let save: TachiMemoryParams = serde_json::from_value(json!({
        "action": "save",
        "format": "json",
        "scope": "project",
        "text": "Ordinary facade memory saves cannot emit continuity evidence.",
        "summary": "Facade continuity save",
        "category": "preference",
        "path": "/user/patterns/facade-continuity-save",
        "id": mem_id,
        "force": true,
        "emit_continuity": true
    }))
    .expect("legacy payload still deserializes without granting evidence authority");
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, save)
        .await
        .expect("ordinary save should remain functional");
    let parsed: Value = serde_json::from_str(&body).expect("save response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert!(parsed.get("continuity_event").is_none());

    let events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    event_type: Some("memory.saved".to_string()),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("read continuity events");
    assert!(events.is_empty());
}

#[tokio::test]
async fn tachi_save_cannot_emit_continuity_event() {
    let server = make_server();
    let mem_id = "standalone-facade-continuity-save-001";

    let save: crate::tool_params::TachiSaveParams = serde_json::from_value(json!({
        "text": "Ordinary standalone saves cannot emit continuity evidence.",
        "kind": "memory",
        "format": "json",
        "scope": "project",
        "summary": "Standalone continuity save",
        "category": "preference",
        "path": "/user/patterns/standalone-continuity-save",
        "id": mem_id,
        "force": true,
        "emit_continuity": true
    }))
    .expect("legacy payload still deserializes without granting evidence authority");
    let body = crate::facade_save_ops::handle_tachi_save(&server, save)
        .await
        .expect("ordinary save should remain functional");
    let parsed: Value = serde_json::from_str(&body).expect("save response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert!(parsed.get("continuity_event").is_none());

    let events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    event_type: Some("memory.saved".to_string()),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("read continuity events");
    assert!(events.is_empty());
}
