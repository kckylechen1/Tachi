use super::*;
use rmcp::model::Tool;

mod filtering;
mod parsing;

fn test_tool(name: &str) -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": format!("tool {name}"),
        "inputSchema": {
            "type": "object",
            "additionalProperties": true,
        }
    }))
    .expect("test tool")
}
