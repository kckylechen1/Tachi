use super::*;
use crate::MemoryServer;

fn source_params() -> IngestSourceParams {
    IngestSourceParams {
        content: "alpha durability chunk beta durability chunk gamma".to_string(),
        source_url: Some("https://example.com/durable-source".to_string()),
        source: Some("durable-source".to_string()),
        path_prefix: Some("/wiki/general/durable-source".to_string()),
        auto_chunk: true,
        auto_summarize: false,
        auto_link: false,
        importance: 0.7,
        scope: "global".to_string(),
        project: None,
        domain: Some("general".to_string()),
        chunk_size_chars: 18,
        chunk_overlap_chars: 0,
        metadata: None,
    }
}

fn source_server_at(path: std::path::PathBuf) -> MemoryServer {
    let bootstrap = make_server();
    drop(bootstrap);
    MemoryServer::new(path, None).expect("open durable source ingest database")
}

#[tokio::test]
async fn ingest_source_chunks_content_and_builds_graph_edges() {
    let server = make_server();

    server
        .with_global_store(|store| {
            let entry = MemoryEntry {
                id: "existing-edge-target".to_string(),
                path: "/wiki/coding/reference".to_string(),
                summary: "cargo workspace chunking".to_string(),
                text: "cargo workspace chunking graph edge reference".to_string(),
                importance: 0.8,
                timestamp: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_until: None,
                category: "fact".to_string(),
                topic: "reference".to_string(),
                keywords: vec![],
                persons: vec![],
                entities: vec![],
                location: String::new(),
                source: "test".to_string(),
                scope: "global".to_string(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                metadata: json!({}),
                vector: None,
                retention_policy: None,
                domain: Some("coding".to_string()),
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            };
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed comparable memory");

    let response = crate::pipeline_ops::handle_ingest_source(
        &server,
        IngestSourceParams {
            content:
                "cargo workspace chunking graph edge reference\nsecond paragraph for another chunk"
                    .to_string(),
            source_url: Some("https://example.com/docs".to_string()),
            source: Some("docs".to_string()),
            path_prefix: Some("/wiki/coding/test-ingest".to_string()),
            auto_chunk: true,
            auto_summarize: false,
            auto_link: true,
            importance: 0.75,
            scope: "global".to_string(),
            project: None,
            domain: Some("coding".to_string()),
            chunk_size_chars: 32,
            chunk_overlap_chars: 0,
            metadata: None,
        },
    )
    .await
    .expect("ingest_source should succeed");
    let response_json: Value =
        serde_json::from_str(&response).expect("ingest_source response json");
    let saved = response_json["chunks_saved"].as_u64().unwrap_or(0);
    assert!(saved >= 2, "expected chunked ingest, got {response_json}");

    let ids: Vec<String> = serde_json::from_value(response_json["ids"].clone()).expect("ids");
    let edges = server
        .with_global_store_read(|store| {
            store
                .get_edges(&ids[0], "outgoing", Some("similar_to"))
                .map_err(|e| e.to_string())
        })
        .expect("load related edges");
    assert!(
        edges
            .iter()
            .any(|edge| edge.target_id == "existing-edge-target"),
        "expected auto-linked edge to seeded reference"
    );
}

#[tokio::test]
async fn ingest_source_empty_content_records_skip_audit() {
    let server = make_server();

    let response = crate::pipeline_ops::handle_ingest_source(
        &server,
        IngestSourceParams {
            content: "   ".to_string(),
            source_url: Some("https://example.com/empty".to_string()),
            source: Some("empty-source".to_string()),
            path_prefix: Some("/wiki/general/empty".to_string()),
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            importance: 0.7,
            scope: "global".to_string(),
            project: None,
            domain: Some("general".to_string()),
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            metadata: None,
        },
    )
    .await
    .expect("empty ingest_source should return skipped response");

    let json: Value = serde_json::from_str(&response).expect("json");
    assert_eq!(json["status"], "skipped");

    let audits = server
        .with_global_store_read(|store| {
            store
                .audit_log_list(20, Some("ingest"))
                .map_err(|e| e.to_string())
        })
        .expect("audit list");
    assert!(audits.iter().any(|entry| {
        entry["tool_name"] == "ingest_source" && entry["error_kind"] == "empty_source_content"
    }));
}

#[tokio::test]
async fn abandoned_source_claim_reopens_with_stable_ordered_ids_exactly_once() {
    let temp = tempfile::tempdir().expect("temp source ingest database");
    let db_path = temp.path().join("memory.db");
    let params = source_params();
    let path_prefix = params.path_prefix.clone().expect("path prefix");
    let source_label = params.source.as_deref().expect("source label");
    let event_hash = crate::utils::stable_hash(&format!(
        "{}:{}:{}",
        source_label,
        path_prefix,
        params.content.trim()
    ));

    let server = source_server_at(db_path.clone());
    server
        .with_global_store(|store| {
            store
                .try_claim_event(&event_hash, &path_prefix, "ingest_source")
                .map_err(|error| format!("seed abandoned source claim: {error}"))?;
            store
                .connection()
                .execute(
                    "UPDATE processed_events \
                     SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
                     WHERE event_hash = ?1 AND worker = 'ingest_source'",
                    [&event_hash],
                )
                .map_err(|error| format!("age source claim with database time: {error}"))?;
            Ok(())
        })
        .expect("seed abandoned source claim");
    drop(server);

    let reopened = source_server_at(db_path);
    let completed = crate::pipeline_ops::handle_ingest_source(&reopened, params.clone())
        .await
        .expect("abandoned source claim must retry after reopen");
    let response: Value = serde_json::from_str(&completed).expect("completed source response");
    assert_eq!(response["status"], "completed");

    let chunks = crate::pipeline_ops::helpers::chunk_text(
        params.content.trim(),
        params.chunk_size_chars,
        params.chunk_overlap_chars,
    );
    let expected_ids = chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            format!(
                "ingest-source:{event_hash}:{index}:{}",
                crate::utils::stable_hash(chunk)
            )
        })
        .collect::<Vec<_>>();
    let ids: Vec<String> =
        serde_json::from_value(response["ids"].clone()).expect("ordered source ids");
    assert_eq!(ids, expected_ids, "response ids must follow chunk order");

    let replay = crate::pipeline_ops::handle_ingest_source(&reopened, params)
        .await
        .expect("completed source replay");
    let replay: Value = serde_json::from_str(&replay).expect("replay response");
    assert_eq!(replay["status"], "skipped");

    let persisted = reopened
        .with_global_store_read(|store| {
            store
                .list_by_path(&path_prefix, 20, false)
                .map_err(|error| format!("list source chunks: {error}"))
        })
        .expect("read source chunks");
    assert_eq!(persisted.len(), expected_ids.len());
    for expected_id in expected_ids {
        assert_eq!(
            persisted
                .iter()
                .filter(|entry| entry.id == expected_id)
                .count(),
            1,
            "each logical source chunk must exist exactly once"
        );
    }
}

#[tokio::test]
async fn source_success_audit_failure_is_loud_retryable_and_idempotent() {
    let temp = tempfile::tempdir().expect("temp source audit failure database");
    let server = source_server_at(temp.path().join("memory.db"));
    let mut params = source_params();
    params.auto_chunk = false;
    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| {
            connection.execute_batch(
                    "CREATE TRIGGER fail_ingest_success_audit \
                     BEFORE INSERT ON audit_log \
                     WHEN NEW.server_id = 'ingest' AND NEW.tool_name = 'ingest_source' AND NEW.success = 1 \
                     BEGIN SELECT RAISE(FAIL, 'injected ingest audit failure'); END;",
            )
        },
    )
    .expect("inject audit_log_insert failure");

    let error = crate::pipeline_ops::handle_ingest_source(&server, params.clone())
        .await
        .expect_err("success audit failure must reach the source caller");
    assert!(
        error.contains("audit"),
        "audit failure must be explicit: {error}"
    );

    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute_batch("DROP TRIGGER fail_ingest_success_audit"),
    )
    .expect("restore audit writes");

    let completed = crate::pipeline_ops::handle_ingest_source(&server, params.clone())
        .await
        .expect("audit failure must release the claim for retry");
    let response: Value = serde_json::from_str(&completed).expect("retry response");
    assert_eq!(response["status"], "completed");

    let (facts, success_audits, failure_audits) = server
        .with_global_store_read(|store| {
            let facts = store
                .list_by_path(
                    params.path_prefix.as_deref().expect("path prefix"),
                    10,
                    false,
                )
                .map_err(|error| format!("list retried source facts: {error}"))?;
            let success: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log \
                     WHERE server_id = 'ingest' AND tool_name = 'ingest_source' AND success = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("count source success audits: {error}"))?;
            let failure: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log \
                     WHERE server_id = 'ingest' AND tool_name = 'ingest_source' AND success = 0",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("count source failure audits: {error}"))?;
            Ok((facts, success, failure))
        })
        .expect("read source retry state");
    assert_eq!(facts.len(), 1, "retry must not duplicate the source fact");
    assert_eq!(success_audits, 1);
    assert_eq!(failure_audits, 1);
}
