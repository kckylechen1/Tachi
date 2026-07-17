use super::*;

fn base_write_params(path: Option<String>) -> WikiWriteParams {
    WikiWriteParams {
        title: "Lifecycle write test".to_string(),
        text: "Lifecycle write test body.".to_string(),
        path,
        topic: Some("lifecycle-write-test".to_string()),
        summary: None,
        category: "experience".to_string(),
        keywords: vec![],
        entities: vec![],
        importance: 0.7,
        scope: "global".to_string(),
        retention_policy: "permanent".to_string(),
        domain: None,
        project: None,
        metadata: None,
        force: true,
        references: vec![],
        include_patterns: false,
        pattern_query: None,
        pattern_top_k: None,
    }
}

/// #1072: `tachi_wiki_write` stamps a real `lifecycle` (not the old
/// hardcoded-forever `"active"` `status`) — `active` for an ordinary path,
/// `pending_review` for the one draft-path convention this leaf's own
/// writer is aware of (`/wiki/drafts/...`).
#[tokio::test]
async fn tachi_wiki_write_stamps_active_lifecycle_for_ordinary_path() {
    let server = make_server();
    let response = server
        .tachi_wiki_write(Parameters(base_write_params(None)))
        .await
        .expect("wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert_eq!(entry["metadata"]["lifecycle"], json!("active"));
    assert_eq!(entry["metadata"]["artifact_kind"], json!("wiki"));
}

#[tokio::test]
async fn tachi_wiki_write_stamps_pending_review_lifecycle_for_drafts_path() {
    let server = make_server();
    let response = server
        .tachi_wiki_write(Parameters(base_write_params(Some(
            "/wiki/drafts/lifecycle-write-test".to_string(),
        ))))
        .await
        .expect("wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert_eq!(entry["metadata"]["lifecycle"], json!("pending_review"));
    assert_eq!(entry["metadata"]["artifact_kind"], json!("draft"));
}
