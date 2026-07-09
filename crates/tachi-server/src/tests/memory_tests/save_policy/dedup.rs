use super::*;
use crate::memory_search_ops::handle_save_memory;
use crate::tool_params::SaveMemoryParams;
use serde_json::Value;

#[tokio::test]
async fn save_memory_rejects_exact_path_text_duplicate_without_force() {
    let server = make_server();
    let path = format!("/scratch/tachi/dedup-test-{}", uuid::Uuid::new_v4());
    let text = "Deterministic dedup regression payload for save_memory guard.".to_string();

    let params = SaveMemoryParams {
        text: text.clone(),
        summary: String::new(),
        path: path.clone(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: false,
        auto_link: false,
        project: None,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
    };

    let first = handle_save_memory(&server, params.clone())
        .await
        .expect("first save");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    assert!(
        first_json["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("saved")),
        "{first_json:#}"
    );

    let second = handle_save_memory(&server, params)
        .await
        .expect("second save");
    let second_json: Value = serde_json::from_str(&second).expect("second json");
    assert_eq!(second_json["status"], "duplicate");
    assert_eq!(second_json["saved"], false);
    assert_eq!(second_json["id"], first_json["id"]);
}

#[tokio::test]
async fn save_memory_allows_duplicate_when_force_true() {
    let server = make_server();
    let path = format!("/scratch/tachi/dedup-force-{}", uuid::Uuid::new_v4());
    let text = "Force=true should bypass the exact path/text dedup guard.".to_string();

    let mut base = SaveMemoryParams {
        text: text.clone(),
        summary: String::new(),
        path: path.clone(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: false,
        auto_link: false,
        project: None,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
    };

    handle_save_memory(&server, base.clone())
        .await
        .expect("seed save");

    base.force = true;
    base.id = Some(uuid::Uuid::new_v4().to_string());
    let forced = handle_save_memory(&server, base)
        .await
        .expect("forced save");
    let forced_json: Value = serde_json::from_str(&forced).expect("forced json");
    assert!(
        forced_json["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("saved")),
        "{forced_json:#}"
    );
}
