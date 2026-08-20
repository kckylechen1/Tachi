use super::*;

#[tokio::test]
async fn hub_feedback_records_success_and_rating() {
    let server = make_server();

    // Register a capability first
    let cap = HubCapability {
        id: "mcp:feedback-test".to_string(),
        cap_type: "mcp".to_string(),
        name: "feedback-test".to_string(),
        version: 1,
        description: "test feedback capability".to_string(),
        definition: r#"{"transport":"stdio","command":"echo","args":["test"]}"#.to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

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
            id: "mcp:feedback-test".to_string(),
            success: true,
            rating: Some(4.5),
        }))
        .await
        .expect("hub_feedback should succeed");

    let feedback_json: Value = serde_json::from_str(&feedback).unwrap();
    assert!(feedback_json["recorded"].as_bool().unwrap());
    assert_eq!(feedback_json["id"], "mcp:feedback-test");

    // Record failure feedback without rating
    let feedback_fail = server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "mcp:feedback-test".to_string(),
            success: false,
            rating: None,
        }))
        .await
        .expect("hub_feedback for failure should succeed");

    let fail_json: Value = serde_json::from_str(&feedback_fail).unwrap();
    assert!(fail_json["recorded"].as_bool().unwrap());
}

#[tokio::test]
async fn hub_feedback_returns_not_recorded_for_missing_capability() {
    let server = make_server();

    let feedback = server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "mcp:missing-capability".to_string(),
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

    // Register a capability
    let cap = HubCapability {
        id: "mcp:stats-test".to_string(),
        cap_type: "mcp".to_string(),
        name: "stats-test".to_string(),
        version: 1,
        description: "test stats capability".to_string(),
        definition: r#"{"transport":"stdio","command":"echo","args":["test"]}"#.to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

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
    assert!(stats_json["by_type"]["mcp"].as_u64().is_some());
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
    assert_eq!(after, before, "retired tombstones must not affect Hub stats");
}

#[tokio::test]
async fn hub_disconnect_returns_ok_for_nonexistent_server() {
    let server = make_server();

    // Disconnect should succeed even for non-existent server (idempotent)
    let result = server
        .hub_disconnect(Parameters(HubDisconnectParams {
            server_id: "mcp:nonexistent".to_string(),
        }))
        .await;

    // Should not error - disconnect is idempotent
    assert!(
        result.is_ok(),
        "hub_disconnect should not fail for nonexistent server"
    );
}
