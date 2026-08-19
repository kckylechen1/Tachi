use super::*;

#[tokio::test]
async fn tachi_skill_discover_defaults_to_callable_approved_skills() {
    let server = make_server();

    let mut pending = make_skill_capability(
        "skill:debug-pending",
        "debug-pending",
        "Debug workflow that still needs review",
        "standard",
    );
    pending.review_status = "pending".to_string();
    let mut unhealthy = make_skill_capability(
        "skill:debug-unhealthy",
        "debug-unhealthy",
        "Debug workflow with failing health",
        "standard",
    );
    unhealthy.health_status = "unhealthy".to_string();

    server
        .with_global_store(|store| {
            store
                .hub_register(&make_skill_capability(
                    "skill:debug-approved",
                    "debug-approved",
                    "Approved debug workflow",
                    "standard",
                ))
                .map_err(|e| format!("register approved failed: {e}"))?;
            store
                .hub_register(&pending)
                .map_err(|e| format!("register pending failed: {e}"))?;
            store
                .hub_register(&unhealthy)
                .map_err(|e| format!("register unhealthy failed: {e}"))?;
            Ok::<_, String>(())
        })
        .expect("failed to register skills");

    let response = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("debug workflow".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(10),
            skill_id: None,
            args: None,
        }))
        .await
        .expect("tachi_skill discover should succeed");

    let json: Value = serde_json::from_str(&response).expect("skill discover response json");
    let ids = json["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
        .collect::<Vec<_>>();

    assert!(ids.contains(&"skill:debug-approved"), "{json}");
    assert!(!ids.contains(&"skill:debug-pending"), "{json}");
    assert!(!ids.contains(&"skill:debug-unhealthy"), "{json}");
    assert!(json["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| { item.get("callable").and_then(|value| value.as_bool()) == Some(true) }));
    let approved = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("id").and_then(|id| id.as_str()) == Some("skill:debug-approved"))
        .expect("approved skill result should be present");
    assert_eq!(
        approved.get("source").and_then(|value| value.as_str()),
        Some("local_approved_cache")
    );
}
