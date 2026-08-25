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

#[tokio::test]
async fn retired_trajectory_distiller_is_absent_from_hub_and_skill_discovery() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("retired-trajectory-discover");
    let global = make_skill_capability(
        crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID,
        "trajectory-distiller",
        "Historical global trajectory writer",
        "listed",
    );
    let mut project = global.clone();
    project.description = "Historical project trajectory writer".to_string();
    server
        .with_global_store(|store| {
            store
                .hub_register(&global)
                .map_err(|error| error.to_string())
        })
        .expect("inject historical global row");
    server
        .with_project_store(|store| {
            store
                .hub_register(&project)
                .map_err(|error| error.to_string())
        })
        .expect("inject historical project row");

    let hub_raw = crate::hub_ops::handle_hub_discover(
        &server,
        crate::tool_params::HubDiscoverParams {
            query: Some("trajectory-distiller".to_string()),
            cap_type: Some("skill".to_string()),
            enabled_only: false,
        },
    )
    .await
    .expect("hub discovery should succeed");
    let hub_results: Vec<Value> = serde_json::from_str(&hub_raw).expect("hub discover JSON");
    assert!(hub_results.iter().all(|cap| {
        cap.get("id").and_then(Value::as_str)
            != Some(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
    }));

    let skill_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("trajectory-distiller".to_string()),
            cap_type: Some("skill".to_string()),
            enabled_only: Some(false),
            limit: Some(20),
            skill_id: None,
            args: None,
        }))
        .await
        .expect("tachi_skill discover should succeed");
    let skill_results: Value = serde_json::from_str(&skill_raw).expect("skill discover JSON");
    assert!(skill_results["results"]
        .as_array()
        .expect("skill results")
        .iter()
        .all(|cap| {
            cap.get("id").and_then(Value::as_str)
                != Some(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
        }));
}

#[tokio::test]
async fn hub_get_projects_historical_trajectory_distiller_as_immutable_retired_state() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("retired-trajectory-get");
    let legacy = make_skill_capability(
        crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID,
        "trajectory-distiller",
        "Historical approved and enabled project trajectory writer",
        "listed",
    );
    assert!(legacy.enabled);
    assert_eq!(legacy.review_status, "approved");
    server
        .with_project_store(|store| {
            store
                .hub_register(&legacy)
                .map_err(|error| error.to_string())
        })
        .expect("inject approved historical project row");

    let raw = crate::hub_ops::handle_hub_get(
        &server,
        crate::tool_params::HubGetParams {
            id: crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string(),
        },
    )
    .await
    .expect("retired tombstone lookup should remain audit-visible");
    let response: Value = serde_json::from_str(&raw).expect("hub_get JSON");

    assert_eq!(
        response["id"],
        json!(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
    );
    assert_eq!(response["db"], json!("project"));
    assert_eq!(response["enabled"], json!(false));
    assert_eq!(response["review_status"], json!("rejected"));
    assert_eq!(response["callable"], json!(false));
}
