use super::*;

/// The Hub MCP proxy is retired, but pre-retirement databases can still hold
/// `mcp:*` capability rows (with or without runtime policy rows). Those
/// zombies must never become callable: the `server__tool` route is gone, so
/// every call hits the exact unknown-tool error — even for the admin profile
/// that used to reach the proxy.
#[tokio::test]
async fn zombie_mcp_capability_rows_are_not_callable() {
    let server = make_server();
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));

    let with_policy = make_mcp_capability("mcp:fs-locked", 1);
    server
        .with_global_store(|store| {
            store
                .hub_register(&with_policy)
                .map_err(|e| format!("register fs-locked failed: {e}"))?;
            store
                .set_sandbox_policy(
                    "mcp:fs-locked",
                    "process",
                    "[]",
                    "[\"/tmp\"]",
                    "[]",
                    "[]",
                    1000,
                    1000,
                    1,
                    true,
                )
                .map_err(|e| format!("seed legacy policy failed: {e}"))
        })
        .expect("seed zombie capability with a legacy runtime policy");

    let without_policy = make_mcp_capability("mcp:unregistered-policy", 1);
    server
        .with_global_store(|store| {
            store
                .hub_register(&without_policy)
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("seed zombie capability without a policy");

    for tool in ["fs-locked__echo", "unregistered-policy__test_tool"] {
        let result = call_tool_on_server(server.clone(), tool, None)
            .await
            .expect("call should yield a tool result");
        assert_eq!(
            result.is_error,
            Some(true),
            "zombie proxied tool '{tool}' must be an error result"
        );
        let message = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.clone())
            .unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "tool not found: '{tool}'. Call tools/list and use an exact visible tool name. \
                 Management operations use supported owner CLI commands."
            ),
            "zombie proxied tool '{tool}' must return the exact unknown-tool error"
        );
    }
}
