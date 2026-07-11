use super::*;

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
                last_access: None,
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
