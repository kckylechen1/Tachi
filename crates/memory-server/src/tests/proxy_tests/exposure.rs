use super::*;

#[test]
fn filter_mcp_tools_respects_allow_and_deny_permissions() {
    let def = json!({
        "permissions": {
            "allow": ["echo", "add"],
            "deny": ["add"],
        }
    });

    let filtered = filter_mcp_tools_by_permissions(
        &def,
        vec![
            make_test_tool("echo"),
            make_test_tool("add"),
            make_test_tool("secret"),
        ],
    );

    let names: Vec<String> = filtered
        .iter()
        .map(|tool| tool.name.as_ref().to_string())
        .collect();
    assert_eq!(names, vec!["echo"]);
}

#[test]
fn resolve_mcp_tool_exposure_supports_definition_overrides() {
    let flatten = resolve_mcp_tool_exposure(
        &json!({"tool_exposure": "flatten"}),
        McpToolExposureMode::Gateway,
    );
    let gateway = resolve_mcp_tool_exposure(
        &json!({"tool_exposure": "gateway"}),
        McpToolExposureMode::Flatten,
    );
    let expose_false = resolve_mcp_tool_exposure(
        &json!({"expose_tools": false}),
        McpToolExposureMode::Flatten,
    );
    let fallback_default = resolve_mcp_tool_exposure(&json!({}), McpToolExposureMode::Gateway);

    assert_eq!(flatten, McpToolExposureMode::Flatten);
    assert_eq!(gateway, McpToolExposureMode::Gateway);
    assert_eq!(expose_false, McpToolExposureMode::Gateway);
    assert_eq!(fallback_default, McpToolExposureMode::Gateway);
}

#[test]
fn split_proxy_tool_name_preserves_double_underscore_remote_tools() {
    let parsed =
        crate::server_handler::split_proxy_tool_name("server__tool__name", ["server"].into_iter())
            .expect("proxy tool name should parse");
    assert_eq!(parsed, ("server".to_string(), "tool__name".to_string()));
}

#[test]
fn split_proxy_tool_name_prefers_known_longest_server_prefix() {
    let parsed = crate::server_handler::split_proxy_tool_name(
        "server__beta__echo",
        ["server", "server__beta"].into_iter(),
    )
    .expect("proxy tool name should parse");
    assert_eq!(parsed, ("server__beta".to_string(), "echo".to_string()));
}

#[test]
fn split_proxy_tool_name_keeps_legacy_fallback_without_cached_servers() {
    let parsed = crate::server_handler::split_proxy_tool_name("gateway-only__echo", [].into_iter())
        .expect("legacy proxy tool name should parse");
    assert_eq!(parsed, ("gateway-only".to_string(), "echo".to_string()));
}
