use rusqlite::OptionalExtension;

use super::*;

async fn spawn_admitted_ingest_embedding_provider() -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use axum::response::IntoResponse;

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let response_calls = std::sync::Arc::clone(&calls);
    let app = axum::Router::new()
        .route(
            "/v1/embeddings",
            axum::routing::post(move || {
                let response_calls = std::sync::Arc::clone(&response_calls);
                async move {
                    response_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut embedding = vec![0.0_f32; 1024];
                    embedding[0] = 1.0;
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(json!({
                            "data": [{"index": 0, "embedding": embedding}]
                        })),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/chat/completions",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "{\"keywords\":[\"durable-enrichment\"],\"entities\":[]}"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind admitted-ingest embedding provider");
    let address = listener.local_addr().expect("mock provider address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve admitted-ingest embedding provider");
    });
    (format!("http://{address}"), calls, task)
}

#[derive(Clone)]
struct PausedEmbeddingProvider {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    started: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

async fn spawn_paused_admitted_embedding_provider(
) -> (String, PausedEmbeddingProvider, tokio::task::JoinHandle<()>) {
    let provider = PausedEmbeddingProvider {
        calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        started: std::sync::Arc::new(tokio::sync::Notify::new()),
        release: std::sync::Arc::new(tokio::sync::Notify::new()),
    };
    let app = axum::Router::new()
        .route(
            "/v1/embeddings",
            axum::routing::post({
                let provider = provider.clone();
                move || {
                    let provider = provider.clone();
                    async move {
                        provider
                            .calls
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        provider.started.notify_waiters();
                        provider.release.notified().await;
                        let mut embedding = vec![0.0_f32; 1024];
                        embedding[0] = 1.0;
                        axum::Json(json!({
                            "data": [{"index": 0, "embedding": embedding}]
                        }))
                    }
                }
            }),
        )
        .route(
            "/chat/completions",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "{\"keywords\":[\"lease-enrichment\"],\"entities\":[]}"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind paused admitted embedding provider");
    let address = listener.local_addr().expect("paused provider address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve paused admitted embedding provider");
    });
    (format!("http://{address}"), provider, task)
}

fn staged_auto_ingest_fixture(
    server: &crate::server_state::MemoryServer,
    text: &str,
    path: &str,
) -> crate::pipeline_ops::StagedAutoIngest {
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    }))
    .expect("build staged tool result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": path,
    });
    crate::pipeline_ops::stage_auto_ingest_from_mcp(
        server,
        "mcp:admitted-reader",
        "read",
        &definition,
        None,
        &result,
    )
    .expect("stage admitted MCP result")
    .expect("auto-ingest definition admits text")
}

async fn complete_admitted_enrichment_stage(
    server: &crate::server_state::MemoryServer,
    staged: &crate::pipeline_ops::StagedAutoIngest,
) {
    let pending = crate::pipeline_ops::run_staged_auto_ingest(server, staged.clone())
        .await
        .expect("admitted enrichment stage remains replayable")
        .expect("pending enrichment accounting is returned");
    let pending: Value = serde_json::from_str(&pending).expect("pending enrichment JSON");
    assert_eq!(pending["status"], "partial");
    assert_eq!(pending["failed_stage"], "enrichment_pending");
    assert!(
        server.complete_retained_admitted_enrichments_for_test() > 0,
        "the retained consumer must turn durable intents into terminal entry evidence"
    );
}

fn ingest_effect_counts(
    server: &crate::server_state::MemoryServer,
    path: &str,
) -> (usize, i64, i64) {
    server
        .with_global_store_read(|store| {
            let memories = store
                .list_by_path(path, 4096, false)
                .map_err(|error| error.to_string())?
                .len();
            let edges = store
                .connection()
                .query_row("SELECT COUNT(*) FROM memory_edges", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|error| error.to_string())?;
            let completions = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log \
                     WHERE server_id = 'ingest' AND success = 1 \
                       AND tool_name IN ('ingest_source', 'auto_ingest_job')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((memories, edges, completions))
        })
        .expect("read admitted-ingest effects")
}

fn update_staged_payload(
    server: &crate::server_state::MemoryServer,
    job_id: &str,
    transform: impl FnOnce(&mut Value),
) {
    server
        .with_global_store(|store| {
            let payload = store
                .connection()
                .query_row(
                    "SELECT event_id FROM processed_events \
                     WHERE event_hash = ?1 AND worker = 'auto_ingest_job'",
                    [job_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| error.to_string())?;
            let mut payload: Value =
                serde_json::from_str(&payload).map_err(|error| error.to_string())?;
            transform(&mut payload);
            store
                .connection()
                .execute(
                    "UPDATE processed_events SET event_id = ?1 \
                     WHERE event_hash = ?2 AND worker = 'auto_ingest_job'",
                    rusqlite::params![payload.to_string(), job_id],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .expect("mutate durable staged payload fixture");
}

#[tokio::test]
async fn admitted_auto_ingest_rejects_oversized_payload_before_mutation() {
    let server = make_server();
    let path = "/wiki/general/admitted-too-large";
    let oversized_result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{
            "type": "text",
            "text": "x".repeat(2 * 1024 * 1024 + 1)
        }],
        "isError": false
    }))
    .expect("build oversized tool result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": path,
    });

    let error = crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &server,
        "mcp:admitted-reader",
        "read",
        &definition,
        None,
        &oversized_result,
    )
    .expect_err("payload beyond the admitted 2 MiB bound must refuse before staging");
    assert!(
        error.contains("payload"),
        "typed refusal names payload: {error}"
    );
    assert_eq!(
        ingest_effect_counts(&server, path),
        (0, 0, 0),
        "admission refusal happens before memory, edge, or completion effects"
    );
}

#[tokio::test]
async fn admitted_auto_ingest_revalidates_the_staged_job_at_mutation_time() {
    let server = make_server();
    let path = "/wiki/general/admitted-tamper";
    let staged = staged_auto_ingest_fixture(&server, "original durable MCP text", path);
    update_staged_payload(&server, &staged.job_id, |payload| {
        payload["request"]["content"] = json!("tampered")
    });

    let error = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect_err("mutation-time content drift must conflict");
    assert!(
        error.contains("drift"),
        "typed conflict names drift: {error}"
    );
    assert_eq!(
        ingest_effect_counts(&server, path),
        (0, 0, 0),
        "staged-job drift refuses before all ingest effects"
    );
}

#[tokio::test]
async fn admitted_auto_ingest_rejects_chunk_policy_drift_before_mutation() {
    let server = make_server();
    let path = "/wiki/general/admitted-policy-drift";
    let staged = staged_auto_ingest_fixture(&server, "policy drift fixture", path);
    update_staged_payload(&server, &staged.job_id, |payload| {
        payload["request"]["chunk_size_chars"] = json!(8193)
    });

    let error = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect_err("mutation-time policy drift must conflict");
    assert!(
        error.contains("drift"),
        "typed conflict names drift: {error}"
    );
    assert_eq!(ingest_effect_counts(&server, path), (0, 0, 0));
}

#[tokio::test]
async fn admitted_auto_ingest_enrichment_failure_is_partial_and_replayable() {
    let server = make_server();
    let path = "/wiki/general/admitted-enrichment-partial";
    let staged = staged_auto_ingest_fixture(&server, "enrichment must be accounted", path);
    server.close_enrichment_channel_for_test();

    let first = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("a recoverable admitted stage failure returns typed accounting")
        .expect("staged ingest returns a partial receipt");
    let first: Value = serde_json::from_str(&first).expect("partial receipt JSON");
    assert_eq!(first["status"], "partial");
    assert_eq!(first["failed_stage"], "enrichment_enqueue");
    assert_eq!(first["chunks_saved"], 1);

    let pending = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count replayable staged job");
    assert_eq!(pending, 1, "partial never retires its recovery authority");

    let replay = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("partial replay remains admitted")
        .expect("partial replay returns accounting");
    let replay: Value = serde_json::from_str(&replay).expect("replay receipt JSON");
    assert_eq!(replay["status"], "partial");
    assert_eq!(
        ingest_effect_counts(&server, path).0,
        1,
        "retry never duplicates a durable chunk"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_auto_ingest_restart_replays_durable_enrichment_intent_once() {
    let (first_server, _home_guard) = crate::tests::make_server_with_temp_home();
    let (base_url, provider_calls, provider_task) =
        spawn_admitted_ingest_embedding_provider().await;
    let _base = crate::test_support::EnvRestore::set("VOYAGE_BASE_URL", &base_url);
    let _key = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _extract_base = crate::test_support::EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("{base_url}/chat/completions"),
    );
    let _extract_key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-extract-key");
    let _extract_model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "test-model");

    let db_path = first_server.global_db_path_buf();
    let home = first_server.tachi_home_dir().to_path_buf();
    let path = "/wiki/general/admitted-enrichment-restart";
    let staged = staged_auto_ingest_fixture(
        &first_server,
        "accepted enqueue must survive a process crash",
        path,
    );

    let first = crate::pipeline_ops::run_staged_auto_ingest(&first_server, staged.clone())
        .await
        .expect("accepted enqueue returns typed pending accounting")
        .expect("staged job remains visible");
    let first: Value = serde_json::from_str(&first).expect("pending receipt JSON");
    assert_eq!(first["status"], "partial");
    assert_eq!(first["failed_stage"], "enrichment_pending");
    assert_eq!(
        first_server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM processed_events WHERE worker IN \
                         ('auto_ingest_job', 'auto_ingest_enrichment_intent')",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("count pending job and enrichment intent"),
        2,
        "queue acceptance must retain both durable recovery authorities"
    );

    // The retained receiver is intentionally never consumed: dropping this
    // server is the process-crash boundary that loses the accepted mpsc item.
    drop(first_server);
    rusqlite::Connection::open(&db_path)
        .expect("open crashed runtime intent store")
        .execute(
            "UPDATE processed_events \
             SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
             WHERE worker = 'auto_ingest_enrichment_intent'",
            [],
        )
        .expect("expire the dead runtime lease");

    let mut restarted =
        crate::server_state::MemoryServer::new_with_home_for_test(db_path, None, home)
            .expect("restart MemoryServer from the same Product DB");
    restarted.llm = std::sync::Arc::new(
        tachi_llm::LlmClient::new().expect("construct restart embedding client"),
    );

    for _ in 0..2 {
        let replay = crate::pipeline_ops::run_staged_auto_ingest(&restarted, staged.clone())
            .await
            .expect("restart replay remains admitted")
            .expect("restart replay returns pending accounting");
        let replay: Value = serde_json::from_str(&replay).expect("restart replay JSON");
        assert_eq!(replay["status"], "partial");
        assert_eq!(replay["failed_stage"], "enrichment_pending");
    }

    let mut item = {
        let mut receiver = restarted
            .enrichment_lock()
            .retained_enrich_rx
            .lock()
            .expect("lock restarted enrichment receiver");
        let receiver = receiver.as_mut().expect("test receiver retained");
        let item = receiver
            .try_recv()
            .expect("restart dispatches the durable intent");
        assert!(
            receiver.try_recv().is_err(),
            "repeated replay in one runtime must not duplicate the intent"
        );
        item
    };
    item.needs_keyword_enrichment = false;
    restarted.flush_enrichment_batch(&mut vec![item]).await;

    let completed = crate::pipeline_ops::run_staged_auto_ingest(&restarted, staged.clone())
        .await
        .expect("terminal enrichment evidence completes the staged job")
        .expect("completed accounting is returned");
    let completed: Value = serde_json::from_str(&completed).expect("completed receipt JSON");
    assert_eq!(completed["status"], "completed");
    assert_eq!(provider_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(ingest_effect_counts(&restarted, path).0, 1);
    assert_eq!(
        restarted
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM processed_events WHERE worker IN \
                         ('auto_ingest_job', 'auto_ingest_enrichment_intent')",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("count retired recovery rows"),
        0
    );

    let replayed = crate::pipeline_ops::run_staged_auto_ingest(&restarted, staged)
        .await
        .expect("terminal job replay is admitted")
        .expect("terminal receipt is replayed");
    assert_eq!(
        serde_json::from_str::<Value>(&replayed).expect("terminal replay JSON")["status"],
        "replayed"
    );
    assert_eq!(provider_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(ingest_effect_counts(&restarted, path).0, 1);
    provider_task.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_auto_ingest_live_intent_lease_blocks_cross_runtime_duplicate_provider_work() {
    let (mut server_a, _home_guard) = crate::tests::make_server_with_temp_home();
    let (base_url, provider, provider_task) = spawn_paused_admitted_embedding_provider().await;
    let _base = crate::test_support::EnvRestore::set("VOYAGE_BASE_URL", &base_url);
    let _key = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _extract_base = crate::test_support::EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("{base_url}/chat/completions"),
    );
    let _extract_key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-extract-key");
    let _extract_model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "test-model");
    server_a.llm =
        std::sync::Arc::new(tachi_llm::LlmClient::new().expect("construct A provider client"));

    let db_path = server_a.global_db_path_buf();
    let home = server_a.tachi_home_dir().to_path_buf();
    let path = "/wiki/general/admitted-enrichment-live-lease";
    let staged = staged_auto_ingest_fixture(
        &server_a,
        "a live owner must exclude another runtime before provider work",
        path,
    );
    let pending = crate::pipeline_ops::run_staged_auto_ingest(&server_a, staged.clone())
        .await
        .expect("A persists and dispatches its durable intent")
        .expect("A returns pending accounting");
    assert_eq!(
        serde_json::from_str::<Value>(&pending).expect("A pending JSON")["failed_stage"],
        "enrichment_pending"
    );
    let item_a = {
        let mut retained = server_a
            .enrichment_lock()
            .retained_enrich_rx
            .lock()
            .expect("lock A enrichment receiver");
        retained
            .as_mut()
            .expect("A receiver retained")
            .try_recv()
            .expect("A owns the dispatched intent")
    };
    let worker_a = {
        let server = server_a.clone();
        tokio::spawn(async move {
            server.flush_enrichment_batch(&mut vec![item_a]).await;
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.started.notified(),
    )
    .await
    .expect("A reaches the paused provider");
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    let mut server_b =
        crate::server_state::MemoryServer::new_with_home_for_test(db_path, None, home)
            .expect("start B against the same Product DB");
    server_b.llm =
        std::sync::Arc::new(tachi_llm::LlmClient::new().expect("construct B provider client"));
    let replay_b = crate::pipeline_ops::run_staged_auto_ingest(&server_b, staged.clone())
        .await
        .expect("B replay remains admitted")
        .expect("B observes pending accounting");
    assert_eq!(
        serde_json::from_str::<Value>(&replay_b).expect("B pending JSON")["failed_stage"],
        "enrichment_pending"
    );
    let item_b = server_b
        .enrichment_lock()
        .retained_enrich_rx
        .lock()
        .expect("lock B enrichment receiver")
        .as_mut()
        .expect("B receiver retained")
        .try_recv()
        .ok();
    let b_received_intent = item_b.is_some();
    let worker_b = item_b.map(|item| {
        let server = server_b.clone();
        tokio::spawn(async move {
            server.flush_enrichment_batch(&mut vec![item]).await;
        })
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!b_received_intent, "B must not own a live A intent");
    assert_eq!(
        provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a live durable lease admits exactly one provider invocation"
    );

    provider.release.notify_waiters();
    worker_a.await.expect("join A enrichment worker");
    if let Some(worker_b) = worker_b {
        worker_b.await.expect("join unexpected B worker");
    }
    let terminal_entry = server_a
        .with_global_store_read(|store| {
            store
                .list_by_path(path, 1, false)
                .map_err(|error| error.to_string())
        })
        .expect("load terminal enriched entry");
    assert_eq!(terminal_entry.len(), 1);
    assert!(
        terminal_entry[0].metadata.get("enrichment").is_some(),
        "provider completion must persist terminal enrichment evidence: {:?}",
        terminal_entry[0].metadata
    );
    let completed = crate::pipeline_ops::run_staged_auto_ingest(&server_a, staged)
        .await
        .expect("terminal evidence completes the job")
        .expect("terminal receipt returned");
    assert_eq!(
        serde_json::from_str::<Value>(&completed).expect("completion JSON")["status"],
        "completed",
        "terminal accounting: {completed}"
    );
    let (intent, job, receipt, completion) = server_a
        .with_global_store_read(|store| {
            let count = |worker: &str| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM processed_events WHERE worker = ?1",
                        [worker],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
            };
            let completion = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE server_id = 'ingest' \
                     AND tool_name = 'auto_ingest_job' AND success = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((
                count("auto_ingest_enrichment_intent")?,
                count("auto_ingest_job")?,
                count("auto_ingest_job_receipt")?,
                completion,
            ))
        })
        .expect("read terminal durable state");
    assert_eq!((intent, job, receipt, completion), (0, 0, 1, 1));
    assert_eq!(ingest_effect_counts(&server_a, path).0, 1);
    provider_task.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_auto_ingest_stale_consumer_cannot_write_or_retire_after_heartbeat_loss() {
    let (mut server_a, _home_guard) = crate::tests::make_server_with_temp_home();
    let (base_url, provider, provider_task) = spawn_paused_admitted_embedding_provider().await;
    let _base = crate::test_support::EnvRestore::set("VOYAGE_BASE_URL", &base_url);
    let _key = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _extract_base = crate::test_support::EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("{base_url}/chat/completions"),
    );
    let _extract_key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-extract-key");
    let _extract_model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "test-model");
    server_a.llm =
        std::sync::Arc::new(tachi_llm::LlmClient::new().expect("construct A provider client"));

    let db_path = server_a.global_db_path_buf();
    let home = server_a.tachi_home_dir().to_path_buf();
    let path = "/wiki/general/admitted-enrichment-stale-consumer";
    let staged = staged_auto_ingest_fixture(
        &server_a,
        "heartbeat ownership loss must fence stale provider results",
        path,
    );
    let pending = crate::pipeline_ops::run_staged_auto_ingest(&server_a, staged.clone())
        .await
        .expect("A dispatches durable enrichment")
        .expect("A returns pending accounting");
    assert_eq!(
        serde_json::from_str::<Value>(&pending).expect("A pending JSON")["failed_stage"],
        "enrichment_pending"
    );
    let item_a = server_a
        .enrichment_lock()
        .retained_enrich_rx
        .lock()
        .expect("lock A receiver")
        .as_mut()
        .expect("A receiver retained")
        .try_recv()
        .expect("A receives owned intent");
    let worker_a = {
        let server = server_a.clone();
        tokio::spawn(async move {
            server.flush_enrichment_batch(&mut vec![item_a]).await;
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.started.notified(),
    )
    .await
    .expect("A enters paused provider call");
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    crate::enrichment::force_next_admitted_heartbeat_ownership_loss_for_test(&server_a);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    crate::test_support::with_unrestricted_fixture_connection(&db_path, |connection| {
        connection.execute(
            "UPDATE processed_events \
             SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
             WHERE worker = 'auto_ingest_enrichment_intent'",
            [],
        )
    })
    .expect("expire A lease after forced heartbeat ownership loss");

    let mut server_b =
        crate::server_state::MemoryServer::new_with_home_for_test(db_path, None, home)
            .expect("start B against same Product DB");
    server_b.llm =
        std::sync::Arc::new(tachi_llm::LlmClient::new().expect("construct B provider client"));
    let replay_b = crate::pipeline_ops::run_staged_auto_ingest(&server_b, staged.clone())
        .await
        .expect("B reclaims expired intent")
        .expect("B returns pending accounting");
    assert_eq!(
        serde_json::from_str::<Value>(&replay_b).expect("B pending JSON")["failed_stage"],
        "enrichment_pending"
    );
    let item_b = server_b
        .enrichment_lock()
        .retained_enrich_rx
        .lock()
        .expect("lock B receiver")
        .as_mut()
        .expect("B receiver retained")
        .try_recv()
        .expect("B owns reclaimed intent");

    tokio::time::timeout(std::time::Duration::from_secs(2), worker_a)
        .await
        .expect("heartbeat ownership loss cancels A before its provider returns")
        .expect("join stale A consumer");
    let after_a = server_b
        .with_global_store_read(|store| {
            let entry = store
                .list_by_path(path, 1, false)
                .map_err(|error| error.to_string())?
                .into_iter()
                .next()
                .expect("ingested entry remains");
            let intent_rows = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events \
                     WHERE worker = 'auto_ingest_enrichment_intent'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((entry.metadata.get("enrichment").cloned(), intent_rows))
        })
        .expect("read state after stale A returns");
    assert_eq!(
        after_a,
        (None, 1),
        "stale A must discard provider results and cannot retire B's intent"
    );

    provider.release.notify_waiters();
    let worker_b = {
        let server = server_b.clone();
        tokio::spawn(async move {
            server.flush_enrichment_batch(&mut vec![item_b]).await;
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while provider.calls.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("B invokes the provider as the sole current owner");
    provider.release.notify_waiters();
    worker_b.await.expect("join B enrichment consumer");
    let completed = crate::pipeline_ops::run_staged_auto_ingest(&server_b, staged)
        .await
        .expect("B terminal evidence completes job")
        .expect("B completion receipt returned");
    assert_eq!(
        serde_json::from_str::<Value>(&completed).expect("B completion JSON")["status"],
        "completed"
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let (intent, job, receipt, completion) = server_b
        .with_global_store_read(|store| {
            let count = |worker: &str| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM processed_events WHERE worker = ?1",
                        [worker],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
            };
            let completion = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE server_id = 'ingest' \
                     AND tool_name = 'auto_ingest_job' AND success = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((
                count("auto_ingest_enrichment_intent")?,
                count("auto_ingest_job")?,
                count("auto_ingest_job_receipt")?,
                completion,
            ))
        })
        .expect("read B-owned terminal durable state");
    assert_eq!(
        (intent, job, receipt, completion),
        (0, 0, 1, 1),
        "one B-owned terminal intent, receipt, and completion lifecycle"
    );
    assert_eq!(ingest_effect_counts(&server_b, path).0, 1);
    provider_task.abort();
}

#[tokio::test]
async fn admitted_auto_ingest_enrichment_ownership_loss_is_typed_partial() {
    let server = make_server();
    let path = "/wiki/general/admitted-enrichment-ownership";
    let staged = staged_auto_ingest_fixture(
        &server,
        "ownership loss immediately before enqueue remains replayable",
        path,
    );
    crate::pipeline_ops::force_next_admitted_enrichment_ownership_loss_for_test(&server);

    let first = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("ownership loss is typed accounting, not a bare error")
        .expect("staged job remains pending");
    let first: Value = serde_json::from_str(&first).expect("ownership partial JSON");
    assert_eq!(first["status"], "partial");
    assert_eq!(first["failed_stage"], "enrichment_ownership");
    assert_eq!(first["chunks_saved"], 1);
    assert_eq!(first["enrichments_enqueued"], 0);
    assert_eq!(first["edges_written"], 0);
    assert_eq!(ingest_effect_counts(&server, path), (1, 0, 0));
    assert_eq!(
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("count replayable staged job"),
        1
    );

    let replay = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("ownership partial is replayable")
        .expect("retry returns closed accounting");
    let replay: Value = serde_json::from_str(&replay).expect("retry accounting JSON");
    assert_eq!(replay["status"], "partial");
    assert_eq!(replay["failed_stage"], "enrichment_pending");
    assert_eq!(replay["chunks_saved"], 1);
    assert_eq!(replay["enrichments_enqueued"], 1);
    assert_eq!(replay["edges_written"], 0);
    assert_eq!(ingest_effect_counts(&server, path), (1, 0, 0));
}

#[tokio::test]
async fn admitted_auto_ingest_chunk_write_failure_is_partial_and_replayable() {
    let server = make_server();
    let path = "/wiki/general/admitted-chunk-partial";
    let staged = staged_auto_ingest_fixture(&server, &"x".repeat(1300), path);
    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_admitted_chunk_write \
                 BEFORE INSERT ON memories WHEN NEW.path LIKE '%/1' \
                 BEGIN SELECT RAISE(FAIL, 'injected admitted chunk failure'); END;",
            )
        },
    )
    .expect("inject admitted chunk write failure");

    let first = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("chunk failure is a typed partial outcome")
        .expect("partial chunk receipt is returned");
    let first: Value = serde_json::from_str(&first).expect("partial JSON");
    assert_eq!(first["status"], "partial");
    assert_eq!(first["failed_stage"], "chunk_write");
    assert_eq!(first["chunks_saved"], 1);
    assert_eq!(
        ingest_effect_counts(&server, path),
        (1, 0, 0),
        "the committed prefix is reported without a false completion"
    );

    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute_batch("DROP TRIGGER fail_admitted_chunk_write"),
    )
    .expect("restore admitted chunk writes");
    complete_admitted_enrichment_stage(&server, &staged).await;
    let replay = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("partial chunk stage can replay")
        .expect("replay returns completion accounting");
    let replay: Value = serde_json::from_str(&replay).expect("completion JSON");
    assert_eq!(replay["status"], "completed");
    assert_eq!(ingest_effect_counts(&server, path).0, 2);
}

#[tokio::test]
async fn admitted_auto_ingest_completion_receipt_failure_is_partial_and_replayable() {
    let server = make_server();
    let path = "/wiki/general/admitted-receipt-partial";
    let staged =
        staged_auto_ingest_fixture(&server, "receipt failure must remain replayable", path);
    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_admitted_auto_ingest_completion \
                 BEFORE INSERT ON audit_log \
                 WHEN NEW.server_id = 'ingest' AND NEW.tool_name = 'auto_ingest_job' \
                   AND NEW.success = 1 \
                 BEGIN SELECT RAISE(FAIL, 'injected admitted completion receipt failure'); END;",
            )
        },
    )
    .expect("inject admitted completion receipt failure");

    complete_admitted_enrichment_stage(&server, &staged).await;

    let first = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("completion-receipt failure is a typed partial outcome")
        .expect("partial receipt is returned");
    let first: Value = serde_json::from_str(&first).expect("partial JSON");
    assert_eq!(first["status"], "partial");
    assert_eq!(first["failed_stage"], "completion_receipt");

    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute_batch("DROP TRIGGER fail_admitted_auto_ingest_completion"),
    )
    .expect("restore completion receipt writes");
    let replay = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("partial completion receipt can replay")
        .expect("replay returns a receipt");
    let replay: Value = serde_json::from_str(&replay).expect("completion JSON");
    assert_eq!(replay["status"], "replayed");
    assert_eq!(ingest_effect_counts(&server, path).0, 1);
}

#[tokio::test]
async fn admitted_auto_ingest_terminal_replay_returns_the_durable_receipt() {
    let server = make_server();
    let path = "/wiki/general/admitted-terminal-receipt";
    let staged = staged_auto_ingest_fixture(&server, "terminal replay accounting", path);

    complete_admitted_enrichment_stage(&server, &staged).await;

    let completed = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("initial admitted ingest completes")
        .expect("completed receipt is returned");
    let completed: Value = serde_json::from_str(&completed).expect("completed JSON");
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["chunks_saved"], 1);
    assert_eq!(completed["enrichments_enqueued"], 1);

    let replayed = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("same staged handle resolves its durable terminal receipt")
        .expect("replayed receipt is returned");
    let replayed: Value = serde_json::from_str(&replayed).expect("replayed JSON");
    assert_eq!(replayed["status"], "replayed");
    assert_eq!(replayed["chunks_saved"], completed["chunks_saved"]);
    assert_eq!(replayed["ids"], completed["ids"]);
    assert_eq!(ingest_effect_counts(&server, path).0, 1);
}

#[test]
fn admitted_auto_ingest_bounds_accept_exact_edges_and_reject_plus_one() {
    let cases = [
        (
            "raw_bytes_exact",
            "x".repeat(2 * 1024 * 1024),
            8192,
            0,
            true,
        ),
        (
            "raw_bytes_plus_one",
            "x".repeat(2 * 1024 * 1024 + 1),
            8192,
            0,
            false,
        ),
        ("chunk_size_exact", "fixture".to_string(), 8192, 0, true),
        ("chunk_size_plus_one", "fixture".to_string(), 8193, 0, false),
        (
            "overlap_below_size",
            "fixture".to_string(),
            1200,
            1199,
            true,
        ),
        (
            "overlap_equals_size",
            "fixture".to_string(),
            1200,
            1200,
            false,
        ),
        ("chunk_count_exact", "x".repeat(2048), 1, 0, true),
        ("chunk_count_plus_one", "x".repeat(2049), 1, 0, false),
    ];
    for (label, content, chunk_size, overlap, accepted) in cases {
        let result = crate::pipeline_ops::validate_admitted_ingest_bounds_for_test(
            content, chunk_size, overlap,
        );
        assert_eq!(
            result.is_ok(),
            accepted,
            "{label} boundary acceptance: {result:?}"
        );
    }
}

#[tokio::test]
async fn admitted_auto_ingest_edge_failure_is_partial_without_duplicate_chunks() {
    let server = make_server();
    let path = "/wiki/general/admitted-edge-partial";
    let mut target = make_entry("admitted-edge-target");
    target.path = "/wiki/general/admitted-edge-target".to_string();
    target.text = "shared admitted edge discriminator phrase".to_string();
    target.summary = target.text.clone();
    target.domain = Some("general".to_string());
    target.metadata = json!({"allow_cross_project": true});
    server
        .with_global_store(|store| store.upsert(&target).map_err(|error| error.to_string()))
        .expect("seed admitted edge peer");
    let staged =
        staged_auto_ingest_fixture(&server, "shared admitted edge discriminator phrase", path);
    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_admitted_edge_write \
                 BEFORE INSERT ON memory_edges \
                 BEGIN SELECT RAISE(FAIL, 'injected admitted edge failure'); END;",
            )
        },
    )
    .expect("inject admitted edge failure");

    complete_admitted_enrichment_stage(&server, &staged).await;

    let first = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("edge failure is a typed partial outcome")
        .expect("partial receipt is returned");
    let first: Value = serde_json::from_str(&first).expect("partial JSON");
    assert_eq!(first["status"], "partial");
    assert_eq!(first["failed_stage"], "edge_write");
    assert_eq!(first["chunks_saved"], 1);

    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute_batch("DROP TRIGGER fail_admitted_edge_write"),
    )
    .expect("restore edge writes");
    let replay = crate::pipeline_ops::run_staged_auto_ingest(&server, staged)
        .await
        .expect("partial edge stage can replay")
        .expect("replay returns a receipt");
    let replay: Value = serde_json::from_str(&replay).expect("replay JSON");
    assert_eq!(replay["status"], "completed");
    assert_eq!(ingest_effect_counts(&server, path).0, 1);
}

#[test]
fn admitted_ingest_callers_are_source_derived_after_memory_retirement() {
    let facade = include_str!("../../../facade_memory_ops/mod.rs");
    assert_eq!(
        facade.matches("handle_ingest_source(").count(),
        0,
        "Memory cannot call the retired direct ingest-source wrapper"
    );
    assert!(!facade.contains("handle_ingest("));

    let auto_ingest = include_str!("../../../pipeline_ops/auto_ingest.rs");
    assert!(
        auto_ingest.contains("run_admitted_ingest_from_staged_job("),
        "MCP replay must enter the typed staged-job owner"
    );
    assert_eq!(
        auto_ingest.matches("handle_ingest_source(").count(),
        0,
        "MCP replay cannot bypass the typed staged-job owner"
    );
    let proxy = include_str!("../../../mcp_pool/proxy.rs");
    assert!(proxy.contains("stage_auto_ingest_from_mcp("));
    assert!(proxy.contains("run_staged_auto_ingest("));
    assert!(!proxy.contains("handle_ingest_source("));

    for (surface, source) in [
        ("save", include_str!("../../../facade_save_ops.rs")),
        (
            "extract_facts",
            include_str!("../../../pipeline_ops/ingest/extract.rs"),
        ),
    ] {
        for forbidden_authority in [
            "reqwest::",
            "std::fs::read",
            "tokio::fs::read",
            "File::open(",
            "Command::new(",
        ] {
            assert!(
                !source.contains(forbidden_authority),
                "{surface} accepts text/reference data but owns no fetch/filesystem/command authority: {forbidden_authority}"
            );
        }
    }
}

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
    let staged_job_id = staged.job_id.clone();
    complete_admitted_enrichment_stage(&server, &staged).await;
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
    assert_eq!(entries[0].metadata["capability_id"], "mcp:web-reader");
    assert_eq!(entries[0].metadata["tool_name"], "webReader");
    assert_eq!(entries[0].metadata["admitted_job_id"], staged_job_id);
    assert!(entries[0].metadata["admitted_idempotency_key"].is_string());
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
#[allow(clippy::await_holding_lock)]
async fn production_runtime_replays_staged_auto_ingest_exactly_once_after_restart() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (base_url, _provider_calls, provider_task) =
        spawn_admitted_ingest_embedding_provider().await;
    let _base = crate::test_support::EnvRestore::set("VOYAGE_BASE_URL", &base_url);
    let _key = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _extract_base = crate::test_support::EnvRestore::set(
        "EXTRACT_BASE_URL",
        &format!("{base_url}/chat/completions"),
    );
    let _extract_key = crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "test-extract-key");
    let _extract_model = crate::test_support::EnvRestore::set("EXTRACT_MODEL", "test-model");
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
    for _ in 0..600 {
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
    provider_task.abort();
}

#[tokio::test]
async fn malformed_oldest_batch_is_quarantined_without_starving_valid_job() {
    let server = make_server();
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "valid job behind malformed replay rows"}],
        "isError": false
    }))
    .expect("build valid staged result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": "/wiki/general/auto-ingest-after-quarantine"
    });
    crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &server,
        "mcp:quarantine-reader",
        "read",
        &definition,
        None,
        &result,
    )
    .expect("stage valid seventeenth job")
    .expect("valid auto-ingest job is staged");
    server
        .with_global_store(|store| {
            for index in 0..16 {
                let malformed = if index == 0 {
                    "x".repeat(12_000)
                } else {
                    format!("{{malformed-{index}")
                };
                store
                    .connection()
                    .execute(
                        "INSERT INTO processed_events (event_hash, event_id, worker, created_at) \
                         VALUES (?1, ?2, 'auto_ingest_job', '2000-01-01T00:00:00.000Z')",
                        rusqlite::params![format!("malformed-auto-ingest-{index:02}"), malformed],
                    )
                    .map_err(|error| format!("seed malformed auto-ingest row {index}: {error}"))?;
            }
            Ok(())
        })
        .expect("seed malformed oldest replay batch");

    let first_cycle = crate::pipeline_ops::replay_pending_auto_ingest_once(&server)
        .await
        .expect("malformed rows are terminally quarantined");
    assert_eq!(
        first_cycle, 0,
        "first bounded cycle contains only malformed rows"
    );
    let (pending_after_first, quarantined) = server
        .with_global_store_read(|store| {
            let pending = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            let mut statement = store
                .connection()
                .prepare(
                    "SELECT event_id FROM processed_events \
                     WHERE worker = 'auto_ingest_job_dead_letter' ORDER BY event_hash",
                )
                .map_err(|error| error.to_string())?;
            let quarantined = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            Ok((pending, quarantined))
        })
        .expect("read first quarantine cycle state");
    assert_eq!(
        pending_after_first, 1,
        "valid seventeenth row remains pending"
    );
    assert_eq!(
        quarantined.len(),
        16,
        "all malformed rows leave pending enumeration"
    );
    for forensic_json in &quarantined {
        let forensic: Value =
            serde_json::from_str(forensic_json).expect("quarantine forensic JSON");
        assert_eq!(forensic["classification"], "auto_ingest_malformed_payload");
        assert!(forensic["original_payload"].as_str().unwrap().len() <= 4096);
        assert!(forensic["decode_error"].as_str().unwrap().len() <= 512);
    }
    assert_eq!(
        serde_json::from_str::<Value>(&quarantined[0]).unwrap()["payload_truncated"],
        true,
        "oversized malformed payload is explicitly bounded"
    );

    let second_cycle = crate::pipeline_ops::replay_pending_auto_ingest_once(&server)
        .await
        .expect("next bounded cycle reaches valid job");
    assert_eq!(
        second_cycle, 1,
        "valid job progresses after quarantine batch"
    );
    assert_eq!(
        server.complete_retained_admitted_enrichments_for_test(),
        1,
        "valid job's durable enrichment intent reaches terminal entry evidence"
    );
    let completion_cycle = crate::pipeline_ops::replay_pending_auto_ingest_once(&server)
        .await
        .expect("terminal enrichment evidence retires the valid job");
    assert_eq!(completion_cycle, 1, "valid job completes exactly once");
    let third_cycle = crate::pipeline_ops::replay_pending_auto_ingest_once(&server)
        .await
        .expect("terminal quarantine rows are not retried");
    assert_eq!(third_cycle, 0, "no quarantined row re-enters replay");

    let (memories, pending, quarantine_count) = server
        .with_global_store_read(|store| {
            let memories = store
                .list_by_path("/wiki/general/auto-ingest-after-quarantine", 10, false)
                .map_err(|error| error.to_string())?;
            let pending = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            let quarantine_count = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events \
                     WHERE worker = 'auto_ingest_job_dead_letter'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((memories, pending, quarantine_count))
        })
        .expect("read final quarantine replay state");
    assert_eq!(
        memories.len(),
        1,
        "valid logical memory is written exactly once"
    );
    assert_eq!(pending, 0);
    assert_eq!(
        quarantine_count, 16,
        "quarantine remains terminal and stable"
    );
}

const SECRET_AUTHORIZATION: &str = "Bearer sk-live-REDTEST111";
const SECRET_API_KEY: &str = "sk-live-REDTEST-api-key-222";
const SECRET_TOKEN: &str = "ghp_REDTESTTOKEN3333333333333333333333";
const SECRET_PASSWORD: &str = "REDTEST-password-444";
const SECRET_SECRET: &str = "REDTEST-secret-555";
const SECRET_CUSTOM_ENTROPY: &str = "zq8Kx2vN9mPl4wR7tY3uB6sD1fG5hJ0a";
const SECRET_URL_QUERY: &str = "ghp_URLQUERYSECRET7777777777777777777777";
const SECRET_LEGACY_V1: &str = "Bearer sk-live-LEGACYV1SECRET999";

fn secret_argument_values() -> [&'static str; 7] {
    [
        SECRET_AUTHORIZATION,
        SECRET_API_KEY,
        SECRET_TOKEN,
        SECRET_PASSWORD,
        SECRET_SECRET,
        SECRET_CUSTOM_ENTROPY,
        SECRET_URL_QUERY,
    ]
}

fn secret_bearing_arguments() -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([
        ("Authorization".to_string(), json!(SECRET_AUTHORIZATION)),
        ("api_key".to_string(), json!(SECRET_API_KEY)),
        ("token".to_string(), json!(SECRET_TOKEN)),
        ("password".to_string(), json!(SECRET_PASSWORD)),
        ("secret".to_string(), json!(SECRET_SECRET)),
        ("custom".to_string(), json!(SECRET_CUSTOM_ENTROPY)),
        (
            "url".to_string(),
            json!(format!(
                "https://example.com/article?token={SECRET_URL_QUERY}"
            )),
        ),
    ])
}

fn assert_no_secret_values(surface: &str, haystack: &str) {
    for secret in secret_argument_values() {
        assert!(
            !haystack.contains(secret),
            "{surface} leaked secret value {secret:?}: {haystack}"
        );
    }
}

fn table_cell_strings(server: &crate::server_state::MemoryServer, table: &str) -> Vec<String> {
    server
        .with_global_store_read(|store| {
            let mut statement = store
                .connection()
                .prepare(&format!("SELECT * FROM {table}"))
                .map_err(|error| error.to_string())?;
            let column_count = statement.column_count();
            let mut rows = statement.query([]).map_err(|error| error.to_string())?;
            let mut cells = Vec::new();
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                for index in 0..column_count {
                    cells.push(match row.get_ref(index).map_err(|error| error.to_string())? {
                        rusqlite::types::ValueRef::Null => String::new(),
                        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                        rusqlite::types::ValueRef::Real(value) => value.to_string(),
                        rusqlite::types::ValueRef::Text(value) => {
                            String::from_utf8_lossy(value).into_owned()
                        }
                        rusqlite::types::ValueRef::Blob(value) => {
                            String::from_utf8_lossy(value).into_owned()
                        }
                    });
                }
            }
            Ok(cells)
        })
        .unwrap_or_else(|error| panic!("dump {table} cells: {error}"))
}

fn assert_processed_events_secret_free(server: &crate::server_state::MemoryServer) {
    for cell in table_cell_strings(server, "processed_events") {
        assert_no_secret_values("processed_events", &cell);
        assert!(
            !cell.contains(SECRET_LEGACY_V1),
            "processed_events leaked legacy v1 secret: {cell}"
        );
    }
}

fn assert_memories_secret_free(server: &crate::server_state::MemoryServer) {
    for cell in table_cell_strings(server, "memories") {
        assert_no_secret_values("memories", &cell);
    }
}

fn load_auto_ingest_job_payloads(server: &crate::server_state::MemoryServer) -> Vec<Value> {
    server
        .with_global_store_read(|store| {
            let mut statement = store
                .connection()
                .prepare(
                    "SELECT event_id FROM processed_events WHERE worker = 'auto_ingest_job' \
                     ORDER BY created_at, event_hash",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            rows.map(|row| {
                let payload = row.map_err(|error| error.to_string())?;
                serde_json::from_str(&payload).map_err(|error| error.to_string())
            })
            .collect()
        })
        .expect("load staged auto-ingest payloads")
}

fn assert_staged_payload_carries_keys_and_digest(payload: &Value) {
    let source_keys = payload["source"]["argument_keys"]
        .as_array()
        .expect("staged source.argument_keys must exist");
    let metadata_keys = payload["request"]["metadata"]["argument_keys"]
        .as_array()
        .expect("staged request.metadata.argument_keys must exist");
    assert!(
        !source_keys.is_empty(),
        "argument_keys must record admitted top-level names: {payload}"
    );
    assert_eq!(source_keys, metadata_keys);
    let source_digest = payload["source"]["arguments_digest"]
        .as_str()
        .expect("staged source.arguments_digest must exist");
    let metadata_digest = payload["request"]["metadata"]["arguments_digest"]
        .as_str()
        .expect("staged request.metadata.arguments_digest must exist");
    assert!(!source_digest.is_empty(), "arguments_digest must be non-empty");
    assert_eq!(source_digest, metadata_digest);
    assert!(
        payload["source"].get("arguments").is_none(),
        "staged source must not keep a raw arguments object: {payload}"
    );
    assert!(
        payload["request"]["metadata"].get("arguments").is_none(),
        "staged request metadata must not keep a raw arguments object: {payload}"
    );
}

fn stage_secret_bearing_auto_ingest(
    server: &crate::server_state::MemoryServer,
    path: &str,
) -> crate::pipeline_ops::StagedAutoIngest {
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "public article body for secret-redaction fixture"}],
        "isError": false
    }))
    .expect("build secret-redaction tool result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": path,
    });
    let arguments = secret_bearing_arguments();
    crate::pipeline_ops::stage_auto_ingest_from_mcp(
        server,
        "mcp:secret-reader",
        "read",
        &definition,
        Some(&arguments),
        &result,
    )
    .expect("secret-bearing staging must succeed")
    .expect("auto-ingest definition admits text")
}

async fn complete_secret_bearing_ingest(
    server: &crate::server_state::MemoryServer,
    staged: &crate::pipeline_ops::StagedAutoIngest,
) -> String {
    complete_admitted_enrichment_stage(server, staged).await;
    crate::pipeline_ops::run_staged_auto_ingest(server, staged.clone())
        .await
        .expect("secret-bearing ingest must complete")
        .expect("completed ingest returns a receipt")
}

#[tokio::test]
async fn auto_ingest_staging_does_not_persist_secret_argument_values() {
    let server = make_server();
    let path = "/wiki/general/auto-ingest-secret-redaction";
    let staged = stage_secret_bearing_auto_ingest(&server, path);
    let staged_payloads = load_auto_ingest_job_payloads(&server);
    assert_eq!(staged_payloads.len(), 1, "one staged job after admission");
    assert_staged_payload_carries_keys_and_digest(&staged_payloads[0]);
    let keys = staged_payloads[0]["source"]["argument_keys"]
        .as_array()
        .expect("argument_keys");
    for expected in [
        "Authorization",
        "api_key",
        "custom",
        "password",
        "secret",
        "token",
        "url",
    ] {
        assert!(
            keys.iter().any(|key| key == expected),
            "argument_keys missing {expected}: {keys:?}"
        );
    }
    assert_processed_events_secret_free(&server);

    let receipt = complete_secret_bearing_ingest(&server, &staged).await;
    assert_no_secret_values("returned receipt", &receipt);
    let durable_receipt = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT event_id FROM processed_events \
                     WHERE event_hash = ?1 AND worker = 'auto_ingest_job_receipt'",
                    [&staged.job_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("load durable auto-ingest receipt");
    assert_no_secret_values("durable receipt", &durable_receipt);
    assert_processed_events_secret_free(&server);
    assert_memories_secret_free(&server);
    let entries = server
        .with_global_store_read(|store| {
            store
                .list_by_path(path, 10, false)
                .map_err(|error| error.to_string())
        })
        .expect("list ingested memories");
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].metadata.get("argument_keys").is_some(),
        "memory metadata must carry argument_keys: {}",
        entries[0].metadata
    );
    assert!(
        entries[0].metadata.get("arguments_digest").is_some(),
        "memory metadata must carry arguments_digest: {}",
        entries[0].metadata
    );
    let status = crate::pipeline_ops::handle_get_pipeline_status(&server)
        .await
        .expect("pipeline status");
    assert_no_secret_values("pipeline status JSON", &status);
}

#[tokio::test]
async fn auto_ingest_replay_stays_secret_free_after_completion() {
    let server = make_server();
    let path = "/wiki/general/auto-ingest-secret-replay";
    let staged = stage_secret_bearing_auto_ingest(&server, path);
    let _receipt = complete_secret_bearing_ingest(&server, &staged).await;

    let replayed = crate::pipeline_ops::run_staged_auto_ingest(&server, staged.clone())
        .await
        .expect("completed job replay remains available")
        .expect("replay returns the durable receipt");
    assert_no_secret_values("replayed receipt", &replayed);
    let replayed_json: Value = serde_json::from_str(&replayed).expect("replay receipt JSON");
    assert_eq!(replayed_json["status"], "replayed");

    let pending_cycle = crate::pipeline_ops::replay_pending_auto_ingest_once(&server)
        .await
        .expect("pending replay after completion is a no-op");
    assert_eq!(pending_cycle, 0, "completed jobs must not re-enter pending replay");

    assert_processed_events_secret_free(&server);
    assert_memories_secret_free(&server);
    let durable_receipt = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT event_id FROM processed_events \
                     WHERE event_hash = ?1 AND worker = 'auto_ingest_job_receipt'",
                    [&staged.job_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("load durable receipt after replay");
    assert_no_secret_values("durable receipt after replay", &durable_receipt);
}

#[tokio::test]
async fn legacy_v1_auto_ingest_job_is_quarantined_without_secret_forensics() {
    let server = make_server();
    let job_id = "legacy-v1-unredacted-arguments-job";
    let legacy_payload = json!({
        "schema": "tachi.admitted_mcp_ingest.v1",
        "job_id": job_id,
        "source": {
            "capability_id": "mcp:legacy-reader",
            "tool_name": "read",
            "label": "legacy-reader:read",
            "url": null,
            "arguments": {
                "Authorization": SECRET_LEGACY_V1
            }
        },
        "content_digest": "deadbeef",
        "target": {
            "scope": "global",
            "project": null,
            "domain": "general",
            "path_prefix": "/wiki/general/legacy-v1-quarantine"
        },
        "target_digest": "deadbeef",
        "policy": {
            "auto_chunk": true,
            "auto_summarize": true,
            "auto_link": true,
            "importance": 0.7,
            "chunk_size_chars": 1200,
            "chunk_overlap_chars": 120
        },
        "policy_digest": "deadbeef",
        "idempotency_key": "deadbeef",
        "request": {
            "content": "legacy v1 replay fixture",
            "source": "legacy-reader:read",
            "path_prefix": "/wiki/general/legacy-v1-quarantine",
            "auto_chunk": true,
            "auto_summarize": true,
            "auto_link": true,
            "importance": 0.7,
            "scope": "global",
            "domain": "general",
            "chunk_size_chars": 1200,
            "chunk_overlap_chars": 120,
            "metadata": {
                "capability_id": "mcp:legacy-reader",
                "tool_name": "read",
                "arguments": {
                    "Authorization": SECRET_LEGACY_V1
                },
                "auto_ingest": true
            }
        }
    });
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO processed_events (event_hash, event_id, worker, created_at) \
                     VALUES (?1, ?2, 'auto_ingest_job', STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    rusqlite::params![job_id, legacy_payload.to_string()],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .expect("inject legacy v1 staged job");

    let replay = crate::pipeline_ops::run_staged_auto_ingest(
        &server,
        crate::pipeline_ops::StagedAutoIngest {
            job_id: job_id.to_string(),
        },
    )
    .await
    .expect("legacy v1 must quarantine rather than fail closed as retryable drift");
    assert!(
        replay.is_none(),
        "legacy v1 must never execute: {replay:?}"
    );

    let (pending, forensic) = server
        .with_global_store_read(|store| {
            let pending = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM processed_events WHERE worker = 'auto_ingest_job'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            let forensic = store
                .connection()
                .query_row(
                    "SELECT event_id FROM processed_events \
                     WHERE event_hash = ?1 AND worker = 'auto_ingest_job_dead_letter'",
                    [job_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            Ok((pending, forensic))
        })
        .expect("read legacy v1 quarantine state");
    assert_eq!(pending, 0, "legacy v1 pending row must be removed");
    let forensic = forensic.expect("legacy v1 must leave a forensic dead-letter");
    assert!(
        forensic.contains("legacy_v1_staged_job_quarantined_unredacted_arguments"),
        "quarantine must name the legacy reason loudly: {forensic}"
    );
    assert!(
        !forensic.contains(SECRET_LEGACY_V1),
        "forensic dead-letter must not copy the verbatim secret: {forensic}"
    );
}

#[tokio::test]
async fn auto_ingest_arguments_digest_binds_idempotency() {
    let server = make_server();
    let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "idempotency digest fixture"}],
        "isError": false
    }))
    .expect("build digest fixture result");
    let definition = json!({
        "auto_ingest": true,
        "ingest_scope": "global",
        "ingest_domain": "general",
        "ingest_path_prefix": "/wiki/general/auto-ingest-digest-bind",
    });
    let first_arguments =
        serde_json::Map::from_iter([("note".to_string(), json!("alpha")), ("n".to_string(), json!(1))]);
    let first = crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &server,
        "mcp:digest-reader",
        "read",
        &definition,
        Some(&first_arguments),
        &result,
    )
    .expect("first staging succeeds")
    .expect("first job is admitted");
    let second = crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &server,
        "mcp:digest-reader",
        "read",
        &definition,
        Some(&first_arguments),
        &result,
    )
    .expect("identical restage is not an error");
    assert!(
        second.is_none() || second.as_ref().map(|job| job.job_id.as_str()) == Some(first.job_id.as_str()),
        "identical arguments must not create a second logical job: {second:?}"
    );
    let after_duplicate = load_auto_ingest_job_payloads(&server);
    assert_eq!(
        after_duplicate.len(),
        1,
        "identical restage must keep a single pending job"
    );
    assert_staged_payload_carries_keys_and_digest(&after_duplicate[0]);
    let first_digest = after_duplicate[0]["source"]["arguments_digest"]
        .as_str()
        .expect("first arguments_digest")
        .to_string();

    let mutated_arguments =
        serde_json::Map::from_iter([("note".to_string(), json!("beta")), ("n".to_string(), json!(1))]);
    let third = crate::pipeline_ops::stage_auto_ingest_from_mcp(
        &server,
        "mcp:digest-reader",
        "read",
        &definition,
        Some(&mutated_arguments),
        &result,
    )
    .expect("value-divergent staging succeeds")
    .expect("a different argument value is a different job");
    assert_ne!(
        third.job_id, first.job_id,
        "argument value changes must change the idempotency-bound job id"
    );
    let after_divergent = load_auto_ingest_job_payloads(&server);
    assert_eq!(after_divergent.len(), 2, "divergent arguments create a second job");
    let digests: Vec<String> = after_divergent
        .iter()
        .map(|payload| {
            payload["source"]["arguments_digest"]
                .as_str()
                .expect("arguments_digest on every staged job")
                .to_string()
        })
        .collect();
    assert!(
        digests.iter().any(|digest| digest == &first_digest),
        "original digest must remain: {digests:?}"
    );
    assert!(
        digests.iter().any(|digest| digest != &first_digest),
        "changed argument value must produce a different arguments_digest: {digests:?}"
    );
}
