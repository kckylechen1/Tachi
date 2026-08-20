use super::*;

#[tokio::test]
async fn prepare_capability_bundle_returns_primary_skill_and_section() {
    let server = make_server();
    let excel = make_skill_capability(
        "skill:excel-automation",
        "excel-automation",
        "Build spreadsheet workflows and Excel reports from CSV data.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&excel).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed bundle registry");

    let result = crate::capability_ops::handle_prepare_capability_bundle(
        &server,
        PrepareCapabilityBundleParams {
            query: "build an excel spreadsheet from csv exports".to_string(),
            host: Some("codex".to_string()),
            skill_limit: 3,
            capability_limit: 3,
            include_section: true,
        },
    )
    .await
    .expect("prepare_capability_bundle should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(
        json["bundle"]["primary_skill"]["id"],
        json!("skill:excel-automation")
    );
    let host_tools = json["bundle"]["host_tools"]
        .as_array()
        .expect("host_tools array")
        .iter()
        .filter_map(|row| row.as_str())
        .collect::<Vec<_>>();
    assert!(host_tools.contains(&"python"));
    assert!(json["bundle"]["section"]["block"]
        .as_str()
        .unwrap_or("")
        .contains("Capability Bundle"));
}

#[tokio::test]
async fn retired_project_trajectory_distiller_is_absent_from_recommendations_and_bundle() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("retired-trajectory-recommendation");
    let retired = make_skill_capability(
        crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID,
        "trajectory-distiller",
        "Distill execution trajectory traces into reusable skill documents",
        "listed",
    );
    assert!(
        tachi_hub::capability_callable(&retired),
        "fixture must reproduce the historical callable project-row bypass"
    );
    let fallback = make_skill_capability(
        "skill:execution-trace-review",
        "execution-trace-review",
        "Review execution trajectory traces without generating new skills",
        "listed",
    );
    server
        .with_project_store(|store| {
            store
                .hub_register(&retired)
                .map_err(|error| error.to_string())
        })
        .expect("inject callable historical project trajectory row");
    server
        .with_global_store(|store| {
            store
                .hub_register(&fallback)
                .map_err(|error| error.to_string())
        })
        .expect("register allowed recommendation fallback");
    let query = "distill execution trajectory traces";

    let recommendations = crate::capability_ops::recommend_capabilities_inner(
        &server,
        query,
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend capabilities");
    assert!(
        recommendations
            .iter()
            .all(|cap| cap.id != crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID),
        "retired trajectory-distiller must be filtered before ranking: {recommendations:?}"
    );

    let result = crate::capability_ops::handle_prepare_capability_bundle(
        &server,
        PrepareCapabilityBundleParams {
            query: query.to_string(),
            host: Some("codex".to_string()),
            skill_limit: 3,
            capability_limit: 3,
            include_section: true,
        },
    )
    .await
    .expect("prepare capability bundle");
    let bundle: Value = serde_json::from_str(&result).expect("bundle JSON");
    assert_ne!(
        bundle["bundle"]["primary_skill"]["id"],
        json!(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
    );
    assert!(bundle["bundle"]["supporting_capabilities"]
        .as_array()
        .expect("supporting capabilities")
        .iter()
        .all(|cap| {
            cap["id"].as_str() != Some(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
        }));
}
