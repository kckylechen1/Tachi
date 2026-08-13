use super::*;

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
fn memory_inventory_and_ingest_callers_remain_source_derived_during_transition() {
    let inventory_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tachi-params/src/facade/action_inventory.rs"
    ));
    let inventory_block = inventory_source
        .split("pub const TACHI_MEMORY_ACTIONS")
        .nth(1)
        .and_then(|source| source.split("];").next())
        .expect("locate the production Memory action inventory");
    let inventory = inventory_block
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix('"')
                .and_then(|line| line.split('"').next())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(inventory.len(), 17, "#1689 owns the later 17 -> 9 fold");

    let facade = include_str!("../../../facade_memory_ops/mod.rs");
    let facade_inventory_block = facade
        .split("match action.as_str()")
        .nth(1)
        .and_then(|source| source.split("\"recall_simulate\"").next())
        .expect("locate active Memory facade action arms");
    let facade_inventory = facade_inventory_block
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let action = line.strip_prefix('"')?.split('"').next()?;
            line.contains("=>").then_some(action)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        facade_inventory, inventory,
        "facade routing must remain an exact source-derived projection of the 17 actions"
    );
    let direct_source_calls = facade.matches("handle_ingest_source(").count();
    assert_eq!(
        direct_source_calls, 1,
        "the one legacy Memory ingest_source caller stays explicit until #1689"
    );
    assert!(
        facade.contains("handle_ingest("),
        "the legacy unified Memory ingest caller also remains transitional"
    );

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
