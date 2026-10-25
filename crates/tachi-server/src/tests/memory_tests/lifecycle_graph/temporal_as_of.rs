use super::*;

/// tachi#2125: get + as_of returns a clear miss when the row is outside its window.
#[tokio::test]
async fn get_memory_as_of_outside_window_is_clear_miss() {
    let server = make_server();

    let id = format!("asof-miss-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&id);
    entry.text = "Alice lived in Berlin".to_string();
    entry.valid_from = "2024-01-01T00:00:00.000Z".to_string();
    entry.valid_until = Some("2024-06-01T00:00:00.000Z".to_string());
    entry.timestamp = entry.valid_from.clone();
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("save: {e}")))
        .expect("save entry");

    let miss = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
            as_of: Some("2024-08-01T00:00:00.000Z".to_string()),
        },
    )
    .await
    .expect("get_memory");
    let v: Value = serde_json::from_str(&miss).expect("json");
    assert_eq!(v["error"], json!("Memory not found"), "body: {miss}");
    assert_eq!(
        v["reason"],
        json!("outside_validity_window"),
        "body: {miss}"
    );
    assert_eq!(v["valid_at_as_of"], json!(false), "body: {miss}");
    assert_eq!(v["found"], json!(false), "body: {miss}");
    assert_eq!(v["id"], json!(id), "body: {miss}");

    // Without as_of, id fetch still returns the historical row.
    let hit = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
            as_of: None,
        },
    )
    .await
    .expect("get without as_of");
    let v: Value = serde_json::from_str(&hit).expect("json");
    assert!(v.get("error").is_none(), "body: {hit}");
    assert_eq!(v["id"], json!(id), "body: {hit}");
}

#[tokio::test]
async fn get_memory_as_of_inside_window_returns_row() {
    let server = make_server();

    let id = format!("asof-hit-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&id);
    entry.text = "Alice lived in Berlin".to_string();
    entry.valid_from = "2024-01-01T00:00:00.000Z".to_string();
    entry.valid_until = Some("2024-06-01T00:00:00.000Z".to_string());
    entry.timestamp = entry.valid_from.clone();
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("save: {e}")))
        .expect("save entry");

    let hit = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
            as_of: Some("2024-03-15T00:00:00.000Z".to_string()),
        },
    )
    .await
    .expect("get_memory");
    let v: Value = serde_json::from_str(&hit).expect("json");
    assert!(v.get("error").is_none(), "body: {hit}");
    assert_eq!(v["id"], json!(id), "body: {hit}");
}

#[tokio::test]
async fn save_memory_normalizes_ymd_cue_when_validity_omitted() {
    let server = make_server();
    let path = format!("/notes/temporal-cue-{}", uuid::Uuid::new_v4());
    let raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "On 2023-11-07 we chose Postgres for the primary store.".to_string(),
            summary: String::new(),
            path: path.clone(),
            importance: 0.6,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
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
        .expect("save");
    let saved: Value = serde_json::from_str(&raw).expect("json");
    let id = saved["id"].as_str().expect("id").to_string();

    let body = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
            as_of: None,
        },
    )
    .await
    .expect("get");
    let v: Value = serde_json::from_str(&body).expect("json");
    let stored = server
        .with_global_store_read(|store| store.get(&id).map_err(|e| format!("store get: {e}")))
        .expect("store get ok")
        .expect("row present");
    assert!(
        stored.valid_from.starts_with("2023-11-07"),
        "store valid_from={}, save_raw={raw}, get_body={body}",
        stored.valid_from
    );
    let vf = v["valid_from"].as_str().expect("get should surface valid_from");
    assert!(
        vf.starts_with("2023-11-07"),
        "expected cue-normalized valid_from, got {vf}; body={body}"
    );
}

#[tokio::test]
async fn save_memory_explicit_validity_wins_over_text_cues() {
    let server = make_server();
    let path = format!("/notes/temporal-explicit-{}", uuid::Uuid::new_v4());
    let raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Yesterday we rolled back the deploy.".to_string(),
            summary: String::new(),
            path: path.clone(),
            importance: 0.6,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: Some("2020-01-02T00:00:00.000Z".to_string()),
            valid_until: Some("2020-02-01T00:00:00.000Z".to_string()),
            metadata: None,
            emit_continuity: false,
        }))
        .await
        .expect("save");
    let saved: Value = serde_json::from_str(&raw).expect("json");
    let id = saved["id"].as_str().expect("id").to_string();

    let stored = server
        .with_global_store_read(|store| store.get(&id).map_err(|e| format!("store get: {e}")))
        .expect("store get ok")
        .expect("row present");
    assert!(
        stored.valid_from.starts_with("2020-01-02"),
        "explicit valid_from must win; got {}, save_raw={raw}",
        stored.valid_from
    );
    assert_eq!(
        stored.valid_until.as_deref().map(|s| &s[..10]),
        Some("2020-02-01"),
        "explicit valid_until must be preserved; got {:?}, save_raw={raw}",
        stored.valid_until
    );
}
