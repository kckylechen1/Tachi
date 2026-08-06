use serde_json::json;
use tachi_params::{HubCallParams, TachiTaskParams};

#[test]
fn hub_call_arguments_schema_and_deserialize_preserve_nested_tool_args() {
    let schema = rmcp::handler::server::tool::schema_for_type::<HubCallParams>();
    assert_eq!(
        schema["properties"]["arguments"]["type"],
        json!("object"),
        "hub_call.arguments must advertise a JSON object so clients keep nested tool args"
    );

    let params: HubCallParams = serde_json::from_value(json!({
        "server_id": "mcp:exa",
        "tool_name": "web_search_exa",
        "arguments": {"query": "rust mcp streamable http", "numResults": 3}
    }))
    .expect("hub_call params should deserialize");

    assert_eq!(
        params.arguments.get("query"),
        Some(&json!("rust mcp streamable http"))
    );
    assert_eq!(params.arguments.get("numResults"), Some(&json!(3)));

    let alias_params: HubCallParams = serde_json::from_value(json!({
        "server_id": "mcp:exa",
        "tool_name": "web_search_exa",
        "args": {"query": "alias preserved"}
    }))
    .expect("hub_call args alias should deserialize");
    assert_eq!(
        alias_params.arguments.get("query"),
        Some(&json!("alias preserved"))
    );
}

#[test]
fn tachi_task_schema_advertises_survivors_not_retired_c1a_actions() {
    let schema = rmcp::handler::server::tool::schema_for_type::<TachiTaskParams>();
    let action_description = schema["properties"]["action"]["description"]
        .as_str()
        .expect("action description");

    assert!(
        action_description.contains("profiles"),
        "tachi_task.action schema must advertise profile discovery: {action_description}"
    );
    assert!(
        action_description.contains("cycle_status"),
        "tachi_task.action schema must advertise lifecycle status: {action_description}"
    );
    for retired in tachi_params::TACHI_TASK_RETIRED_C1A_ACTIONS {
        assert!(
            !action_description.contains(retired),
            "tachi_task.action schema must not advertise retired C1a action {retired}: {action_description}"
        );
    }
}
