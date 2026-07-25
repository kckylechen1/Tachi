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

    let staged = crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &server,
        "mcp:web-reader",
        "webReader",
        &definition,
        Some(&arguments),
        &result,
    )
    .expect("auto ingest staging must succeed")
    .expect("auto ingest enabled with text content");
    let response = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("auto ingest must report durable completion")
        .expect("staged auto ingest returns a response");
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
    let pending_jobs = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("count completed auto-ingest jobs: {error}"))
        })
        .expect("count completed auto-ingest jobs");
    assert_eq!(pending_jobs, 0, "success must retire its durable retry job");
}
