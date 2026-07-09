use super::*;

#[tokio::test]
async fn tachi_memory_save_emit_continuity_returns_event() {
    let server = make_server();
    let mem_id = "facade-continuity-save-001";

    let mut save = tachi_memory_params("save");
    save.format = Some("json".to_string());
    save.scope = Some("project".to_string());
    save.text = Some("Facade memory saves can opt into continuity event emission.".to_string());
    save.summary = Some("Facade continuity save".to_string());
    save.category = Some("preference".to_string());
    save.path = Some("/user/patterns/facade-continuity-save".to_string());
    save.id = Some(mem_id.to_string());
    save.force = true;
    save.emit_continuity = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, save)
        .await
        .expect("save should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("save response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert_eq!(
        parsed["continuity_event"]["event_type"],
        json!("memory.saved")
    );
    assert_eq!(
        parsed["continuity_event"]["projection_hints"],
        json!(["pattern"])
    );

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
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["memory_id"], json!(mem_id));
}
