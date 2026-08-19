use super::*;

fn skill_params(action: &str) -> TachiSkillParams {
    TachiSkillParams {
        action: action.to_string(),
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
    }
}

#[tokio::test]
async fn skill_from_pattern_registers_pending_candidate() {
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

    let response =
        crate::hub_ops::handle_skill_from_pattern(&server, &skill_params("from_pattern"))
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

#[tokio::test]
async fn tachi_skill_facade_rejects_retired_actions() {
    let server = make_server();

    let error = server
        .tachi_skill(Parameters(skill_params("from_pattern")))
        .await
        .expect_err("tachi_skill must reject retired from_pattern action");
    assert!(error.contains("Invalid action 'from_pattern'. Use 'discover' or 'run'."));

    let bundle_error = server
        .tachi_skill(Parameters(skill_params("bundle")))
        .await
        .expect_err("tachi_skill must reject retired bundle action");
    assert!(bundle_error.contains("Invalid action 'bundle'. Use 'discover' or 'run'."));
}
