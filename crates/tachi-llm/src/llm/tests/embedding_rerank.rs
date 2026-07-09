use super::*;

fn embedding_values(seed: f64) -> Vec<f64> {
    (0..1024).map(|idx| seed + idx as f64).collect()
}

#[test]
fn voyage_batch_embeddings_accept_matching_response_indexes() {
    let data = vec![
        json!({"index": 0, "embedding": embedding_values(0.0)}),
        json!({"index": 1, "embedding": embedding_values(1000.0)}),
    ];

    let embeddings =
        parse_voyage_batch_embeddings(&data, 2).expect("matching indexes should parse");

    assert_eq!(embeddings.len(), 2);
    assert_eq!(embeddings[0][0], 0.0);
    assert_eq!(embeddings[1][0], 1000.0);
}

#[test]
fn voyage_batch_embeddings_reject_mismatched_response_index() {
    let data = vec![
        json!({"index": 1, "embedding": embedding_values(1000.0)}),
        json!({"index": 0, "embedding": embedding_values(0.0)}),
    ];

    let err = parse_voyage_batch_embeddings(&data, 2)
        .expect_err("out-of-order response indexes should fail");

    assert!(err.contains("index mismatch"));
}

#[test]
fn rerank_document_filter_preserves_original_indices() {
    let docs = vec![
        "first".to_string(),
        "   ".to_string(),
        "second".to_string(),
        "".to_string(),
    ];

    let (filtered, index_map) = non_empty_rerank_documents(&docs);

    assert_eq!(filtered, vec![&docs[0], &docs[2]]);
    assert_eq!(index_map, vec![0, 2]);
}

#[test]
fn default_rerank_provider_is_voyage() {
    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::unset(RERANK_PROVIDER_ENV);
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);

    let cfg = RerankConfig::from_env().expect("default config");
    assert_eq!(cfg.provider, RerankProviderKind::Voyage);
    assert_eq!(cfg.provider_name(), "voyage");
    assert_eq!(cfg.model_name(), Some("rerank-2.5"));
}

#[test]
fn voyage_provider_selected_explicitly() {
    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "voyage");

    let cfg = RerankConfig::from_env().expect("voyage config");
    assert_eq!(cfg.provider, RerankProviderKind::Voyage);
}

#[test]
fn voyage_request_shape_unchanged() {
    // Frozen request body for the voyage arm (pre-seam parity).
    let docs = ["alpha".to_string(), "beta".to_string()];
    let refs: Vec<&String> = docs.iter().collect();
    let body = voyage_rerank_request_body("probe query", &refs, 2);
    assert_eq!(body["model"], json!("rerank-2.5"));
    assert_eq!(body["query"], json!("probe query"));
    assert_eq!(body["documents"], json!(["alpha", "beta"]));
    assert_eq!(body["top_k"], json!(2));
    // Exactly the four keys the pre-seam path sent.
    let obj = body.as_object().expect("object");
    assert_eq!(obj.len(), 4);
}

#[test]
fn unknown_rerank_provider_fails_closed() {
    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "cohere");

    let err = RerankConfig::from_env().expect_err("unknown provider must fail");
    assert!(err.contains("unknown rerank provider"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn local_rerank_unconfigured_returns_typed_error() {
    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "local");
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);

    let client = LlmClient::new().expect("client");
    let docs = vec!["a".to_string(), "b".to_string()];
    let err = client
        .rerank("q", &docs, 1)
        .await
        .expect_err("unconfigured local must error");
    assert_eq!(err, "local rerank provider not configured");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn local_rerank_unconfigured_does_not_fall_back_to_voyage() {
    // Fail closed: even with a voyage key present, local without endpoint errors.
    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "local");
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);
    let _voyage = EnvRestore::set("VOYAGE_API_KEY", "should-not-be-used");

    let client = LlmClient::new().expect("client");
    let docs = vec!["a".to_string(), "b".to_string()];
    let err = client
        .rerank("q", &docs, 1)
        .await
        .expect_err("must not silent-fallback to voyage");
    assert_eq!(err, "local rerank provider not configured");
    assert!(!err.to_ascii_lowercase().contains("voyage"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn local_rerank_hits_configured_endpoint() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    #[derive(Clone, Default)]
    struct Capture {
        hits: usize,
        last_body: Option<Value>,
    }
    let capture = Arc::new(Mutex::new(Capture::default()));
    let app = Router::new()
        .route(
            "/rerank",
            post(
                |State(cap): State<Arc<Mutex<Capture>>>, body: Json<Value>| async move {
                    let mut g = cap.lock().unwrap_or_else(|e| e.into_inner());
                    g.hits += 1;
                    g.last_body = Some(body.0);
                    Json(json!({
                        "results": [
                            {"index": 1, "relevance_score": 0.91},
                            {"index": 0, "relevance_score": 0.12},
                        ]
                    }))
                },
            ),
        )
        .with_state(capture.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local rerank mock");
    let port = listener.local_addr().expect("addr").port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serve");
    });

    let endpoint = format!("http://127.0.0.1:{port}/rerank");
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "local");
    let _endpoint = EnvRestore::set(RERANK_LOCAL_ENDPOINT_ENV, &endpoint);

    let client = LlmClient::new().expect("client");
    // Index 1 is empty → filtered out; remaining map: filtered[0]=orig 0, filtered[1]=orig 2
    let docs = vec![
        "doc-zero".to_string(),
        "   ".to_string(),
        "doc-two".to_string(),
    ];
    let out = client
        .rerank("which doc?", &docs, 2)
        .await
        .expect("configured local should succeed");

    assert_eq!(out, vec![(2, 0.91), (0, 0.12)]);

    let g = capture.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(g.hits, 1, "request must hit the configured endpoint");
    let body = g.last_body.as_ref().expect("captured body");
    assert_eq!(body["query"], json!("which doc?"));
    assert_eq!(body["documents"], json!(["doc-zero", "doc-two"]));
    assert_eq!(body["top_k"], json!(2));
    assert!(
        body.get("model").is_none(),
        "local body must not inject voyage model name"
    );

    server_task.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn default_path_dispatches_to_voyage_arm() {
    // Without provider override, `rerank` selects voyage — which requires a
    // voyage key. Missing key proves the voyage arm (not local) was chosen.
    let _guard = crate::test_support::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::unset(RERANK_PROVIDER_ENV);
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);
    let _vk = EnvRestore::unset("VOYAGE_API_KEY");
    let _vrk = EnvRestore::unset("VOYAGE_RERANK_API_KEY");

    let client = LlmClient::new().expect("client");
    // Clear any vault-backed secrets that might exist on the host.
    client.clear_provider_secrets();
    let docs = vec!["a".to_string(), "b".to_string()];
    let err = client
        .rerank("q", &docs, 1)
        .await
        .expect_err("voyage arm without key must fail with missing-key error");
    assert!(
        err.contains("Missing API key") || err.contains("VOYAGE"),
        "default path must enter voyage arm, got: {err}"
    );
    assert!(
        !err.contains("local rerank provider not configured"),
        "default must not hit local arm"
    );
}
