use super::*;

#[tokio::test]
async fn save_memory_emit_continuity_writes_memory_saved_event() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Continuity memory saves can explicitly produce projection-hinted events."
                .to_string(),
            summary: "Continuity save event".to_string(),
            path: "/scratch/continuity/save-event".to_string(),
            importance: 0.8,
            category: "preference".to_string(),
            topic: "continuity".to_string(),
            keywords: vec!["continuity".to_string()],
            persons: vec![],
            entities: vec!["Tachi".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: Some("continuity-save-event-memory".to_string()),
            force: true,
            auto_link: false,
            project: None,
            retention_policy: Some("durable".to_string()),
            domain: Some("agent_os".to_string()),
            timestamp: Some("2026-06-24T00:00:00Z".to_string()),
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: true,
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    assert_eq!(
        saved_json["continuity_event"]["event_type"],
        json!("memory.saved")
    );
    assert_eq!(
        saved_json["continuity_event"]["projection_hints"],
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
    assert_eq!(events[0].event_type, "memory.saved");
    assert_eq!(
        events[0].projection_hints,
        vec![memcore::ProjectionKind::Pattern]
    );
    assert_eq!(
        events[0].payload["memory_id"],
        json!("continuity-save-event-memory")
    );
}
