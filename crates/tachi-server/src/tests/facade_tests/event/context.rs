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

    let mut bonding = tachi_event_params("emit");
    bonding.id = Some("bonding-context-1".to_string());
    bonding.source_repo = Some("sigil".to_string());
    bonding.adapter = Some("facade-test".to_string());
    bonding.domain = Some("agent_os".to_string());
    bonding.session_id = Some("session-lore".to_string());
    bonding.actor = Some("codex".to_string());
    bonding.event_type = Some("bonding.observed".to_string());
    bonding.authority = Some("collect_only".to_string());
    bonding.projection_hints = vec!["bonding".to_string()];
    bonding.payload = Some(json!({
        "bonding_key": "bonding-carrier",
        "summary": "Bonding is shared protocol",
        "meaning": "Bonding means shared shorthand and context, not generic warmth.",
        "origin_context": "user corrected bonding vs warmth",
        "shorthand_triggers": ["bonding", "carrier"],
        "appropriate_contexts": ["continuity design"],
        "inappropriate_contexts": ["fact override"],
    }));
    crate::event_ops::handle_tachi_event(&server, bonding)
        .await
        .expect("emit bonding");

    let mut timeline = tachi_event_params("emit");
    timeline.id = Some("timeline-context-1".to_string());
    timeline.source_repo = Some("sigil".to_string());
    timeline.adapter = Some("facade-test".to_string());
    timeline.domain = Some("agent_os".to_string());
    timeline.session_id = Some("session-lore".to_string());
    timeline.actor = Some("codex".to_string());
    timeline.event_type = Some("timeline.candidate".to_string());
    timeline.authority = Some("collect_only".to_string());
    timeline.projection_hints = vec!["timeline".to_string()];
    timeline.payload = Some(json!({
        "projection_key": "continuity-evolution",
        "summary": "Continuity architecture evolved from recall into lifecycle memory",
        "discoveries": ["pattern memory needs timeline evidence"],
        "decisions": ["keep affect tone-only"],
        "open_threads": ["cold-seat A2A transport"],
        "causal_edges": [{"from": "bonding correction", "to": "shared protocol model"}],
    }));
    crate::event_ops::handle_tachi_event(&server, timeline)
        .await
        .expect("emit timeline");

    let mut project = tachi_event_params("project");
    project.projection_hints = vec![
        "world_book".to_string(),
        "affect".to_string(),
        "pattern".to_string(),
        "bonding".to_string(),
        "timeline".to_string(),
    ];
    let projected = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project lorebook, affect, and pattern");
    let projected_json: Value = serde_json::from_str(&projected).expect("projected JSON");
    assert_eq!(projected_json["projected_count"], json!(5));

    let mut context = tachi_event_params("context");
    context.projection_hints = vec![
        "world_book".to_string(),
        "affect".to_string(),
        "pattern".to_string(),
        "bonding".to_string(),
        "timeline".to_string(),
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
        parsed["affect"][0]["affect"]["signals"]["known_markers"][0],
        json!("fomo")
    );
    assert_eq!(
        parsed["patterns"][0]["projection_key"],
        json!("agent-os-continuity")
    );
    assert_eq!(
        parsed["patterns"][0]["pattern_ref"]["projection_key"],
        json!("agent-os-continuity")
    );
    assert_eq!(
        parsed["pattern_refs"][0]["projection_key"],
        json!("agent-os-continuity")
    );
    assert_eq!(
        parsed["bonding"][0]["lexicon"]["meaning"],
        json!("Bonding means shared shorthand and context, not generic warmth.")
    );
    assert_eq!(
        parsed["bonding"][0]["schema"]["name"],
        json!("SharedLexicon")
    );
    assert_eq!(
        parsed["bonding"][0]["lexicon"]["shorthand_triggers"],
        json!(["bonding", "carrier"])
    );
    assert_eq!(
        parsed["timeline"][0]["timeline"]["discoveries"],
        json!(["pattern memory needs timeline evidence"])
    );
    assert_eq!(
        parsed["timeline"][0]["schema"]["name"],
        json!("TimelineEntry")
    );
    assert_eq!(
        parsed["timeline"][0]["schema"]["status"],
        json!("validated")
    );
    assert_eq!(
        parsed["timeline"][0]["timeline"]["causal_edges"][0]["to"],
        json!("shared protocol model")
    );
    assert_eq!(parsed["a2a"]["transport"], json!("not_configured"));
    assert_eq!(
        parsed["a2a"]["cold_seat"]["ingest_conclusions"],
        json!(false)
    );
    assert_eq!(
        parsed["a2a"]["open_threads"][0]["thread"],
        json!("cold-seat A2A transport")
    );
    assert!(parsed["a2a"]["event_refs"]
        .as_array()
        .expect("event refs array")
        .iter()
        .all(|event_ref| event_ref.get("payload").is_none()));
    assert_eq!(
        parsed["host_lifecycle"]["schema"]["name"],
        json!("HostContinuityLifecycle")
    );
    assert_eq!(parsed["host_lifecycle"]["status"], json!("v1_contract"));
    assert!(parsed["host_lifecycle"]["steps"]
        .as_array()
        .expect("lifecycle steps")
        .iter()
        .any(|step| step["phase"] == json!("before_stop")));
    assert_eq!(
        parsed["host_lifecycle"]["event_envelope"]["event_type_prefix"],
        json!("host.")
    );
    assert_eq!(parsed["feedback"]["saved_count"], json!(2));
    assert_eq!(
        parsed["guardrails"]["a2a"],
        json!("share evidence and open questions, not conclusions")
    );

    let mut a2a = tachi_event_params("a2a");
    a2a.projection_hints = vec![
        "pattern".to_string(),
        "bonding".to_string(),
        "timeline".to_string(),
    ];
    let a2a_body = crate::event_ops::handle_tachi_event(&server, a2a)
        .await
        .expect("a2a");
    let a2a_json: Value = serde_json::from_str(&a2a_body).expect("a2a JSON");
    assert_eq!(a2a_json["action"], json!("a2a"));
    assert_eq!(
        a2a_json["a2a"]["cold_seat"]["ingest_conclusions"],
        json!(false)
    );
    assert!(a2a_json.get("feedback").is_none());
}

/// tachi#1561 (L4): `tachi_event(action='context')` emits its `memories`
/// array verbatim — the `is_projection` predicates further down only build the
/// extra typed arrays and never gate the dump — while `path_prefix` is
/// caller-controlled. So a context call scoped anywhere outside the projection
/// namespaces used to walk raw rows, bodies included, straight into the
/// response. Discriminator: on the pre-fix code the internal row and its body
/// are both present.
#[tokio::test]
async fn tachi_event_context_does_not_dump_internal_rows_for_an_arbitrary_prefix() {
    let server = make_server();

    server
        .with_global_store(|store| {
            let mut cache = make_entry("foundry:recall-cache:context-leak");
            cache.path = "/scratch/leak/recall-cache/context".to_string();
            cache.text = "internal recall-cache body that must never reach a reader".to_string();
            cache.summary = "internal recall-cache row".to_string();
            cache.topic = "recall_rerank_cache".to_string();
            cache.source = memcore::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
            cache.metadata = json!({ "cache_key": memcore::FOUNDRY_RECALL_CACHE_SOURCE });
            store.upsert(&cache).map_err(|e| e.to_string())?;

            let mut ordinary = make_entry("context-ordinary");
            ordinary.path = "/scratch/leak/ordinary".to_string();
            ordinary.text = "ordinary user-facing body".to_string();
            store.upsert(&ordinary).map_err(|e| e.to_string())
        })
        .expect("seed context leak fixtures");

    let mut context = tachi_event_params("context");
    context.path_prefix = Some("/scratch/leak".to_string());
    let body = crate::event_ops::handle_tachi_event(&server, context)
        .await
        .expect("context with an explicit path_prefix");
    let parsed: Value = serde_json::from_str(&body).expect("context JSON");
    let memories = parsed["memories"]
        .as_array()
        .cloned()
        .expect("memories array");

    assert!(
        memories
            .iter()
            .any(|row| row["id"] == json!("context-ordinary")),
        "an ordinary row under the requested prefix must still be returned: {body}"
    );
    assert!(
        !memories
            .iter()
            .any(|row| row["id"] == json!("foundry:recall-cache:context-leak")),
        "internal row leaked through tachi_event context: {body}"
    );
    assert!(
        !body.contains("must never reach a reader"),
        "internal row body leaked through tachi_event context: {body}"
    );
}
