use super::*;

#[tokio::test]
async fn process_runtime_rejects_unenforceable_fs_roots() {
    let server = make_server();
    let cap = make_mcp_capability("mcp:fs-locked", 1);

    server
        .with_global_store(|store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register fs-locked failed: {e}"))
        })
        .expect("failed to register fs-locked capability");

    server
        .sandbox_set_policy(Parameters(SandboxSetPolicyParams {
            capability_id: "mcp:fs-locked".to_string(),
            runtime_type: "process".to_string(),
            env_allowlist: vec![],
            fs_read_roots: vec!["/tmp".to_string()],
            fs_write_roots: vec![],
            cwd_roots: vec![],
            max_startup_ms: 1000,
            max_tool_ms: 1000,
            max_concurrency: 1,
            enabled: true,
        }))
        .await
        .expect("sandbox_set_policy should succeed");

    let err = server
        .proxy_call_internal("fs-locked", "echo", None)
        .await
        .expect_err("fs root policy should fail closed for process runtime");

    assert!(
        err.to_string().contains("cannot enforce"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn sandbox_policy_prevents_unregistered_capability_startup() {
    let server = make_server();

    // Register a capability without a policy
    let cap = HubCapability {
        id: "mcp:unregistered-policy".to_string(),
        cap_type: "mcp".to_string(),
        name: "unregistered-policy".to_string(),
        version: 1,
        description: "test capability without policy".to_string(),
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

    // Try to call without setting policy - should fail with policy error
    let result = server
        .proxy_call_internal("unregistered-policy", "test_tool", None)
        .await;

    assert!(
        result.is_err(),
        "proxy_call should fail without sandbox policy"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("no sandbox policy") || err_msg.contains("Sandbox"),
        "Error should mention sandbox policy: {}",
        err_msg
    );
}
