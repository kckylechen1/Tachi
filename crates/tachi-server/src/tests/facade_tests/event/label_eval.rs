use super::*;

#[tokio::test]
async fn tachi_event_label_eval_compares_outcomes_to_gold_reviews() {
    let server = make_server();

    let mut label = tachi_event_params("emit");
    label.id = Some("label-event-1".to_string());
    label.source_repo = Some("sigil".to_string());
    label.adapter = Some("labeler".to_string());
    label.domain = Some("coding".to_string());
    label.session_id = Some("session-label-eval".to_string());
    label.actor = Some("continuity_labeler".to_string());
    label.event_type = Some("session.outcome".to_string());
    label.authority = Some("review_signal_only".to_string());
    label.effects = vec!["scoring".to_string()];
    label.projection_hints = vec!["outcome".to_string()];
    label.payload = Some(json!({
        "outcome": "partial_reframe",
        "evidence_basis": "external_evidence",
    }));
    crate::event_ops::handle_tachi_event(&server, label)
        .await
        .expect("emit label");

    let mut review = tachi_event_params("emit");
    review.id = Some("label-review-1".to_string());
    review.source_repo = Some("sigil".to_string());
    review.adapter = Some("human-review".to_string());
    review.domain = Some("coding".to_string());
    review.session_id = Some("session-label-eval".to_string());
    review.actor = Some("reviewer".to_string());
    review.event_type = Some("session.outcome.review".to_string());
    review.authority = Some("derived_evidence".to_string());
    review.projection_hints = vec!["evidence_gate".to_string()];
    review.payload = Some(json!({
        "target_event_id": "label-event-1",
        "gold_outcome": "partial_reframe",
        "gold_evidence_basis": "external_evidence",
    }));
    crate::event_ops::handle_tachi_event(&server, review)
        .await
        .expect("emit review");

    let eval = crate::event_ops::handle_tachi_event(&server, tachi_event_params("label_eval"))
        .await
        .expect("label eval");
    let parsed: Value = serde_json::from_str(&eval).expect("label eval JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["reviewed"], json!(1));
    assert_eq!(parsed["missing_targets"], json!(0));
    assert_eq!(parsed["outcome_accuracy"], json!(1.0));
    assert_eq!(parsed["evidence_basis_accuracy"], json!(1.0));
    assert_eq!(parsed["full_match_rate"], json!(1.0));
}

#[tokio::test]
async fn tachi_event_label_eval_runs_heldout_fixture() {
    let server = make_server();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../fixtures/continuity_label_eval_heldout.json"
    ))
    .expect("heldout fixture JSON");
    for event in fixture["events"].as_array().expect("fixture events") {
        let mut emit = tachi_event_params("emit");
        emit.id = event.get("id").and_then(Value::as_str).map(str::to_string);
        emit.source_repo = event
            .get("source_repo")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.adapter = event
            .get("adapter")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.domain = event
            .get("domain")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.session_id = event
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.actor = event
            .get("actor")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.event_type = event
            .get("event_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.authority = event
            .get("authority")
            .and_then(Value::as_str)
            .map(str::to_string);
        emit.effects = event
            .get("effects")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        emit.projection_hints = event
            .get("projection_hints")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        emit.payload = event.get("payload").cloned();
        crate::event_ops::handle_tachi_event(&server, emit)
            .await
            .expect("emit heldout event");
    }

    let eval = crate::event_ops::handle_tachi_event(&server, tachi_event_params("label_eval"))
        .await
        .expect("label eval");
    let parsed: Value = serde_json::from_str(&eval).expect("label eval JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["reviewed"], json!(1));
    assert_eq!(parsed["outcome_accuracy"], json!(1.0));
    assert_eq!(parsed["evidence_basis_accuracy"], json!(1.0));
}
