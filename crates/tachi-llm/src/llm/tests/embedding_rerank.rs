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
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::unset(RERANK_PROVIDER_ENV);
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);

    let cfg = RerankConfig::from_env().expect("default config");
    assert_eq!(cfg.provider, RerankProviderKind::Voyage);
    assert_eq!(cfg.provider_name(), "voyage");
    assert_eq!(cfg.model_name(), Some("rerank-2.5"));
}

#[test]
fn voyage_provider_selected_explicitly() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "voyage");

    let cfg = RerankConfig::from_env().expect("voyage config");
    assert_eq!(cfg.provider, RerankProviderKind::Voyage);
}

#[test]
fn voyage_request_helper_shape_frozen() {
    // Helper-level freeze (pure). Wire-level parity is in
    // `voyage_request_shape_unchanged` (mock endpoint capture).
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_request_shape_unchanged() {
    // Wire-level: mock Voyage endpoint captures the actual POST body from
    // `rerank_voyage` (not just the pure helper). RED if the live request
    // shape drifts from the frozen pre-seam body.
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let _guard = crate::test_support::global_test_lock().lock();

    #[derive(Clone, Default)]
    struct Capture {
        hits: usize,
        last_body: Option<Value>,
        last_auth: Option<String>,
    }
    let capture = Arc::new(Mutex::new(Capture::default()));
    let app = Router::new()
        .route(
            "/v1/rerank",
            post(
                |State(cap): State<Arc<Mutex<Capture>>>,
                 headers: axum::http::HeaderMap,
                 body: Json<Value>| async move {
                    let mut g = cap.lock().unwrap_or_else(|e| e.into_inner());
                    g.hits += 1;
                    g.last_body = Some(body.0);
                    g.last_auth = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    Json(json!({
                        "data": [
                            {"index": 0, "relevance_score": 0.9},
                            {"index": 1, "relevance_score": 0.1},
                        ]
                    }))
                },
            ),
        )
        .with_state(capture.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind voyage mock");
    let port = listener.local_addr().expect("addr").port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serve");
    });

    let endpoint = format!("http://127.0.0.1:{port}/v1/rerank");
    let _provider = EnvRestore::unset(RERANK_PROVIDER_ENV);
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);
    let _voyage_url = EnvRestore::set(RERANK_VOYAGE_ENDPOINT_ENV, &endpoint);
    let _vk = EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _vrk = EnvRestore::unset("VOYAGE_RERANK_API_KEY");

    let client = LlmClient::new().expect("client");
    let docs = vec!["alpha".to_string(), "beta".to_string()];
    let out = client
        .rerank("probe query", &docs, 2)
        .await
        .expect("voyage mock should succeed");
    assert_eq!(out, vec![(0, 0.9), (1, 0.1)]);
    assert_eq!(
        client.last_rerank_dispatch_for_tests(),
        Some(RerankProviderKind::Voyage),
        "default path must enter voyage arm via provider enum"
    );

    let g = capture.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(g.hits, 1, "request must hit the voyage mock endpoint");
    let body = g.last_body.as_ref().expect("captured body");
    assert_eq!(body["model"], json!("rerank-2.5"));
    assert_eq!(body["query"], json!("probe query"));
    assert_eq!(body["documents"], json!(["alpha", "beta"]));
    assert_eq!(body["top_k"], json!(2));
    let obj = body.as_object().expect("object");
    assert_eq!(obj.len(), 4, "wire body must keep exactly four pre-seam keys");
    assert_eq!(
        g.last_auth.as_deref(),
        Some("Bearer test-voyage-key"),
        "wire auth must use the voyage bearer key"
    );

    server_task.abort();
}

#[test]
fn unknown_rerank_provider_fails_closed() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "cohere");

    let err = RerankConfig::from_env().expect_err("unknown provider must fail");
    assert!(err.contains("unknown rerank provider"));
    // Eager: construction must also refuse unknown provider (never mid-search).
    let client_err = match LlmClient::new() {
        Ok(_) => panic!("unknown provider must fail at construction"),
        Err(e) => e,
    };
    assert!(client_err.contains("unknown rerank provider"));
}

#[test]
fn local_rerank_unconfigured_fails_at_construction() {
    // Eager config validation: local without endpoint must fail at
    // provider construction, never mid-search as a silent hybrid fallback.
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "local");
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);

    let err = RerankConfig::from_env().expect_err("local without endpoint must fail");
    assert_eq!(err, "local rerank provider not configured");
    let client_err = match LlmClient::new() {
        Ok(_) => panic!("unconfigured local must fail at construction"),
        Err(e) => e,
    };
    assert_eq!(client_err, "local rerank provider not configured");
}

#[test]
fn local_rerank_unconfigured_does_not_fall_back_to_voyage() {
    // Fail closed at construction: even with a voyage key present, local
    // without endpoint never constructs a client that could hit voyage.
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::set(RERANK_PROVIDER_ENV, "local");
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);
    let _voyage = EnvRestore::set("VOYAGE_API_KEY", "should-not-be-used");

    let err = match LlmClient::new() {
        Ok(_) => panic!("must not silent-fallback to voyage"),
        Err(e) => e,
    };
    assert_eq!(err, "local rerank provider not configured");
    assert!(!err.to_ascii_lowercase().contains("voyage"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn local_rerank_hits_configured_endpoint() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let _guard = crate::test_support::global_test_lock().lock();

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
    assert_eq!(
        client.last_rerank_dispatch_for_tests(),
        Some(RerankProviderKind::Local),
        "local config must enter local arm via provider enum"
    );

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
    // Discrimination: must RED if the provider enum match is bypassed.
    // Asserts the arm actually entered (dispatch counter), not just "no error"
    // or a missing-key string that a hardcoded voyage call could also produce.
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::unset(RERANK_PROVIDER_ENV);
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);
    let _voyage_url = EnvRestore::unset(RERANK_VOYAGE_ENDPOINT_ENV);
    let _vk = EnvRestore::unset("VOYAGE_API_KEY");
    let _vrk = EnvRestore::unset("VOYAGE_RERANK_API_KEY");

    let client = LlmClient::new().expect("client");
    assert_eq!(
        client.rerank_config().provider,
        RerankProviderKind::Voyage,
        "default construction must resolve voyage provider"
    );
    // Clear any vault-backed secrets that might exist on the host.
    client.clear_provider_secrets();
    let docs = vec!["a".to_string(), "b".to_string()];
    let err = client
        .rerank("q", &docs, 1)
        .await
        .expect_err("voyage arm without key must fail with missing-key error");
    assert_eq!(
        client.last_rerank_dispatch_for_tests(),
        Some(RerankProviderKind::Voyage),
        "dispatch must go through provider enum voyage arm (counter would be \
         None/Local if the seam were bypassed or mis-routed)"
    );
    assert!(
        err.contains("Missing API key") || err.contains("VOYAGE"),
        "default path must enter voyage arm, got: {err}"
    );
    assert!(
        !err.contains("local rerank provider not configured"),
        "default must not hit local arm"
    );
}
