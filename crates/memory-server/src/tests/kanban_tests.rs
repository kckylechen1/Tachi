use super::*;

#[tokio::test]
async fn post_card_check_inbox_and_update_roundtrip() {
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
    let server = make_server();

    let posted = server
        .post_card(Parameters(PostCardParams {
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
        }))
        .await
        .expect("post_card should succeed");
    let posted_json: serde_json::Value =
        serde_json::from_str(&posted).expect("post_card response should be JSON");
    let card_id = posted_json["card_id"]
        .as_str()
        .expect("post_card should return card_id")
        .to_string();

    let inbox = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: Some("open".to_string()),
            since: None,
            include_broadcast: true,
            limit: 20,
            workspace_id: None,
            conversation_id: None,
        }))
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

    let updated = server
        .update_card(Parameters(UpdateCardParams {
            card_id: card_id.clone(),
            new_status: "acknowledged".to_string(),
            response_text: Some("Got it".to_string()),
        }))
        .await
        .expect("update_card should succeed");
    let updated_json: serde_json::Value =
        serde_json::from_str(&updated).expect("update_card response should be JSON");
    assert_eq!(updated_json["updated"], json!(true));

    let inbox_after = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "iris".to_string(),
            status_filter: Some("acknowledged".to_string()),
            since: None,
            include_broadcast: true,
            limit: 20,
            workspace_id: None,
            conversation_id: None,
        }))
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
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
    let server = make_server();

    let posted = server
        .post_card(Parameters(PostCardParams {
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
        }))
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

#[tokio::test]
async fn check_inbox_respects_broadcast_toggle() {
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
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
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
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

#[tokio::test]
async fn post_card_accepts_acpx_card_types() {
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
    let server = make_server();

    for card_type in ["ack", "progress", "result"] {
        let posted = server
            .post_card(Parameters(PostCardParams {
                from_agent: "iris".to_string(),
                to_agent: "hapi".to_string(),
                title: format!("{} update", card_type),
                body: format!("{} body", card_type),
                priority: "medium".to_string(),
                card_type: card_type.to_string(),
                thread_id: Some("thread-acpx".to_string()),
                workspace_id: None,
                project_id: None,
                conversation_id: None,
                agent_session_id: None,
            }))
            .await
            .expect("post_card should accept ACPX card type");
        let posted_json: serde_json::Value =
            serde_json::from_str(&posted).expect("post_card response should be JSON");
        assert_eq!(posted_json["db"], json!("global"));
    }

    let inbox = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "hapi".to_string(),
            status_filter: None,
            since: None,
            include_broadcast: true,
            limit: 10,
            workspace_id: None,
            conversation_id: None,
        }))
        .await
        .expect("check_inbox for ACPX cards should succeed");
    let inbox_json: serde_json::Value =
        serde_json::from_str(&inbox).expect("check_inbox response should be JSON");
    let cards = inbox_json["cards"]
        .as_array()
        .expect("cards should be array");

    assert!(cards.iter().any(|card| card["card_type"] == json!("ack")));
    assert!(cards
        .iter()
        .any(|card| card["card_type"] == json!("progress")));
    assert!(cards
        .iter()
        .any(|card| card["card_type"] == json!("result")));
}

#[tokio::test]
async fn kanban_update_to_expired_status_prevents_further_updates() {
    let server = make_server();

    // Post a card
    let post = server
        .post_card(Parameters(PostCardParams {
            from_agent: "sender".to_string(),
            to_agent: "receiver".to_string(),
            title: "Test Card".to_string(),
            body: "Test body".to_string(),
            card_type: "request".to_string(),
            priority: "medium".to_string(),
            workspace_id: None,
            project_id: None,
            conversation_id: None,
            thread_id: None,
            agent_session_id: None,
        }))
        .await
        .expect("post_card should succeed");

    let post_json: Value = serde_json::from_str(&post).unwrap();
    let card_id = post_json["card_id"].as_str().unwrap();

    // Update to expired status
    server
        .update_card(Parameters(UpdateCardParams {
            card_id: card_id.to_string(),
            new_status: "expired".to_string(),
            response_text: None,
        }))
        .await
        .expect("update_card to expired should succeed");

    let inbox = server
        .check_inbox(Parameters(CheckInboxParams {
            agent_id: "receiver".to_string(),
            status_filter: Some("open".to_string()),
            since: None,
            limit: 100,
            include_broadcast: true,
            workspace_id: None,
            conversation_id: None,
        }))
        .await
        .expect("check_inbox should succeed");

    let inbox_json: Value = serde_json::from_str(&inbox).unwrap();
    let cards = inbox_json["cards"].as_array().unwrap();
    assert!(
        cards.iter().all(|c| c["id"].as_str().unwrap() != card_id),
        "expired card should not appear in inbox with open filter"
    );
}
