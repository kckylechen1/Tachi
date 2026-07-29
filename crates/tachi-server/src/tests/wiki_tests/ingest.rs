use super::*;
use axum::{response::IntoResponse, routing::post, Json, Router};
use std::sync::Arc;
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

struct MockWikiIngestProvider {
    llm: LlmClient,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockWikiIngestProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockWikiIngestProvider {
    async fn start(content: &str, finish_reason: &str) -> Self {
        let content = content.to_string();
        let finish_reason = finish_reason.to_string();
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let content = content.clone();
                let finish_reason = finish_reason.clone();
                async move {
                    Json(json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": content},
                            "finish_reason": finish_reason,
                        }],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
                        "model": "mock-wiki-ingest",
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock wiki ingest provider");
        let port = listener
            .local_addr()
            .expect("mock wiki ingest provider address")
            .port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock wiki ingest provider");
        });
        let llm = wiki_ingest_llm(
            format!("http://127.0.0.1:{port}/chat/completions"),
            "WIKI_INGEST_TEST_API_KEY",
        );
        assert!(llm.set_provider_secret_pool(
            "WIKI_INGEST_TEST_API_KEY",
            vec![ProviderSecret {
                key_id: "wiki-ingest-test-key".to_string(),
                value: "test-key".to_string(),
            }],
        ));
        Self { llm, task }
    }
}

fn wiki_ingest_llm(base_url: String, key_env: &'static str) -> LlmClient {
    let lane = || ChatLaneConfig {
        base_url: base_url.clone(),
        model: "mock-wiki-ingest".to_string(),
        api_key_envs: vec![key_env],
    };
    LlmClient::new_with_config(
        ProviderRuntimeConfig {
            extract: lane(),
            summary: lane(),
            reasoning: lane(),
            distill: lane(),
            rerank: RerankConfig {
                provider: RerankProviderKind::Voyage,
                local_endpoint: None,
            },
        },
        None,
    )
    .expect("initialize wiki ingest LLM")
}

fn install_unavailable_wiki_ingest_llm(server: &mut crate::MemoryServer) {
    server.llm = Arc::new(wiki_ingest_llm(
        "http://127.0.0.1:1/chat/completions".to_string(),
        "WIKI_INGEST_TEST_MISSING_API_KEY",
    ));
}

fn write_wiki_ingest_source(home: &super::super::TempHomeGuard, name: &str) -> String {
    let source_path = home.temp_home.join(".tachi").join(name);
    std::fs::write(&source_path, "# Source\nA durable wiki ingest test source.")
        .expect("write wiki ingest source");
    source_path.to_string_lossy().to_string()
}

fn wiki_memory_count(server: &crate::MemoryServer) -> i64 {
    server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .expect("count wiki memories")
}

#[tokio::test]
async fn tachi_wiki_ingest_creates_entry_and_related_edge() {
    let mut existing = make_entry("wiki-ingest-existing");
    existing.path = "/wiki/general/existing".to_string();
    existing.summary = "Existing ingest topic".to_string();
    existing.text = "Existing entry for IngestTopic.".to_string();
    existing.entities = vec!["IngestTopic".to_string()];

    let (mut server, home) = seed_wiki_project_entries(vec![existing]);
    install_unavailable_wiki_ingest_llm(&mut server);
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
async fn tachi_wiki_ingest_persists_real_extract_receipt_on_first_write() {
    let provider = MockWikiIngestProvider::start(
        r#"{"title":"Receipt Wiki","topic":"receipt-wiki","summary":"receipt summary","keywords":["receipt"],"entities":["Tachi"]}"#,
        "stop",
    )
    .await;
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    server.llm = Arc::new(provider.llm.clone());
    let source = write_wiki_ingest_source(&home, "ingest-real-receipt.md");

    let response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source,
            topic: None,
            update_related: false,
        }))
        .await
        .expect("wiki ingest with real model receipt");
    let response: Value = serde_json::from_str(&response).expect("wiki receipt response");
    let id = response["id"].as_str().expect("wiki receipt id");
    let entry = server
        .with_named_project_store_read("wiki", |store| {
            store.get(id).map_err(|error| error.to_string())
        })
        .expect("read wiki receipt row")
        .expect("wiki receipt row exists");
    let receipt = entry
        .metadata
        .pointer("/provenance/model_invocation")
        .expect("wiki ingest model receipt");
    assert_eq!(receipt["schema"], "model-invocation-v1");
    assert_eq!(receipt["lane"], "extract");
    assert_eq!(receipt["completion_status"], "complete");
}

#[tokio::test]
async fn tachi_wiki_ingest_truncated_metadata_writes_nothing() {
    let provider = MockWikiIngestProvider::start(
        r#"{"title":"Looks complete","topic":"truncated"}"#,
        "length",
    )
    .await;
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    server.llm = Arc::new(provider.llm.clone());
    let source = write_wiki_ingest_source(&home, "ingest-truncated.md");

    let error = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source,
            topic: None,
            update_related: false,
        }))
        .await
        .expect_err("truncated wiki metadata must fail");
    assert!(error.contains(tachi_llm::LLM_OUTPUT_TRUNCATED), "{error}");
    assert_eq!(wiki_memory_count(&server), 0);
}

#[tokio::test]
async fn tachi_wiki_ingest_malformed_metadata_writes_nothing() {
    let provider = MockWikiIngestProvider::start("not wiki metadata JSON", "stop").await;
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    server.llm = Arc::new(provider.llm.clone());
    let source = write_wiki_ingest_source(&home, "ingest-malformed.md");

    let error = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source,
            topic: None,
            update_related: false,
        }))
        .await
        .expect_err("malformed wiki metadata must fail");
    assert!(
        error.contains("wiki ingest metadata parse failed"),
        "{error}"
    );
    assert_eq!(wiki_memory_count(&server), 0);
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
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    install_unavailable_wiki_ingest_llm(&mut server);
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

    let (mut server, home) = seed_wiki_project_entries(vec![existing]);
    install_unavailable_wiki_ingest_llm(&mut server);
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
