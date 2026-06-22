use super::{make_mcp_capability, make_server};
use crate::tool_params::{
    SandboxSetPolicyParams, VirtualCapabilityBindParams, VirtualCapabilityRegisterParams,
    VirtualCapabilityResolveParams,
};
use memory_core::HubCapability;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

#[tokio::test]
async fn vc_resolve_prefers_first_callable_binding_and_inherits_policy() {
    let server = make_server();
    let exa = make_mcp_capability("mcp:exa", 7);

    server
        .with_global_store(|store| {
            store
                .hub_register(&exa)
                .map_err(|e| format!("register exa failed: {e}"))
        })
        .expect("failed to register exa");

    let vc_register = server
        .vc_register(Parameters(VirtualCapabilityRegisterParams {
            id: "vc:web_search".to_string(),
            name: "web_search".to_string(),
            description: "logical web search".to_string(),
            contract: "web_search".to_string(),
            routing_strategy: "priority".to_string(),
            tags: vec!["search".to_string(), "web".to_string()],
            input_schema: None,
            scope: "global".to_string(),
        }))
        .await
        .expect("vc_register should succeed");
    let vc_register_json: serde_json::Value =
        serde_json::from_str(&vc_register).expect("vc_register response should be JSON");
    assert_eq!(vc_register_json["registered"], json!(true));

    server
        .sandbox_set_policy(Parameters(SandboxSetPolicyParams {
            capability_id: "vc:web_search".to_string(),
            runtime_type: "process".to_string(),
            env_allowlist: vec![],
            fs_read_roots: vec![],
            fs_write_roots: vec![],
            cwd_roots: vec![],
            max_startup_ms: 1500,
            max_tool_ms: 1500,
            max_concurrency: 1,
            enabled: true,
        }))
        .await
        .expect("sandbox_set_policy for vc should succeed");

    let bind = server
        .vc_bind(Parameters(VirtualCapabilityBindParams {
            vc_id: "vc:web_search".to_string(),
            capability_id: "mcp:exa".to_string(),
            priority: 10,
            version_pin: Some(7),
            enabled: true,
            metadata: Some(json!({"provider": "exa"})),
        }))
        .await
        .expect("vc_bind should succeed");
    let bind_json: serde_json::Value =
        serde_json::from_str(&bind).expect("vc_bind response should be JSON");
    assert_eq!(bind_json["updated"], json!(true));

    let resolved = server
        .vc_resolve(Parameters(VirtualCapabilityResolveParams {
            id: "vc:web_search".to_string(),
        }))
        .await
        .expect("vc_resolve should succeed");
    let resolved_json: serde_json::Value =
        serde_json::from_str(&resolved).expect("vc_resolve response should be JSON");
    assert_eq!(resolved_json["resolved_id"], json!("mcp:exa"));

    let (policy, source) = server.get_effective_sandbox_policy(Some("vc:web_search"), "mcp:exa");
    assert!(
        policy.is_some(),
        "virtual capability policy should be inherited"
    );
    assert_eq!(source, "vc:web_search");
}

#[tokio::test]
async fn vc_resolve_skips_version_mismatch_and_uses_next_binding() {
    let server = make_server();
    let exa = make_mcp_capability("mcp:exa", 2);
    let context7 = make_mcp_capability("mcp:context7", 3);

    server
        .with_global_store(|store| {
            store
                .hub_register(&exa)
                .and_then(|_| store.hub_register(&context7))
                .map_err(|e| format!("register vc targets failed: {e}"))
        })
        .expect("failed to register vc targets");

    server
        .vc_register(Parameters(VirtualCapabilityRegisterParams {
            id: "vc:docs_search".to_string(),
            name: "docs_search".to_string(),
            description: "logical docs search".to_string(),
            contract: "docs_search".to_string(),
            routing_strategy: "priority".to_string(),
            tags: vec!["docs".to_string()],
            input_schema: None,
            scope: "global".to_string(),
        }))
        .await
        .expect("vc_register should succeed");

    server
        .vc_bind(Parameters(VirtualCapabilityBindParams {
            vc_id: "vc:docs_search".to_string(),
            capability_id: "mcp:exa".to_string(),
            priority: 10,
            version_pin: Some(9),
            enabled: true,
            metadata: None,
        }))
        .await
        .expect("bind exa should succeed");

    server
        .vc_bind(Parameters(VirtualCapabilityBindParams {
            vc_id: "vc:docs_search".to_string(),
            capability_id: "mcp:context7".to_string(),
            priority: 20,
            version_pin: None,
            enabled: true,
            metadata: None,
        }))
        .await
        .expect("bind context7 should succeed");

    let resolved = server
        .vc_resolve(Parameters(VirtualCapabilityResolveParams {
            id: "vc:docs_search".to_string(),
        }))
        .await
        .expect("vc_resolve should succeed");
    let resolved_json: serde_json::Value =
        serde_json::from_str(&resolved).expect("vc_resolve response should be JSON");
    assert_eq!(resolved_json["resolved_id"], json!("mcp:context7"));

    let candidates = resolved_json["report"]["candidates"]
        .as_array()
        .expect("candidates should be array");
    assert_eq!(candidates[0]["status"], json!("version_pin_mismatch"));
    assert_eq!(candidates[1]["selected"], json!(true));
}

#[tokio::test]
async fn vc_register_and_bind_workflow() {
    let server = make_server();

    // First register a concrete capability
    let cap = HubCapability {
        id: "mcp:concrete".to_string(),
        cap_type: "mcp".to_string(),
        name: "concrete".to_string(),
        version: 1,
        description: "concrete capability".to_string(),
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

    // Register virtual capability
    let vc_reg = server
        .vc_register(Parameters(VirtualCapabilityRegisterParams {
            id: "vc:test".to_string(),
            name: "Test Virtual Capability".to_string(),
            description: "Virtual capability for testing".to_string(),
            contract: "test".to_string(),
            routing_strategy: "priority".to_string(),
            input_schema: None,
            tags: vec!["test".to_string()],
            scope: "project".to_string(),
        }))
        .await
        .expect("vc_register should succeed");

    let vc_json: Value = serde_json::from_str(&vc_reg).unwrap();
    assert_eq!(vc_json["id"], "vc:test");

    // Bind virtual to concrete
    let bind = server
        .vc_bind(Parameters(VirtualCapabilityBindParams {
            vc_id: "vc:test".to_string(),
            capability_id: "mcp:concrete".to_string(),
            priority: 100,
            enabled: true,
            version_pin: None,
            metadata: None,
        }))
        .await
        .expect("vc_bind should succeed");

    let bind_json: Value = serde_json::from_str(&bind).unwrap();
    assert!(bind_json["updated"].as_bool().unwrap());

    // Resolve should return the concrete capability
    let resolve = server
        .vc_resolve(Parameters(VirtualCapabilityResolveParams {
            id: "vc:test".to_string(),
        }))
        .await
        .expect("vc_resolve should succeed");

    let resolve_json: Value = serde_json::from_str(&resolve).unwrap();
    assert_eq!(resolve_json["resolved_id"], "mcp:concrete");
}
