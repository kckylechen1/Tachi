use super::*;

#[tokio::test]
async fn save_memory_folds_legacy_persons_and_location_out_of_public_fields() {
    let server = make_server();
    let id = "legacy-person-location-boundary";

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text:
                "Kyle validated that legacy OpenClaw person and location fields are boundary-only."
                    .to_string(),
            summary: "Legacy field boundary".to_string(),
            path: "/project/legacy-boundary".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "legacy-fields".to_string(),
            keywords: vec!["legacy".to_string()],
            persons: vec!["Kyle".to_string()],
            entities: vec!["Tachi".to_string()],
            location: "St. Louis".to_string(),
            scope: "project".to_string(),
            vector: None,
            id: Some(id.to_string()),
            force: true,
            auto_link: false,
            project: None,
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
    assert!(saved_json.get("persons").is_none());
    assert!(saved_json.get("location").is_none());

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    assert!(fetched_json.get("persons").is_none());
    assert!(fetched_json.get("location").is_none());
    assert_eq!(fetched_json["entities"], json!(["Tachi", "Kyle", "user"]));
    assert_eq!(
        fetched_json["metadata"]["legacy_location"],
        json!("St. Louis")
    );
}
