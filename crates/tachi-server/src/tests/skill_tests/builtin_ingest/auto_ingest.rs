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

#[tokio::test(flavor = "current_thread")]
async fn production_runtime_replays_staged_auto_ingest_exactly_once_after_restart() {
    let temp = tempfile::tempdir().expect("auto-ingest replay tempdir");
    let db_path = temp.path().join("global.db");

    let first_runtime = crate::server_state::MemoryServer::new_with_background_workers_for_test(
        db_path.clone(),
        None,
        false,
    )
    .expect("create staging runtime without detached processing");
    let mut target = make_entry("auto-ingest-replay-target");
    target.path = "/wiki/general/auto-ingest-replay-target".to_string();
    target.text = "durable replay exact once graph reference".to_string();
    target.summary = target.text.clone();
    target.domain = Some("general".to_string());
    target.metadata = json!({"allow_cross_project": true});
    first_runtime
        .with_global_store(|store| store.upsert(&target).map_err(|error| error.to_string()))
        .expect("seed replay graph target");
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "durable replay exact once graph reference"}],
        "isError": false
    }))
    .expect("build staged tool result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": "/wiki/general/auto-ingest-restart"
    });
    crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &first_runtime,
        "mcp:restart-reader",
        "read",
        &definition,
        None,
        &result,
    )
    .expect("stage durable auto-ingest payload")
    .expect("auto-ingest payload is enabled");
    drop(first_runtime);

    let replay_runtime = crate::server_state::MemoryServer::new_with_background_workers_for_test(
        db_path, None, true,
    )
    .expect("create replacement production runtime with workers");
    let mut completed = false;
    for _ in 0..200 {
        let state = replay_runtime
            .with_global_store_read(|store| {
                let memories = store
                    .list_by_path("/wiki/general/auto-ingest-restart", 10, false)
                    .map_err(|error| error.to_string())?;
                let pending = store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())?;
                Ok((memories, pending))
            })
            .expect("read replay state");
        if state.0.len() == 1 && state.1 == 0 {
            completed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        completed,
        "production-owned replay consumer must retire staged work"
    );
    let restaged = crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &replay_runtime,
        "mcp:restart-reader",
        "read",
        &definition,
        None,
        &result,
    )
    .expect("completed logical payload can be offered again");
    assert!(
        restaged.is_none(),
        "terminal durable audit must prevent the same logical job from becoming pending again"
    );

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let (memories, observations, terminal_audits, terminal_claims) = replay_runtime
        .with_global_store_read(|store| {
            let memories = store
                .list_by_path("/wiki/general/auto-ingest-restart", 10, false)
                .map_err(|error| error.to_string())?;
            let observations = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM edge_observations WHERE source_id = ?1",
                    [memories
                        .first()
                        .map(|entry| entry.id.as_str())
                        .unwrap_or("")],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            let terminal_audits = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log \
                     WHERE server_id = 'ingest' AND tool_name = 'auto_ingest_job' AND success = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            let terminal_claims = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job_claim'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((memories, observations, terminal_audits, terminal_claims))
        })
        .expect("read exactly-once replay effects");
    assert_eq!(memories.len(), 1, "replay must persist one logical memory");
    assert_eq!(
        observations, 1,
        "periodic replay must not duplicate the graph observation"
    );
    assert_eq!(
        terminal_audits, 1,
        "logical replay completion is durable once"
    );
    assert_eq!(terminal_claims, 1, "completed lease row remains terminal");
}
