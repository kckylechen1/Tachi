use super::*;

#[tokio::test]
async fn tachi_event_metrics_reports_session_outcome_challenge_rate() {
    let server = make_server();

    for (id, outcome) in [
        ("outcome-ai", "ai_corrected"),
        ("outcome-user", "user_correct"),
        ("outcome-open", "unresolved"),
    ] {
        let mut emit = tachi_event_params("emit");
        emit.id = Some(id.to_string());
        emit.source_repo = Some("sigil".to_string());
        emit.adapter = Some("facade-test".to_string());
        emit.domain = Some("coding".to_string());
        emit.session_id = Some(id.to_string());
        emit.actor = Some("labeler".to_string());
        emit.event_type = Some("session.outcome".to_string());
        emit.authority = Some("review_signal_only".to_string());
        emit.effects = vec!["scoring".to_string()];
        emit.projection_hints = vec!["outcome".to_string()];
        emit.payload = Some(json!({
            "outcome": outcome,
            "evidence_basis": "external_evidence",
        }));

        crate::event_ops::handle_tachi_event(&server, emit)
            .await
            .expect("emit outcome");
    }

    let metrics = crate::event_ops::handle_tachi_event(&server, tachi_event_params("metrics"))
        .await
        .expect("metrics should succeed");
    let parsed: Value = serde_json::from_str(&metrics).expect("metrics JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(
        parsed["metrics"]["session_outcomes"]["outcome_events"],
        json!(3)
    );
    assert_eq!(
        parsed["metrics"]["session_outcomes"]["eligible_outcomes"],
        json!(2)
    );
    assert_eq!(
        parsed["metrics"]["session_outcomes"]["challenge_rate"],
        json!(0.5)
    );
}
