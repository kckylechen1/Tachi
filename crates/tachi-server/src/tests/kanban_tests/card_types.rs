use super::*;

#[tokio::test]
async fn post_card_accepts_acpx_card_types() {
    let server = make_server();

    for card_type in ["ack", "progress", "result"] {
        let posted = handle_post_card(
            &server,
            PostCardParams {
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
            },
        )
        .await
        .expect("post_card should accept ACPX card type");
        let posted_json: serde_json::Value =
            serde_json::from_str(&posted).expect("post_card response should be JSON");
        assert_eq!(posted_json["db"], json!("global"));
    }

    let inbox = handle_check_inbox(
        &server,
        CheckInboxParams {
            agent_id: "hapi".to_string(),
            status_filter: None,
            since: None,
            include_broadcast: true,
            limit: 10,
            workspace_id: None,
            conversation_id: None,
        },
    )
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
