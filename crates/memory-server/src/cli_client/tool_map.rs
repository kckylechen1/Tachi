//! Legacy CLI/MCP tool-name remapping for daemon forwarding.

fn with_action(
    mut args: serde_json::Map<String, serde_json::Value>,
    action: &str,
) -> serde_json::Map<String, serde_json::Value> {
    args.insert("action".into(), serde_json::json!(action));
    args
}

pub(super) fn remap_daemon_tool(
    tool_name: &str,
    args: serde_json::Map<String, serde_json::Value>,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    match tool_name {
        "extract_facts" => ("tachi_memory".into(), with_action(args, "extract_facts")),
        "save_memory" | "remember" => ("tachi_memory".into(), with_action(args, "save")),
        "search_memory" => ("tachi_memory".into(), with_action(args, "search")),
        "get_memory" => ("tachi_memory".into(), with_action(args, "get")),
        "tachi_wiki_search" | "wiki_search" => ("tachi_wiki".into(), with_action(args, "search")),
        "tachi_wiki_write" | "wiki_write" => ("tachi_wiki".into(), with_action(args, "write")),
        other => (other.to_string(), args),
    }
}
