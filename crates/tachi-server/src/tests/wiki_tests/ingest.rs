use super::*;
use axum::{response::IntoResponse, routing::post, Json, Router};
use std::sync::{Arc, Mutex};
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

struct MockWikiIngestProvider {
    llm: LlmClient,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockWikiIngestProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockWikiIngestProvider {
    async fn start(content: &str, finish_reason: &str) -> Self {
        Self::start_with_model(content, finish_reason, "mock-wiki-ingest").await
    }

    async fn start_with_model(content: &str, finish_reason: &str, model: &str) -> Self {
        let content = content.to_string();
        let finish_reason = finish_reason.to_string();
        let response_model = model.to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(request): Json<Value>| {
                let content = content.clone();
                let finish_reason = finish_reason.clone();
                let response_model = response_model.clone();
                captured_requests
                    .lock()
                    .expect("capture wiki ingest request")
                    .push(request);
                async move {
                    Json(json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": content},
                            "finish_reason": finish_reason,
                        }],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
                        "model": response_model,
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
            model,
        );
        assert!(llm.set_provider_secret_pool(
            "WIKI_INGEST_TEST_API_KEY",
            vec![ProviderSecret {
                key_id: "wiki-ingest-test-key".to_string(),
                value: "test-key".to_string(),
            }],
        ));
        Self {
            llm,
            requests,
            task,
        }
    }

    fn captured_requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .expect("read captured wiki ingest requests")
            .clone()
    }
}

fn wiki_ingest_llm(base_url: String, key_env: &'static str, model: &str) -> LlmClient {
    let lane = || ChatLaneConfig {
        base_url: base_url.clone(),
        model: model.to_string(),
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
        "unavailable-wiki-ingest",
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
async fn tachi_wiki_ingest_rejects_oversized_local_file_before_allocation() {
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    install_unavailable_wiki_ingest_llm(&mut server);
    let source_path = home.temp_home.join(".tachi/oversized-ingest-source.md");
    std::fs::write(
        &source_path,
        vec![b'x'; crate::wiki_ops::WIKI_INGEST_SOURCE_MAX_BYTES + 1],
    )
    .expect("write oversized ingest source");

    let error = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: source_path.to_string_lossy().to_string(),
            topic: Some("oversized-source".to_string()),
            update_related: false,
        }))
        .await
        .expect_err("oversized local wiki source must be refused");

    assert!(error.contains("byte limit"), "{error}");
    assert_eq!(wiki_memory_count(&server), 0);
}

#[tokio::test]
async fn tachi_wiki_ingest_reports_local_invalid_utf8_clearly() {
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    install_unavailable_wiki_ingest_llm(&mut server);
    let source_path = home.temp_home.join(".tachi/invalid-utf8-ingest-source.md");
    std::fs::write(&source_path, [0xff, 0xfe]).expect("write invalid UTF-8 source");

    let error = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: source_path.to_string_lossy().to_string(),
            topic: Some("invalid-utf8-source".to_string()),
            update_related: false,
        }))
        .await
        .expect_err("invalid UTF-8 local source must be refused");

    assert!(error.contains("read source file as UTF-8"), "{error}");
    assert_eq!(wiki_memory_count(&server), 0);
}

#[tokio::test]
async fn tachi_wiki_ingest_signed_url_scrubs_every_post_fetch_output_seam() {
    let raw_source = "https://93.184.216.34:8443/wiki/page.md?X-Amz-Signature=secret-token&expires=123#secret-fragment";
    let durable_source = "https://93.184.216.34:8443/wiki/page.md";
    let provider = MockWikiIngestProvider::start(
        r#"{"title":"Signed Wiki","topic":"signed-wiki","summary":"signed source summary","keywords":["signed"],"entities":["Wiki"]}"#,
        "stop",
    )
    .await;
    let (mut server, _home) = seed_wiki_project_entries(vec![]);
    server.llm = Arc::new(provider.llm.clone());

    let response = crate::wiki_ops::handle_wiki_ingest_post_fetch_for_test(
        &server,
        raw_source,
        "# Signed source\nFetched content without URL credentials.".to_string(),
        None,
        false,
    )
    .await
    .expect("signed URL post-fetch ingest");
    let response_json: Value = serde_json::from_str(&response).expect("signed ingest response");
    let id = response_json["id"].as_str().expect("signed ingest id");
    let entry = server
        .with_named_project_store_read("wiki", |store| {
            store.get(id).map_err(|error| error.to_string())
        })
        .expect("read signed ingest entry")
        .expect("signed ingest entry exists");
    let wiki_log = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("wiki-operation-log")
                .map_err(|error| error.to_string())
        })
        .expect("read signed ingest log")
        .expect("signed ingest log exists");
    let requests = provider.captured_requests();
    assert_eq!(
        requests.len(),
        1,
        "one metadata extraction request expected"
    );
    let model_prompt = requests[0]["messages"]
        .as_array()
        .expect("captured model messages")
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(response_json["source"], json!(durable_source));
    assert_eq!(entry.metadata["ingest_source"], json!(durable_source));
    assert_eq!(
        entry.metadata["evidence_refs_v1"][0]["ref"],
        json!(durable_source)
    );
    assert_eq!(
        entry.metadata["provenance"]["context"]["source"],
        json!(durable_source)
    );

    for (label, rendered) in [
        ("model prompt", model_prompt),
        ("response", response),
        ("durable metadata", entry.metadata.to_string()),
        (
            "evidence refs",
            entry.metadata["evidence_refs_v1"].to_string(),
        ),
        ("provenance", entry.metadata["provenance"].to_string()),
        ("wiki operation log", wiki_log.text),
    ] {
        assert!(
            rendered.contains(durable_source),
            "{label} must contain the sanitized source: {rendered}"
        );
        assert!(
            !rendered.contains(raw_source),
            "{label} must not contain the raw signed URL: {rendered}"
        );
        for secret in [
            "X-Amz-Signature",
            "secret-token",
            "expires=123",
            "secret-fragment",
        ] {
            assert!(
                !rendered.contains(secret),
                "{label} leaked {secret}: {rendered}"
            );
        }
    }
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
    assert_eq!(
        json["source"],
        json!(source_path.to_string_lossy().to_string())
    );
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
async fn tachi_wiki_ingest_update_preserves_trusted_first_receipt_exactly() {
    let provider_a = MockWikiIngestProvider::start_with_model(
        r#"{"title":"First Wiki","topic":"stable-receipt-topic","summary":"first summary","keywords":["first"],"entities":["Tachi"]}"#,
        "stop",
        "wiki-receipt-a",
    )
    .await;
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    server.llm = Arc::new(provider_a.llm.clone());
    let first_source = write_wiki_ingest_source(&home, "ingest-first-receipt-a.md");
    let first_response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: first_source,
            topic: None,
            update_related: false,
        }))
        .await
        .expect("first typed wiki write");
    let first_response: Value = serde_json::from_str(&first_response).expect("first wiki response");
    let first_id = first_response["id"].as_str().expect("first wiki id");
    let receipt_a = server
        .with_named_project_store_read("wiki", |store| {
            store.get(first_id).map_err(|error| error.to_string())
        })
        .expect("read first wiki row")
        .expect("first wiki row exists")
        .metadata
        .pointer("/provenance/model_invocation")
        .cloned()
        .expect("first typed invocation receipt");
    assert_eq!(receipt_a["effective_model"], "wiki-receipt-a");

    let provider_b = MockWikiIngestProvider::start_with_model(
        r#"{
            "title":"Updated Wiki",
            "topic":"stable-receipt-topic",
            "summary":"updated summary",
            "keywords":["updated"],
            "entities":["Tachi"],
            "provenance": {
                "caller_marker":"hostile",
                "model_invocation":{"schema":"hostile-model-invocation"}
            }
        }"#,
        "stop",
        "wiki-receipt-b",
    )
    .await;
    server.llm = Arc::new(provider_b.llm.clone());
    let second_source = write_wiki_ingest_source(&home, "ingest-update-receipt-b.md");
    let second_response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: second_source.clone(),
            topic: None,
            update_related: false,
        }))
        .await
        .expect("wiki update with a second invocation");
    let second_response: Value =
        serde_json::from_str(&second_response).expect("second wiki response");
    let second_id = second_response["id"].as_str().expect("second wiki id");
    assert_ne!(second_id, first_id, "update must create a replacement row");

    let replacement = server
        .with_named_project_store_read("wiki", |store| {
            store.get(second_id).map_err(|error| error.to_string())
        })
        .expect("read replacement wiki row")
        .expect("replacement wiki row exists");
    assert_eq!(
        replacement.metadata.pointer("/provenance/model_invocation"),
        Some(&receipt_a),
        "replacement must preserve receipt A as an exact JSON value"
    );
    assert_eq!(replacement.metadata["ingest_source"], second_source);
    assert_eq!(
        replacement.metadata["provenance"]["tool_name"],
        "wiki_ingest"
    );
    assert_eq!(
        replacement.metadata["provenance"]["source_kind"],
        "wiki_ingest"
    );
    assert_eq!(
        replacement.metadata["provenance"]["context"]["source"],
        replacement.metadata["ingest_source"]
    );
    assert!(
        replacement.metadata["provenance"]
            .get("caller_marker")
            .is_none(),
        "untyped model/caller provenance must not survive inject_provenance"
    );
    assert_ne!(
        replacement.metadata["provenance"]["model_invocation"]["effective_model"], "wiki-receipt-b",
        "the update invocation must not overwrite the first receipt"
    );
}

#[tokio::test]
async fn tachi_wiki_ingest_does_not_preserve_an_invalid_existing_receipt() {
    let provider_a = MockWikiIngestProvider::start_with_model(
        r#"{"title":"Legacy Wiki","topic":"invalid-existing-receipt","summary":"first summary","keywords":["first"],"entities":["Tachi"]}"#,
        "stop",
        "wiki-invalid-old",
    )
    .await;
    let (mut server, home) = seed_wiki_project_entries(vec![]);
    server.llm = Arc::new(provider_a.llm.clone());
    let first_source = write_wiki_ingest_source(&home, "ingest-invalid-old-receipt.md");
    let first_response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: first_source,
            topic: None,
            update_related: false,
        }))
        .await
        .expect("first wiki write");
    let first_response: Value = serde_json::from_str(&first_response).expect("first response");
    let first_id = first_response["id"].as_str().expect("first wiki id");

    server
        .with_named_project_store("wiki", |store| {
            let mut existing = store
                .get(first_id)
                .map_err(|error| error.to_string())?
                .expect("first wiki row exists");
            existing.metadata["provenance"]["model_invocation"]["schema"] =
                json!("legacy-invalid-receipt");
            store
                .upsert(&existing)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .expect("simulate a legacy row with an invalid reserved receipt");

    let provider_b = MockWikiIngestProvider::start_with_model(
        r#"{"title":"Replacement Wiki","topic":"invalid-existing-receipt","summary":"replacement summary","keywords":["replacement"],"entities":["Tachi"]}"#,
        "stop",
        "wiki-valid-current",
    )
    .await;
    server.llm = Arc::new(provider_b.llm.clone());
    let second_source = write_wiki_ingest_source(&home, "ingest-valid-current-receipt.md");
    let second_response = server
        .tachi_wiki_ingest(Parameters(TachiWikiIngestParams {
            source: second_source,
            topic: None,
            update_related: false,
        }))
        .await
        .expect("replacement wiki write");
    let second_response: Value =
        serde_json::from_str(&second_response).expect("replacement response");
    let second_id = second_response["id"].as_str().expect("replacement wiki id");
    let replacement = server
        .with_named_project_store_read("wiki", |store| {
            store.get(second_id).map_err(|error| error.to_string())
        })
        .expect("read replacement wiki row")
        .expect("replacement wiki row exists");
    let receipt = replacement
        .metadata
        .pointer("/provenance/model_invocation")
        .expect("replacement typed receipt");
    assert_eq!(receipt["schema"], "model-invocation-v1");
    assert_eq!(receipt["effective_model"], "wiki-valid-current");
}

#[tokio::test]
async fn model_derived_wiki_facade_update_preserves_trusted_first_receipt_exactly() {
    let provider_a = MockWikiIngestProvider::start_with_model("{}", "stop", "wiki-facade-a").await;
    let provider_b = MockWikiIngestProvider::start_with_model("{}", "stop", "wiki-facade-b").await;
    let (server, _home) = seed_wiki_project_entries(vec![]);

    let invocation_a = provider_a
        .llm
        .call_extract_llm_with_receipt("Return OK.", "OK", None, 0.0, 8)
        .await
        .expect("receipt A")
        .invocation;
    let first = crate::copilot_ops::handle_tachi_wiki_write_with_model_invocation(
        &server,
        WikiWriteParams {
            title: "Facade receipt".to_string(),
            text: "First model-derived wiki facade write.".to_string(),
            path: Some("/wiki/agent/tachi/facade-receipt".to_string()),
            topic: Some("facade-receipt".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["receipt".to_string()],
            entities: vec!["Tachi".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        },
        invocation_a,
    )
    .await
    .expect("first model-derived wiki facade write");
    let first_json: Value = serde_json::from_str(&first).expect("first facade response");
    let first_id = first_json["id"].as_str().expect("first facade id");
    let receipt_a = server
        .with_named_project_store_read("wiki", |store| {
            store.get(first_id).map_err(|error| error.to_string())
        })
        .expect("read first facade row")
        .expect("first facade row exists")
        .metadata
        .pointer("/provenance/model_invocation")
        .cloned()
        .expect("first facade typed receipt");
    assert_eq!(receipt_a["effective_model"], "wiki-facade-a");

    let invocation_b = provider_b
        .llm
        .call_extract_llm_with_receipt("Return OK.", "OK", None, 0.0, 8)
        .await
        .expect("receipt B")
        .invocation;
    let second = crate::copilot_ops::handle_tachi_wiki_write_with_model_invocation(
        &server,
        WikiWriteParams {
            title: "Facade receipt".to_string(),
            text: "Second model-derived wiki facade write must not overwrite receipt A."
                .to_string(),
            path: Some("/wiki/agent/tachi/facade-receipt".to_string()),
            topic: Some("facade-receipt".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["receipt".to_string()],
            entities: vec!["Tachi".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: Some(json!({
                "provenance": {
                    "caller_marker": "hostile",
                    "model_invocation": {
                        "schema": "model-invocation-v1",
                        "effective_model": "hostile-forgery"
                    }
                }
            })),
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        },
        invocation_b,
    )
    .await
    .expect("second model-derived wiki facade write");
    let second_json: Value = serde_json::from_str(&second).expect("second facade response");
    let second_id = second_json["id"].as_str().expect("second facade id");
    assert_eq!(second_id, first_id, "wiki facade update stays in place");

    let updated = server
        .with_named_project_store_read("wiki", |store| {
            store.get(second_id).map_err(|error| error.to_string())
        })
        .expect("read updated facade row")
        .expect("updated facade row exists");
    assert_eq!(
        updated.metadata.pointer("/provenance/model_invocation"),
        Some(&receipt_a),
        "model-derived facade update must preserve trusted receipt A exactly"
    );
    assert!(
        updated.metadata["provenance"]
            .get("caller_marker")
            .is_none(),
        "public/untyped metadata must not select the trusted receipt slot"
    );
    assert_ne!(
        updated.metadata["provenance"]["model_invocation"]["effective_model"], "wiki-facade-b",
        "invocation B must not overwrite the first model-derived receipt"
    );
    assert_ne!(
        updated.metadata["provenance"]["model_invocation"]["effective_model"], "hostile-forgery",
        "caller metadata must not forge the model invocation receipt"
    );
}

#[tokio::test]
async fn ordinary_public_wiki_update_preserves_trusted_existing_model_receipt() {
    let provider_a = MockWikiIngestProvider::start_with_model("{}", "stop", "wiki-public-a").await;
    let (server, _home) = seed_wiki_project_entries(vec![]);

    let invocation_a = provider_a
        .llm
        .call_extract_llm_with_receipt("Return OK.", "OK", None, 0.0, 8)
        .await
        .expect("receipt A")
        .invocation;
    let first = crate::copilot_ops::handle_tachi_wiki_write_with_model_invocation(
        &server,
        WikiWriteParams {
            title: "Public update receipt".to_string(),
            text: "First model-derived wiki write with receipt A.".to_string(),
            path: Some("/wiki/agent/tachi/public-update-receipt".to_string()),
            topic: Some("public-update-receipt".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["receipt".to_string()],
            entities: vec!["Tachi".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        },
        invocation_a,
    )
    .await
    .expect("first model-derived wiki write");
    let first_json: Value = serde_json::from_str(&first).expect("first public response");
    let first_id = first_json["id"].as_str().expect("first public id");
    let receipt_a = server
        .with_named_project_store_read("wiki", |store| {
            store.get(first_id).map_err(|error| error.to_string())
        })
        .expect("read first public row")
        .expect("first public row exists")
        .metadata
        .pointer("/provenance/model_invocation")
        .cloned()
        .expect("first public typed receipt");
    assert_eq!(receipt_a["effective_model"], "wiki-public-a");

    let second = crate::copilot_ops::handle_tachi_wiki_write(
        &server,
        WikiWriteParams {
            title: "Public update receipt".to_string(),
            text: "Ordinary public wiki update must keep receipt A.".to_string(),
            path: Some("/wiki/agent/tachi/public-update-receipt".to_string()),
            topic: Some("public-update-receipt".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["receipt".to_string()],
            entities: vec!["Tachi".to_string()],
            importance: 0.9,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: Some(json!({
                "provenance": {
                    "caller_marker": "hostile",
                    "model_invocation": {
                        "schema": "model-invocation-v1",
                        "effective_model": "hostile-new-receipt"
                    }
                }
            })),
            force: true,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        },
    )
    .await
    .expect("ordinary public wiki update");
    let second_json: Value = serde_json::from_str(&second).expect("second public response");
    let second_id = second_json["id"].as_str().expect("second public id");
    assert_eq!(second_id, first_id, "public wiki update stays in place");

    let updated = server
        .with_named_project_store_read("wiki", |store| {
            store.get(second_id).map_err(|error| error.to_string())
        })
        .expect("read updated public row")
        .expect("updated public row exists");
    assert_eq!(
        updated.metadata.pointer("/provenance/model_invocation"),
        Some(&receipt_a),
        "ordinary public replacement must restore trusted receipt A exactly"
    );
    assert!(
        updated.metadata["provenance"]
            .get("caller_marker")
            .is_none(),
        "hostile public provenance must not survive inject_provenance"
    );
    assert_ne!(
        updated.metadata["provenance"]["model_invocation"]["effective_model"],
        "hostile-new-receipt",
        "public metadata must not forge or select the receipt slot"
    );
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
    assert_eq!(entry["metadata"]["artifact_kind"], json!("wiki"));
    assert_eq!(
        entry["metadata"]["knowledge_scope"],
        json!("unspecified"),
        "physical storage in project=wiki must not invent semantic applicability"
    );
    assert_eq!(entry["metadata"]["origin_projects"], json!([]));
    assert_eq!(entry["metadata"]["applies_to"], json!({}));
    assert_eq!(entry["metadata"]["known_exceptions"], json!([]));
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
    let persisted: i64 = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE json_extract(metadata, '$.ingest_source') = ?1",
                    [source_path.to_string_lossy().as_ref()],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count failed ingest rows");
    assert_eq!(persisted, 0, "failed edge must roll the ingest row back");
}
