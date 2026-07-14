use super::*;

#[tokio::test]
async fn save_memory_updates_legacy_location_on_overwrite() {
    let server = make_server();
    let id = "legacy-location-update-boundary";

    for location in ["St. Louis", "Shanghai"] {
        let saved = server
            .save_memory(Parameters(SaveMemoryParams {
                text: "Location overwrite test.".to_string(),
                summary: "Location overwrite".to_string(),
                path: "/project/location-overwrite".to_string(),
                importance: 0.7,
                category: "fact".to_string(),
                topic: "legacy-fields".to_string(),
                keywords: vec!["legacy".to_string()],
                persons: vec![],
                entities: vec!["Tachi".to_string()],
                location: location.to_string(),
                scope: "project".to_string(),
                vector: None,
                id: Some(id.to_string()),
                force: true,
                auto_link: false,
                project: None,
                project_explicit: false,
                retention_policy: None,
                domain: None,
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: None,
                emit_continuity: false,
            }))
            .await
            .expect("save_memory should succeed");
        let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
        assert!(saved_json.get("location").is_none());
    }

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    assert!(fetched_json.get("location").is_none());
    assert_eq!(
        fetched_json["metadata"]["legacy_location"],
        json!("Shanghai")
    );
}
