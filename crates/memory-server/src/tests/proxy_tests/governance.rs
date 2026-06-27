use super::*;

#[tokio::test]
async fn proxy_call_blocks_disabled_capability_even_directly() {
    let server = make_server();

    let cap = HubCapability {
        id: "mcp:blocked".to_string(),
        cap_type: "mcp".to_string(),
        name: "blocked".to_string(),
        version: 1,
        description: "test disabled server".to_string(),
        definition: r#"{"transport":"stdio","command":"npx","args":[]}"#.to_string(),
        enabled: false,
        review_status: "pending".to_string(),
        health_status: "unknown".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
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

    let err = server
        .proxy_call_internal("blocked", "some_tool", None)
        .await
        .expect_err("disabled MCP capability should be blocked");

    assert!(
        err.to_string().contains("not callable") && err.to_string().contains("enabled=false"),
        "expected governance callable error, got: {}",
        err
    );
}

#[tokio::test]
async fn proxy_call_requires_sandbox_policy_and_records_preflight_denial() {
    let server = make_server();

    let cap = HubCapability {
        id: "mcp:needs-policy".to_string(),
        cap_type: "mcp".to_string(),
        name: "needs-policy".to_string(),
        version: 1,
        description: "test policy requirement".to_string(),
        definition: r#"{"transport":"stdio","command":"npx","args":["-y","dummy-mcp"]}"#
            .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
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

    let err = server
        .proxy_call_internal("needs-policy", "some_tool", None)
        .await
        .expect_err("policy-less capability should be blocked");
    assert!(
        err.to_string().contains("no sandbox policy"),
        "expected missing policy error, got: {err}"
    );

    let audit_resp = server
        .sandbox_exec_audit(Parameters(SandboxExecAuditParams {
            capability_id: Some("mcp:needs-policy".to_string()),
            stage: Some("preflight".to_string()),
            decision: Some("denied".to_string()),
            limit: 10,
        }))
        .await
        .expect("sandbox_exec_audit should succeed");
    let audit_json: serde_json::Value =
        serde_json::from_str(&audit_resp).expect("sandbox_exec_audit should return JSON");
    let items = audit_json["items"]
        .as_array()
        .expect("sandbox_exec_audit should return items array");
    assert!(!items.is_empty(), "expected at least one audit row");
    assert_eq!(items[0]["error_kind"], json!("policy_missing"));
}
