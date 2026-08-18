use super::*;

#[tokio::test]
async fn post_card_check_inbox_and_update_roundtrip() {
    let server = make_server();

    let posted = handle_post_card(
        &server,
        PostCardParams {
            from_agent: "hapi".to_string(),
            to_agent: "iris".to_string(),
            title: "Need review".to_string(),
            body: "Please review PR #42".to_string(),
            priority: "high".to_string(),
            card_type: "request".to_string(),
            thread_id: Some("thread-42".to_string()),
            workspace_id: None,
            project_id: None,
            conversation_id: None,
            agent_session_id: None,
        },
    )
    .await
    .expect("post_card should succeed");
    let posted_json: serde_json::Value =
        serde_json::from_str(&posted).expect("post_card response should be JSON");
    let card_id = posted_json["card_id"]
        .as_str()
        .expect("post_card should return card_id")
        .to_string();

    let card_memory = server
        .get_memory(Parameters(GetMemoryParams {
            id: card_id.clone(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("kanban card should be persisted as memory");
    let card_memory_json: serde_json::Value =
        serde_json::from_str(&card_memory).expect("kanban memory should be JSON");
    assert_eq!(card_memory_json["retention_policy"], json!("pinned"));

    let inbox = handle_check_inbox(
        &server,
        CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: Some("open".to_string()),
            since: None,
            include_broadcast: true,
            limit: 20,
            workspace_id: None,
            conversation_id: None,
        },
    )
    .await
    .expect("check_inbox should succeed");
    let inbox_json: serde_json::Value =
        serde_json::from_str(&inbox).expect("check_inbox response should be JSON");
    let cards = inbox_json["cards"]
        .as_array()
        .expect("check_inbox should return cards array");
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["id"], json!(card_id));
    assert_eq!(cards[0]["status"], json!("open"));

    let updated = handle_update_card(
        &server,
        UpdateCardParams {
            card_id: card_id.clone(),
            new_status: "acknowledged".to_string(),
            response_text: Some("Got it".to_string()),
        },
    )
    .await
    .expect("update_card should succeed");
    let updated_json: serde_json::Value =
        serde_json::from_str(&updated).expect("update_card response should be JSON");
    assert_eq!(updated_json["updated"], json!(true));

    let inbox_after = handle_check_inbox(
        &server,
        CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: Some("acknowledged".to_string()),
            since: None,
            include_broadcast: true,
            limit: 20,
            workspace_id: None,
            conversation_id: None,
        },
    )
    .await
    .expect("check_inbox after update should succeed");
    let inbox_after_json: serde_json::Value =
        serde_json::from_str(&inbox_after).expect("check_inbox after update should be JSON");
    let cards_after = inbox_after_json["cards"]
        .as_array()
        .expect("cards_after should be array");
    assert_eq!(cards_after.len(), 1);
    assert_eq!(cards_after[0]["status"], json!("acknowledged"));
    assert!(
        cards_after[0]["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Got it"),
        "response text should be appended to body"
    );
}

#[tokio::test]
async fn post_card_includes_provenance_context() {
    let server = make_server();

    let posted = handle_post_card(
        &server,
        PostCardParams {
            from_agent: "hapi".to_string(),
            to_agent: "iris".to_string(),
            title: "Need review".to_string(),
            body: "Please review PR #42".to_string(),
            priority: "high".to_string(),
            card_type: "request".to_string(),
            thread_id: Some("thread-42".to_string()),
            workspace_id: Some("alpha".to_string()),
            project_id: None,
            conversation_id: Some("conv-42".to_string()),
            agent_session_id: Some("sess-42".to_string()),
        },
    )
    .await
    .expect("post_card should succeed");
    let posted_json: serde_json::Value = serde_json::from_str(&posted).expect("post JSON");
    let card_id = posted_json["card_id"]
        .as_str()
        .expect("post_card should return card_id")
        .to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: card_id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    let provenance = &fetched_json["metadata"]["provenance"];

    assert_eq!(provenance["tool_name"], json!("post_card"));
    assert_eq!(provenance["source_kind"], json!("kanban_card"));
    assert_eq!(provenance["context"]["from_agent"], json!("hapi"));
    assert_eq!(provenance["context"]["workspace_id"], json!("alpha"));
    assert_eq!(provenance["context"]["conversation_id"], json!("conv-42"));
}
