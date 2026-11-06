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
