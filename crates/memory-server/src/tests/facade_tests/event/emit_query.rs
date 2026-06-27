use super::*;

#[tokio::test]
async fn tachi_event_emit_and_query_round_trips_continuity_metadata() {
    let server = make_server();

    let mut emit = tachi_event_params("emit");
    emit.id = Some("facade-event-1".to_string());
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("facade-test".to_string());
    emit.domain = Some("architecture".to_string());
    emit.session_id = Some("session-1".to_string());
    emit.actor = Some("agent".to_string());
    emit.event_type = Some("pattern.observed".to_string());
    emit.authority = Some("derived_evidence".to_string());
    emit.effects = vec!["recall".to_string(), "project_cycle".to_string()];
    emit.projection_hints = vec!["pattern".to_string(), "project_cycle".to_string()];
    emit.payload = Some(json!({"summary": "shared continuity event ABI"}));
    emit.provenance = Some(json!({"files": ["crates/memory-server/src/event_ops.rs"]}));
    emit.created_at = Some("2026-06-22T00:00:00Z".to_string());

    let saved = crate::event_ops::handle_tachi_event(&server, emit)
        .await
        .expect("emit should succeed");
    let saved_json: Value = serde_json::from_str(&saved).expect("emit JSON");
    assert_eq!(saved_json["status"], json!("saved"));
    assert_eq!(saved_json["event"]["authority"], json!("derived_evidence"));
    assert_eq!(
        saved_json["event"]["effects"],
        json!(["recall", "project_cycle"])
    );

    let mut query = tachi_event_params("query");
    query.domain = Some("architecture".to_string());
    query.event_type = Some("pattern.observed".to_string());
    query.session_id = Some("session-1".to_string());
    query.limit = 5;

    let listed = crate::event_ops::handle_tachi_event(&server, query)
        .await
        .expect("query should succeed");
    let listed_json: Value = serde_json::from_str(&listed).expect("query JSON");
    assert_eq!(listed_json["status"], json!("completed"));
    assert_eq!(listed_json["count"], json!(1));
    assert_eq!(listed_json["events"][0]["id"], json!("facade-event-1"));
    assert_eq!(
        listed_json["events"][0]["projection_hints"],
        json!(["pattern", "project_cycle"])
    );
}
