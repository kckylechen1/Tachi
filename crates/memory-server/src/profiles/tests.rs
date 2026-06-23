use super::*;
use rmcp::model::Tool;

mod filtering;
mod parsing;
mod router_coverage;

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
        std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
        std::env::set_var("SILICONFLOW_MODEL", "test-model");
        std::env::set_var("SUMMARY_MODEL", "test-summary-model");
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
    });
}

fn native_route_names() -> Vec<String> {
    ensure_test_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("profiles test runtime");
    let _guard = runtime.enter();
    let db_path = std::env::temp_dir().join(format!(
        "profiles-metadata-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = crate::MemoryServer::new(db_path, None).expect("test memory server");
    let mut names: Vec<String> = server
        .tool_router
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    names.sort();
    names
}

fn bundle_count(tool_name: &str) -> usize {
    [
        ToolBundle::Observe,
        ToolBundle::Remember,
        ToolBundle::Coordinate,
        ToolBundle::Operate,
    ]
    .into_iter()
    .filter(|bundle| tool_matches_bundle(tool_name, *bundle))
    .count()
}

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
