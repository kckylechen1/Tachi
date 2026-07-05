use super::*;

#[tokio::test]
async fn auto_ingest_hook_persists_mcp_text_results() {
    let server = make_server();
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "reader output for auto ingest"}],
        "isError": false
    }))
    .expect("build tool result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": "/wiki/general/auto-ingest-test"
    });
    let arguments =
        serde_json::Map::from_iter([("url".to_string(), json!("https://example.com/article"))]);

    crate::pipeline_ops::schedule_auto_ingest_from_mcp(
        &server,
        "mcp:web-reader",
        "webReader",
        &definition,
        Some(&arguments),
        &result,
    );

    for _ in 0..50 {
        let entries = server
            .with_global_store_read(|store| {
                store
                    .list_by_path("/wiki/general/auto-ingest-test", 10, false)
                    .map_err(|e| e.to_string())
            })
            .unwrap_or_default();
        if !entries.is_empty() {
            assert!(
                !entries.is_empty(),
                "auto_ingest hook should persist MCP text results"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("auto_ingest entries not persisted after 500ms polling");
}
