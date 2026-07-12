use super::*;
use crate::tests::make_mcp_capability;
use crate::tool_params::SandboxSetPolicyParams;
use rmcp::handler::server::wrapper::Parameters;

/// #995 residual 2: `connect_child_with_context` (the TOCTOU-safe secondary
/// callable gate reached after `ensure_child_connected_with_context`) must
/// name the failing gate too, not just `proxy_call_capability_internal`'s
/// earlier check. A sandbox policy must exist first so the deny reaches the
/// callable check instead of failing earlier on "no sandbox policy".
#[tokio::test]
async fn connect_deny_message_names_failing_gate() {
    let server = make_server();
    let mut cap = make_mcp_capability("mcp:connect-disabled", 1);
    cap.enabled = false;

    server
        .with_global_store(|store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("failed to register capability");

    server
        .sandbox_set_policy(Parameters(SandboxSetPolicyParams {
            capability_id: "mcp:connect-disabled".to_string(),
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
        .expect("sandbox_set_policy should succeed");

    let err = server
        .connect_child_with_context("mcp:connect-disabled", None)
        .await
        .expect_err("disabled capability should be denied at connect time");

    assert!(
        err.to_string().contains("not callable") && err.to_string().contains("enabled=false"),
        "expected connect-time callable error, got: {}",
        err
    );
    assert!(
        err.to_string().contains("failing gate: enabled=false"),
        "expected connect-time deny message to name the failing gate, got: {}",
        err
    );
}
