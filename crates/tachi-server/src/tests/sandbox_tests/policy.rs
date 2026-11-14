use super::*;

/// set_policy/get_policy/list_policies were enforced only by the removed Hub
/// MCP proxy runtime; the verb now refuses them with a precise retirement
/// error at the direct entrypoint (the exact-message pin through the MCP
/// choke point lives in `tests/sandbox_fold.rs`).
#[tokio::test]
async fn sandbox_policy_actions_are_retired() {
    let server = make_server();

    for (action, extra) in [
        ("set_policy", json!({"capability_id": "mcp:exa"})),
        ("get_policy", json!({"capability_id": "mcp:exa"})),
        ("list_policies", json!({})),
    ] {
        let error = server
            .tachi_sandbox(Parameters(sandbox_params(action, extra)))
            .await
            .expect_err("retired policy action must be refused");
        assert!(
            error.contains(&format!("action='{action}' is retired")),
            "retirement error must name the action: {error}"
        );
    }
}
