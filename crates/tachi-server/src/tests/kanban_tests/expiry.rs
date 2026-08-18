use super::*;

#[tokio::test]
async fn kanban_update_to_expired_status_prevents_further_updates() {
    let server = make_server();

    // Post a card
    let post = handle_post_card(
        &server,
        PostCardParams {
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
        },
    )
    .await
    .expect("post_card should succeed");

    let post_json: Value = serde_json::from_str(&post).unwrap();
    let card_id = post_json["card_id"].as_str().unwrap();

    // Update to expired status
    handle_update_card(
        &server,
        UpdateCardParams {
            card_id: card_id.to_string(),
            new_status: "expired".to_string(),
            response_text: None,
        },
    )
    .await
    .expect("update_card to expired should succeed");

    let inbox = handle_check_inbox(
        &server,
        CheckInboxParams {
            agent_id: "receiver".to_string(),
            status_filter: Some("open".to_string()),
            since: None,
            limit: 100,
            include_broadcast: true,
            workspace_id: None,
            conversation_id: None,
        },
    )
    .await
    .expect("check_inbox should succeed");

    let inbox_json: Value = serde_json::from_str(&inbox).unwrap();
    let cards = inbox_json["cards"].as_array().unwrap();
    assert!(
        cards.iter().all(|c| c["id"].as_str().unwrap() != card_id),
        "expired card should not appear in inbox with open filter"
    );
}
