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

/// Cross-vendor review (#1215 BUG 6): `wiki_ingest` used to upsert straight
/// into `/wiki/general/...` with NO lifecycle/authority marker at all, so
/// the no-marker-present read-side default (`Active`, kept for pre-#1072
/// back-compat) silently promoted arbitrary fetched URL/file content to
/// reviewed truth — an "ingest writers" bypass named explicitly in the
/// review. Ingested content is unreviewed by construction; it must land
/// `pending_review`, not `active`.
#[tokio::test]
async fn tachi_wiki_ingest_stamps_pending_review_lifecycle_not_active() {
    let (server, home) = seed_wiki_project_entries(vec![]);
    let source_path = home.temp_home.join(".tachi/ingest-lifecycle-source.md");
    std::fs::write(
        &source_path,
        "# Ingest lifecycle source\nUnreviewed fetched content.",
    )
    .expect("write ingest source");

    let response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: source_path.to_string_lossy().to_string(),
            topic: Some("IngestLifecycleTopic".to_string()),
            update_related: false,
        }))
        .await
        .expect("wiki ingest should succeed");
    let json: Value = serde_json::from_str(&response).expect("wiki ingest json");
    let created_id = json["id"].as_str().expect("created id").to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: created_id,
            include_archived: false,
            // `tachi_wiki_ingest` always writes through
            // `server.with_named_project_store("wiki", ...)` (see
            // `handle_wiki_ingest`), never the workspace-default or global
            // store, matching the sibling `with_named_project_store_read`
            // reads elsewhere in this file. `project: None` here would fall
            // through `handle_get_memory`'s workspace-resolution branch
            // (which resolves to this *test process's* git-root-derived
            // project name, not literally "wiki") straight to the global
            // store, find nothing, and get back `{"error": "Memory not
            // found"}` — a query-scope mismatch with where ingest writes,
            // not a lifecycle-stamping bug.
            project: Some("wiki".to_string()),
        }))
        .await
        .expect("get ingested memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert_eq!(
        entry["metadata"]["lifecycle"],
        json!("pending_review"),
        "ingested content must never be default-retrievable as reviewed truth: {entry:?}"
    );
    assert!(
        entry["metadata"].get("source_refs").is_none(),
        "new ingest writes must not emit legacy source_refs: {entry:#}"
    );
    assert_eq!(
        entry["metadata"]["evidence_refs_v1"][0]["ref"],
        json!(source_path.to_string_lossy().to_string())
    );
    assert_eq!(
        entry["metadata"]["evidence_refs_v1"][0]["captured_at"], entry["timestamp"],
        "typed ref and entry must share one captured timestamp"
    );
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

    let wiki_db: String = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name = 'main'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("resolve wiki DB path");
    let offline =
        rusqlite::Connection::open(wiki_db).expect("open edge failure fixture connection");
    offline
        .execute_batch(
            r#"
            INSERT INTO memory_edges
                (source_id, target_id, relation, weight, metadata, created_at)
            VALUES
                ('edge-failure-blocker', 'edge-failure-blocker', 'test', 1.0, '{}', '');
            CREATE UNIQUE INDEX "injected edge failure"
                ON memory_edges ((1));
            "#,
        )
        .expect("create edge failure constraint");
    drop(offline);

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
