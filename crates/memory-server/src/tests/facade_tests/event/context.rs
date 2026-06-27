use super::*;

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

    let mut pattern = tachi_event_params("emit");
    pattern.id = Some("pattern-context-1".to_string());
    pattern.source_repo = Some("sigil".to_string());
    pattern.adapter = Some("facade-test".to_string());
    pattern.domain = Some("agent_os".to_string());
    pattern.session_id = Some("session-lore".to_string());
    pattern.actor = Some("codex".to_string());
    pattern.event_type = Some("pattern.observed".to_string());
    pattern.authority = Some("collect_only".to_string());
    pattern.projection_hints = vec!["pattern".to_string()];
    pattern.payload = Some(json!({
        "pattern_key": "agent-os-continuity",
        "summary": "Agent OS continuity pattern",
        "text": "Continuity patterns should be returned as first-class context.",
    }));
    crate::event_ops::handle_tachi_event(&server, pattern)
        .await
        .expect("emit pattern");

    let mut project = tachi_event_params("project");
    project.projection_hints = vec![
        "world_book".to_string(),
        "affect".to_string(),
        "pattern".to_string(),
    ];
    let projected = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project lorebook, affect, and pattern");
    let projected_json: Value = serde_json::from_str(&projected).expect("projected JSON");
    assert_eq!(projected_json["projected_count"], json!(3));

    let mut context = tachi_event_params("context");
    context.projection_hints = vec![
        "world_book".to_string(),
        "affect".to_string(),
        "pattern".to_string(),
    ];
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
        parsed["patterns"][0]["projection_key"],
        json!("agent-os-continuity")
    );
    assert_eq!(
        parsed["guardrails"]["a2a"],
        json!("share evidence and open questions, not conclusions")
    );
}
