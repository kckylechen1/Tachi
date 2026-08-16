use super::*;

#[tokio::test]
async fn check_inbox_respects_broadcast_toggle() {
    let server = make_server();

    server
        .post_card(Parameters(PostCardParams {
            from_agent: "aegis".to_string(),
            to_agent: "*".to_string(),
            title: "Fleet alert".to_string(),
            body: "CI pipeline blocked".to_string(),
            priority: "critical".to_string(),
            card_type: "alert".to_string(),
            thread_id: None,
            workspace_id: None,
            project_id: None,
            conversation_id: None,
            agent_session_id: None,
        }))
        .await
        .expect("post_card broadcast should succeed");

    let no_broadcast = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: None,
            since: None,
            include_broadcast: false,
            limit: 10,
            workspace_id: None,
            conversation_id: None,
        }))
        .await
        .expect("check_inbox without broadcast should succeed");
    let no_broadcast_json: serde_json::Value =
        serde_json::from_str(&no_broadcast).expect("check_inbox response should be JSON");
    assert_eq!(no_broadcast_json["count"], json!(0));

    let with_broadcast = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: None,
            since: None,
            include_broadcast: true,
            limit: 10,
            workspace_id: None,
            conversation_id: None,
        }))
        .await
        .expect("check_inbox with broadcast should succeed");
    let with_broadcast_json: serde_json::Value =
        serde_json::from_str(&with_broadcast).expect("check_inbox response should be JSON");
    assert_eq!(with_broadcast_json["count"], json!(1));
}

#[tokio::test]
async fn check_inbox_workspace_and_conversation_filters_require_exact_match() {
    let server = make_server();

    server
        .post_card(Parameters(PostCardParams {
            from_agent: "hapi".to_string(),
            to_agent: "iris".to_string(),
            title: "Scoped card".to_string(),
            body: "For workspace alpha / conversation conv-1".to_string(),
            priority: "medium".to_string(),
            card_type: "request".to_string(),
            thread_id: None,
            workspace_id: Some("alpha".to_string()),
            project_id: None,
            conversation_id: Some("conv-1".to_string()),
            agent_session_id: Some("sess-1".to_string()),
        }))
        .await
        .expect("scoped post_card should succeed");

    server
        .post_card(Parameters(PostCardParams {
            from_agent: "hapi".to_string(),
            to_agent: "iris".to_string(),
            title: "Unscoped card".to_string(),
            body: "Missing workspace / conversation metadata".to_string(),
            priority: "medium".to_string(),
            card_type: "request".to_string(),
            thread_id: None,
            workspace_id: None,
            project_id: None,
            conversation_id: None,
            agent_session_id: None,
        }))
        .await
        .expect("unscoped post_card should succeed");

    let filtered = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: None,
            since: None,
            include_broadcast: true,
            limit: 10,
            workspace_id: Some("alpha".to_string()),
            conversation_id: Some("conv-1".to_string()),
        }))
        .await
        .expect("filtered check_inbox should succeed");
    let filtered_json: serde_json::Value =
        serde_json::from_str(&filtered).expect("filtered check_inbox response should be JSON");
    let cards = filtered_json["cards"]
        .as_array()
        .expect("filtered cards should be array");

    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["workspace_id"], json!("alpha"));
    assert_eq!(cards[0]["conversation_id"], json!("conv-1"));
}
