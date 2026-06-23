use super::*;

#[tokio::test]
async fn tachi_wiki_ingest_creates_entry_and_related_edge() {
    let mut existing = make_entry("wiki-ingest-existing");
    existing.path = "/wiki/general/existing".to_string();
    existing.summary = "Existing ingest topic".to_string();
    existing.text = "Existing entry for IngestTopic.".to_string();
    existing.entities = vec!["IngestTopic".to_string()];

    let (server, home) = seed_wiki_project_entries(vec![existing]);
    let source_path = home.temp_home.join(".tachi/ingest-source.md");
    std::fs::write(
        &source_path,
        "# Ingest source\nIngestTopic appears in this source.",
    )
    .expect("write ingest source");

    let response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: source_path.to_string_lossy().to_string(),
            topic: Some("IngestTopic".to_string()),
            update_related: true,
        }))
        .await
        .expect("wiki ingest should succeed");
    let json: Value = serde_json::from_str(&response).expect("wiki ingest json");
    let created_id = json["id"].as_str().expect("created id");
    assert_eq!(json["status"], json!("created"));
    assert!(json["related_entries"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["id"] == "wiki-ingest-existing")
    }));

    let edges = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get_edges(created_id, "outgoing", Some("references"))
                .map_err(|e| e.to_string())
        })
        .expect("read ingest edges");
    assert!(edges
        .iter()
        .any(|edge| edge.target_id == "wiki-ingest-existing"));
}

#[tokio::test]
async fn tachi_wiki_ingest_propagates_edge_write_errors() {
    let mut existing = make_entry("wiki-ingest-edge-error-existing");
    existing.path = "/wiki/general/edge-error-existing".to_string();
    existing.summary = "Existing edge error topic".to_string();
    existing.text = "Existing entry for EdgeErrorTopic.".to_string();
    existing.entities = vec!["EdgeErrorTopic".to_string()];

    let (server, home) = seed_wiki_project_entries(vec![existing]);
    let source_path = home.temp_home.join(".tachi/ingest-edge-source.md");
    std::fs::write(
        &source_path,
        "# Ingest source\nEdgeErrorTopic appears in this source.",
    )
    .expect("write ingest source");

    server
        .with_named_project_store("wiki", |store| {
            store
                .connection()
                .execute_batch(
                    r#"
                    CREATE TRIGGER inject_edge_write_failure
                    BEFORE INSERT ON memory_edges
                    BEGIN
                        SELECT RAISE(ABORT, 'injected edge failure');
                    END;
                    "#,
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("create edge failure trigger");

    let err = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: source_path.to_string_lossy().to_string(),
            topic: Some("EdgeErrorTopic".to_string()),
            update_related: true,
        }))
        .await
        .expect_err("wiki ingest should fail when edge write fails");

    assert!(
        err.contains("wiki ingest edge"),
        "expected edge write error to propagate, got: {err}"
    );
}
