use super::*;

#[tokio::test]
async fn hub_feedback_records_success_and_rating() {
    let server = make_server();

    // Register a supported skill capability first
    let cap = crate::tests::make_skill_capability(
        "skill:feedback-test",
        "feedback-test",
        "test feedback capability",
        "approved",
    );

    server
        .with_global_store(|store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("failed to register capability");

    // Record successful feedback with rating
    let feedback = server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "skill:feedback-test".to_string(),
            success: true,
            rating: Some(4.5),
        }))
        .await
        .expect("hub_feedback should succeed");

    let feedback_json: Value = serde_json::from_str(&feedback).unwrap();
    assert!(feedback_json["recorded"].as_bool().unwrap());
    assert_eq!(feedback_json["id"], "skill:feedback-test");

    // Record failure feedback without rating
    let feedback_fail = server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "skill:feedback-test".to_string(),
            success: false,
            rating: None,
        }))
        .await
        .expect("hub_feedback for failure should succeed");

    let fail_json: Value = serde_json::from_str(&feedback_fail).unwrap();
    assert!(fail_json["recorded"].as_bool().unwrap());
}

#[tokio::test]
async fn hub_feedback_rejects_historical_retired_types() {
    let server = make_server();
    let legacy = crate::tests::make_mcp_capability("mcp:feedback-retired", 1);
    server
        .with_global_store(|store| {
            store
                .hub_register(&legacy)
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("seed legacy mcp row");

    let err = server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "mcp:feedback-retired".to_string(),
            success: true,
            rating: Some(4.0),
        }))
        .await
        .expect_err("retired mcp row must reject feedback");
    assert!(err.contains("retired"), "unexpected error: {err}");
}

#[tokio::test]
async fn hub_feedback_returns_not_recorded_for_missing_capability() {
    let server = make_server();

    let feedback = server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "skill:missing-capability".to_string(),
            success: true,
            rating: Some(7.5),
        }))
        .await
        .expect("hub_feedback should succeed");

    let feedback_json: serde_json::Value =
        serde_json::from_str(&feedback).expect("feedback should be valid JSON");
    assert_eq!(feedback_json["recorded"], json!(false));
    assert_eq!(feedback_json["db"], json!("global"));
}

#[tokio::test]
async fn hub_stats_returns_capability_counts() {
    let server = make_server();

    // Register a supported skill capability
    let cap = crate::tests::make_skill_capability(
        "skill:stats-test",
        "stats-test",
        "test stats capability",
        "approved",
    );

    server
        .with_global_store(|store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("failed to register capability");

    // Get stats
    let stats = server.hub_stats().await.expect("hub_stats should succeed");

    let stats_json: Value = serde_json::from_str(&stats).unwrap();
    assert!(stats_json["total_capabilities"].as_u64().unwrap() >= 1);
    assert!(stats_json["by_type"]["skill"].as_u64().is_some());
}

#[tokio::test]
async fn hub_stats_excludes_retired_global_and_project_tombstones() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("retired-trajectory-stats");
    let before: Value = serde_json::from_str(
        &server
            .hub_stats()
            .await
            .expect("baseline hub_stats should succeed"),
    )
    .expect("baseline stats JSON");
    let mut global = crate::tests::make_skill_capability(
        crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID,
        "trajectory-distiller",
        "Historical global trajectory writer omitted from stats",
        "listed",
    );
    global.uses = 101;
    global.successes = 99;
    let mut project = global.clone();
    project.description = "Historical project trajectory writer omitted from stats".to_string();
    project.uses = 203;
    project.successes = 197;
    server
        .with_global_store(|store| {
            store
                .hub_register(&global)
                .map_err(|error| error.to_string())
        })
        .expect("inject retired global stats row");
    server
        .with_project_store(|store| {
            store
                .hub_register(&project)
                .map_err(|error| error.to_string())
        })
        .expect("inject retired project stats row");

    let after: Value = serde_json::from_str(
        &server
            .hub_stats()
            .await
            .expect("post-injection hub_stats should succeed"),
    )
    .expect("post-injection stats JSON");
    assert_eq!(
        after, before,
        "retired tombstones must not affect Hub stats"
    );
}
