use super::*;

fn tachi_event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

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
async fn tachi_event_project_materializes_pattern_idempotently() {
    let server = make_server();

    let mut emit = tachi_event_params("emit");
    emit.id = Some("pattern-observed-1".to_string());
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("facade-test".to_string());
    emit.domain = Some("agent_os".to_string());
    emit.session_id = Some("session-pattern".to_string());
    emit.actor = Some("codex".to_string());
    emit.event_type = Some("pattern.observed".to_string());
    emit.authority = Some("collect_only".to_string());
    emit.projection_hints = vec!["pattern".to_string()];
    emit.payload = Some(json!({
        "pattern_key": "continuity-first",
        "summary": "Continuity-first project management",
        "text": "User treats memory, subagent eval, lorebook, and emotion as one continuity substrate.",
    }));
    crate::event_ops::handle_tachi_event(&server, emit)
        .await
        .expect("emit pattern event");

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    let first = crate::event_ops::handle_tachi_event(&server, project.clone())
        .await
        .expect("project pattern");
    let first_json: Value = serde_json::from_str(&first).expect("project JSON");
    assert_eq!(first_json["status"], json!("completed"));
    assert_eq!(first_json["projected_count"], json!(1));
    assert_eq!(
        first_json["projections"][0]["already_projected"],
        json!(false)
    );
    let memory_id = first_json["projections"][0]["memory_id"]
        .as_str()
        .expect("memory id")
        .to_string();

    let second = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project pattern again");
    let second_json: Value = serde_json::from_str(&second).expect("second project JSON");
    assert_eq!(
        second_json["projections"][0]["already_projected"],
        json!(true)
    );

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read projection")
        .expect("projection entry exists");
    assert!(entry.path.starts_with("/user/patterns/agent_os/"));
    assert_eq!(entry.metadata["projection_kind"], json!("pattern"));
    assert_eq!(entry.metadata["counters"]["seen"], json!(1));
    assert_eq!(
        entry.metadata["projected_event_ids"],
        json!(["pattern-observed-1"])
    );
}

#[tokio::test]
async fn tachi_event_context_returns_lorebook_and_affect_guardrails() {
    let server = make_server();

    let mut lore = tachi_event_params("emit");
    lore.id = Some("lorebook-candidate-1".to_string());
    lore.source_repo = Some("romanbath".to_string());
    lore.adapter = Some("facade-test".to_string());
    lore.domain = Some("agent_os".to_string());
    lore.session_id = Some("session-lore".to_string());
    lore.actor = Some("jayne".to_string());
    lore.event_type = Some("lorebook.candidate".to_string());
    lore.authority = Some("collect_only".to_string());
    lore.projection_hints = vec!["world_book".to_string()];
    lore.payload = Some(json!({
        "lorebook_key": "agent-os-continuity",
        "summary": "Agent OS continuity",
        "content": "Continuity is the shared substrate across coding agents, lorebook, and emotion state.",
        "keys": ["Agent OS", "continuity"],
        "secondary_keys": ["Tachi"],
        "position": "before_char",
        "priority": 20,
        "token_budget": 180,
        "enabled": true,
    }));
    crate::event_ops::handle_tachi_event(&server, lore)
        .await
        .expect("emit lorebook");

    let mut affect = tachi_event_params("emit");
    affect.id = Some("affect-observed-1".to_string());
    affect.source_repo = Some("quant".to_string());
    affect.adapter = Some("facade-test".to_string());
    affect.domain = Some("trading".to_string());
    affect.session_id = Some("session-lore".to_string());
    affect.actor = Some("user".to_string());
    affect.event_type = Some("affect.observed".to_string());
    affect.authority = Some("tone_and_reminder_only".to_string());
    affect.effects = vec!["tone".to_string()];
    affect.projection_hints = vec!["affect".to_string()];
    affect.payload = Some(json!({
        "emotion_key": "user:fomo",
        "summary": "FOMO caution",
        "state": "FOMO",
        "intensity": 0.67,
        "confidence": 0.75,
        "counterweight": "cool_down_before_action",
        "delivery_mode": "firm_caution",
    }));
    crate::event_ops::handle_tachi_event(&server, affect)
        .await
        .expect("emit affect");

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["world_book".to_string(), "affect".to_string()];
    let projected = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project lorebook and affect");
    let projected_json: Value = serde_json::from_str(&projected).expect("projected JSON");
    assert_eq!(projected_json["projected_count"], json!(2));

    let mut context = tachi_event_params("context");
    context.projection_hints = vec!["world_book".to_string(), "affect".to_string()];
    let body = crate::event_ops::handle_tachi_event(&server, context)
        .await
        .expect("context");
    let parsed: Value = serde_json::from_str(&body).expect("context JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(
        parsed["lorebook"][0]["lorebook"]["keys"],
        json!(["Agent OS", "continuity"])
    );
    assert_eq!(
        parsed["affect"][0]["guardrails"]["execution_effect"],
        json!("none")
    );
    assert_eq!(
        parsed["guardrails"]["a2a"],
        json!("share evidence and open questions, not conclusions")
    );
}
