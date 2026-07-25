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

    let response = crate::pipeline_ops::schedule_auto_ingest_from_mcp(
        &server,
        "mcp:web-reader",
        "webReader",
        &definition,
        Some(&arguments),
        &result,
    )
    .await
    .expect("auto ingest must report durable completion")
    .expect("auto ingest enabled with text content");
    let response: Value = serde_json::from_str(&response).expect("auto ingest response");
    assert_eq!(response["status"], "completed");

    let entries = server
        .with_global_store_read(|store| {
            store
                .list_by_path("/wiki/general/auto-ingest-test", 10, false)
                .map_err(|e| e.to_string())
        })
        .expect("read synchronously persisted auto-ingest rows");
    assert_eq!(entries.len(), 1, "auto ingest must finish before returning");
}
