use super::*;

#[tokio::test]
async fn tachi_skill_from_pattern_registers_pending_candidate() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut pattern = make_entry("skill-from-pattern-row");
            pattern.path = "/user/patterns/agent_os/continuity-first".to_string();
            pattern.summary = "Continuity-first project management".to_string();
            pattern.text =
                "Use continuity evidence before selecting the next project-management action."
                    .to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "continuity-first",
                "source_event_id": "pattern-event-skill",
                "counters": {"seen": 4, "hit": 2}
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed projected pattern");

    let response = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "from_pattern".to_string(),
            query: Some("continuity-first".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(10),
            skill_id: None,
            args: Some(json!({
                "skill_id": "skill:pattern-continuity-test",
                "name": "continuity-test-pattern",
                "description": "Candidate from continuity pattern"
            })),
            profile: None,
            host: None,
            skill_limit: None,
            capability_limit: None,
            pack_limit: None,
            include_section: None,
        }))
        .await
        .expect("from_pattern should succeed");
    let json: Value = serde_json::from_str(&response).expect("from_pattern JSON");
    assert_eq!(json["status"], json!("candidate_registered"));
    assert_eq!(json["id"], json!("skill:pattern-continuity-test"));
    assert_eq!(json["review_status"], json!("pending"));
    assert_eq!(json["enabled"], json!(false));
    assert_eq!(
        json["pattern_ref"]["projection_key"],
        json!("continuity-first")
    );

    let cap = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:pattern-continuity-test")
                .map_err(|e| e.to_string())
        })
        .expect("read registered candidate")
        .expect("candidate should exist");
    assert!(!cap.enabled);
    assert_eq!(cap.review_status, "pending");
    let definition: Value = serde_json::from_str(&cap.definition).expect("definition JSON");
    assert_eq!(definition["policy"]["visibility"], json!("discoverable"));
    assert_eq!(
        definition["pattern_ref"]["id"],
        json!("skill-from-pattern-row")
    );
}
