use super::super::make_server_with_temp_home;
use crate::tool_params::CompactSessionMemoryParams;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

#[tokio::test]
async fn compact_session_memory_persists_rollup_and_signal_entries() {
    let (server, _temp_home) = make_server_with_temp_home();
    let result = server
        .compact_session_memory(Parameters(CompactSessionMemoryParams {
            agent_id: "main".to_string(),
            conversation_id: "conv-1".to_string(),
            window_id: "window-1".to_string(),
            compacted_text: "User prefers Tachi-managed memory and wants Excel-first workflows."
                .to_string(),
            salient_topics: vec!["memory".to_string(), "excel".to_string()],
            durable_signals: vec![
                "User prefers Tachi-managed memory.".to_string(),
                "Excel workflows should start with python plus filesystem.".to_string(),
            ],
            path_prefix: None,
            project: None,
            scope: "project".to_string(),
            importance: 0.7,
            queue_maintenance: false,
        }))
        .await
        .expect("compact_session_memory should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("completed"));
    assert_eq!(json["captured"].as_u64().unwrap_or(0), 3);
    assert!(json["section"]["block"]
        .as_str()
        .unwrap_or("")
        .contains("Durable Session Memory"));
}
