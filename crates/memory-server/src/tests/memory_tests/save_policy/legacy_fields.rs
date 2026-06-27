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

#[test]
fn fact_to_entry_merges_legacy_persons_into_entities() {
    let fact = json!({
        "text": "Kyle migrated Sigil search completely and successfully.",
        "topic": "migration",
        "keywords": ["sigil", "search"],
        "persons": ["Kyle", ""],
        "entities": ["Sigil", "memory-server"],
        "scope": "project",
        "importance": 0.9
    });

    let entry = crate::tool_params::fact_to_entry(&fact, "extraction", json!({}))
        .expect("fact_to_entry should build an entry");
    assert!(entry.persons.is_empty());
    assert_eq!(
        entry.entities,
        vec![
            "Sigil".to_string(),
            "memory-server".to_string(),
            "Kyle".to_string(),
            "user".to_string()
        ]
    );
}

#[test]
fn fact_to_entry_with_reason_surfaces_drop_cause() {
    use crate::tool_params::{
        fact_to_entry, fact_to_entry_with_reason, FACT_DROP_EMPTY, FACT_DROP_TOO_SHORT,
    };

    // Empty text -> empty_text reason.
    assert_eq!(
        fact_to_entry_with_reason(&json!({"text": ""}), "extraction", json!({})).err(),
        Some(FACT_DROP_EMPTY)
    );

    // A faithfully-compressed short fact is dropped by the MIN_FACT_CHAR_COUNT
    // floor, and the reason is now surfaced instead of vanishing silently.
    let short = json!({"text": "端口会漂移", "topic": "t", "scope": "project"});
    assert_eq!(
        fact_to_entry_with_reason(&short, "extraction", json!({})).err(),
        Some(FACT_DROP_TOO_SHORT)
    );
    // The lossy wrapper still collapses both to None (off/default path unchanged).
    assert!(fact_to_entry(&short, "extraction", json!({})).is_none());

    // force=true bypasses the gate, so the same short fact builds an entry.
    assert!(fact_to_entry_with_reason(&short, "extraction", json!({"force": true})).is_ok());
}
