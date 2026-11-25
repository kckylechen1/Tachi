use super::super::{ChatLaneConfig, ProviderRuntimeConfig};
use super::*;
use crate::llm::{voyage_rerank_model, VOYAGE_RERANK_MODEL_ENV};

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
        parse_voyage_batch_embeddings(&data, 2, 1024).expect("matching indexes should parse");

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

    let err = parse_voyage_batch_embeddings(&data, 2, 1024)
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
    let _model = EnvRestore::unset(VOYAGE_RERANK_MODEL_ENV);

    let cfg = RerankConfig::from_env().expect("default config");
    assert_eq!(cfg.provider, RerankProviderKind::Voyage);
    assert_eq!(cfg.provider_name(), "voyage");
    assert_eq!(cfg.model_name().as_deref(), Some("rerank-3"));
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
    let _guard = crate::test_support::global_test_lock().lock();
    let _model = EnvRestore::unset(VOYAGE_RERANK_MODEL_ENV);
    // Helper-level freeze. Wire-level parity is in
    // `voyage_request_shape_unchanged` (mock endpoint capture).
    let docs = ["alpha".to_string(), "beta".to_string()];
    let refs: Vec<&String> = docs.iter().collect();
    let body = voyage_rerank_request_body("probe query", &refs, 2);
    assert_eq!(body["model"], json!("rerank-3"));
    assert_eq!(body["query"], json!("probe query"));
    assert_eq!(body["documents"], json!(["alpha", "beta"]));
    assert_eq!(body["top_k"], json!(2));
    // Exactly the four keys the pre-seam path sent.
    let obj = body.as_object().expect("object");
    assert_eq!(obj.len(), 4);
}

#[test]
fn voyage_request_model_override_is_trimmed_and_blank_falls_back() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _provider = EnvRestore::unset(RERANK_PROVIDER_ENV);
    let _endpoint = EnvRestore::unset(RERANK_LOCAL_ENDPOINT_ENV);
    let cfg = RerankConfig::from_env().expect("default voyage rerank config");
    let docs = ["alpha".to_string()];
    let refs = docs.iter().collect::<Vec<_>>();
    for (raw, expected) in [(" rerank-3.5 ", "rerank-3.5"), (" \t ", "rerank-3")] {
        let _model = EnvRestore::set(VOYAGE_RERANK_MODEL_ENV, raw);
        assert_eq!(voyage_rerank_model(), expected);
        assert_eq!(cfg.model_name().as_deref(), Some(expected));
        assert_eq!(voyage_rerank_request_body("q", &refs, 1)["model"], expected);
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_request_shape_unchanged() {
    assert_voyage_request_shape(None, "rerank-3").await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_request_sends_the_model_override() {
    assert_voyage_request_shape(Some(" rerank-3.5 "), "rerank-3.5").await;
}

#[allow(clippy::await_holding_lock)]
async fn assert_voyage_request_shape(model_override: Option<&str>, expected_model: &str) {
    // Wire-level: mock Voyage endpoint captures the actual POST body from
    // `rerank_voyage` (not just the pure helper). RED if the live request
    // shape drifts from the frozen pre-seam body.
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

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

    // Lock only the env-dependent section: these vars are read live at call
    // time by rerank_voyage (voyage_rerank_url + secret selection). Provider
    // selection is now via injected config, not env.
    let _guard = crate::test_support::global_test_lock().lock();
    let _voyage_url = EnvRestore::set(RERANK_VOYAGE_ENDPOINT_ENV, &endpoint);
    let _vk = EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _vrk = EnvRestore::unset("VOYAGE_RERANK_API_KEY");
    let _model = match model_override {
        Some(model) => EnvRestore::set(VOYAGE_RERANK_MODEL_ENV, model),
        None => EnvRestore::unset(VOYAGE_RERANK_MODEL_ENV),
    };

    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        summary: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        reasoning: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        distill: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    };
    let client = LlmClient::new_with_config(config, None).expect("client");
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
    assert_eq!(body["model"], json!(expected_model));
    assert_eq!(body["query"], json!("probe query"));
    assert_eq!(body["documents"], json!(["alpha", "beta"]));
    assert_eq!(body["top_k"], json!(2));
    let obj = body.as_object().expect("object");
    assert_eq!(
        obj.len(),
        4,
        "wire body must keep exactly four pre-seam keys"
    );
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
async fn local_rerank_hits_configured_endpoint() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

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
    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        summary: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        reasoning: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        distill: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        },
        rerank: RerankConfig {
            provider: RerankProviderKind::Local,
            local_endpoint: Some(endpoint),
        },
    };

    let client = LlmClient::new_with_config(config, None).expect("client");
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
    client
        .clear_provider_secrets()
        .expect("clear provider secrets");
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

// ---- Upstream response boundary --------------------------------------------

/// A provider answer that names the caller's key. It may steer a retry in
/// memory; it must not leave the client in an error.
const LEAKY_PROVIDER_ANSWER: &str =
    r#"{"error":"key test-voyage-key is not entitled to this model"}"#;

/// A one-route mock that answers every POST with `status` and `body`.
async fn fixed_response_mock(
    path: &'static str,
    status: axum::http::StatusCode,
    body: String,
) -> (String, tokio::task::JoinHandle<()>) {
    use axum::{extract::State, routing::post, Router};
    let app = Router::new()
        .route(
            path,
            post(move |State(body): State<String>| async move { (status, body) }),
        )
        .with_state(body);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixed-response mock");
    let port = listener.local_addr().expect("addr").port();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serve");
    });
    (format!("http://127.0.0.1:{port}"), task)
}

fn unused_lane() -> ChatLaneConfig {
    ChatLaneConfig {
        base_url: "https://unused.test/v1/chat/completions".to_string(),
        model: "unused".to_string(),
        api_key_envs: vec!["UNUSED_API_KEY"],
    }
}

fn recall_client(rerank: RerankConfig) -> LlmClient {
    LlmClient::new_with_config(
        ProviderRuntimeConfig {
            extract: unused_lane(),
            summary: unused_lane(),
            reasoning: unused_lane(),
            distill: unused_lane(),
            rerank,
        },
        None,
    )
    .expect("client")
}

fn voyage_rerank() -> RerankConfig {
    RerankConfig {
        provider: RerankProviderKind::Voyage,
        local_endpoint: None,
    }
}

/// Points Voyage at `base` and names the key a refused answer would leak.
fn voyage_env(base: &str) -> (EnvRestore, EnvRestore, EnvRestore, EnvRestore, EnvRestore) {
    (
        EnvRestore::set(RERANK_VOYAGE_ENDPOINT_ENV, format!("{base}/v1/rerank")),
        EnvRestore::set("VOYAGE_BASE_URL", base),
        EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key"),
        EnvRestore::unset("VOYAGE_RERANK_API_KEY"),
        EnvRestore::unset(VOYAGE_RERANK_MODEL_ENV),
    )
}

async fn rerank_error(client: &LlmClient) -> String {
    match client.rerank("which doc?", &["doc".to_string()], 1).await {
        Ok(_) => panic!("a refused provider answer must fail the rerank"),
        Err(err) => err,
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_rerank_error_does_not_carry_the_upstream_body() {
    let (base, server) = fixed_response_mock(
        "/v1/rerank",
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        LEAKY_PROVIDER_ANSWER.to_string(),
    )
    .await;
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = voyage_env(&base);
    let err = rerank_error(&recall_client(voyage_rerank())).await;
    assert_eq!(
        err,
        format!(
            "Voyage rerank API error: 500 Internal Server Error - provider response redacted ({} bytes)",
            LEAKY_PROVIDER_ANSWER.len()
        )
    );
    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_rerank_auth_error_does_not_carry_the_upstream_body() {
    let (base, server) = fixed_response_mock(
        "/v1/rerank",
        axum::http::StatusCode::FORBIDDEN,
        LEAKY_PROVIDER_ANSWER.to_string(),
    )
    .await;
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = voyage_env(&base);
    let err = rerank_error(&recall_client(voyage_rerank())).await;
    assert_eq!(
        err,
        format!(
            "Voyage rerank API error: 403 Forbidden - provider response redacted ({} bytes)",
            LEAKY_PROVIDER_ANSWER.len()
        )
    );
    server.abort();
}

#[tokio::test]
async fn local_rerank_error_does_not_carry_the_upstream_body() {
    let (base, server) = fixed_response_mock(
        "/rerank",
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        LEAKY_PROVIDER_ANSWER.to_string(),
    )
    .await;
    let client = recall_client(RerankConfig {
        provider: RerankProviderKind::Local,
        local_endpoint: Some(format!("{base}/rerank")),
    });
    let err = rerank_error(&client).await;
    assert_eq!(
        err,
        format!(
            "Local rerank API error: 500 Internal Server Error - provider response redacted ({} bytes)",
            LEAKY_PROVIDER_ANSWER.len()
        )
    );
    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_batch_embedding_error_does_not_carry_the_upstream_body() {
    let (base, server) = fixed_response_mock(
        "/v1/embeddings",
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        LEAKY_PROVIDER_ANSWER.to_string(),
    )
    .await;
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = voyage_env(&base);
    let _model = EnvRestore::unset(crate::llm::embedding_config::EMBEDDING_MODEL_ENV);
    let _voyage_model = EnvRestore::unset(crate::llm::embedding_config::VOYAGE_EMBED_MODEL_ENV);
    let _dim = EnvRestore::unset(crate::llm::embedding_config::EMBEDDING_DIMENSION_ENV);
    let err = match recall_client(voyage_rerank())
        .embed_voyage_batch(&["probe document".to_string()], "document")
        .await
    {
        Ok(_) => panic!("a refused provider answer must fail the batch"),
        Err(err) => err,
    };
    assert_eq!(
        err,
        format!(
            "Voyage batch API error: 500 Internal Server Error - provider response redacted ({} bytes)",
            LEAKY_PROVIDER_ANSWER.len()
        )
    );
    server.abort();
}

/// A provider that answers with more than the daemon is willing to hold for
/// one response is refused at the cap instead of being buffered whole.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_voyage_response_past_the_byte_cap_is_refused() {
    let (base, server) = fixed_response_mock(
        "/v1/rerank",
        axum::http::StatusCode::OK,
        "x".repeat(16 * 1024 * 1024 + 1),
    )
    .await;
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = voyage_env(&base);
    let err = rerank_error(&recall_client(voyage_rerank())).await;
    assert_eq!(
        err,
        "Voyage rerank response body read failed: response body exceeded 16777216 bytes"
    );
    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_voyage_embedding_response_past_the_byte_cap_is_refused() {
    let (base, server) = fixed_response_mock(
        "/v1/embeddings",
        axum::http::StatusCode::OK,
        "x".repeat(16 * 1024 * 1024 + 1),
    )
    .await;
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = voyage_env(&base);
    let _model = EnvRestore::unset(crate::llm::embedding_config::EMBEDDING_MODEL_ENV);
    let _voyage_model = EnvRestore::unset(crate::llm::embedding_config::VOYAGE_EMBED_MODEL_ENV);
    let _dim = EnvRestore::unset(crate::llm::embedding_config::EMBEDDING_DIMENSION_ENV);
    let err = match recall_client(voyage_rerank())
        .embed_voyage_batch(&["probe document".to_string()], "document")
        .await
    {
        Ok(_) => panic!("an oversized answer must fail the batch"),
        Err(err) => err,
    };
    assert_eq!(
        err,
        "Voyage batch response body read failed: response body exceeded 16777216 bytes"
    );
    server.abort();
}
