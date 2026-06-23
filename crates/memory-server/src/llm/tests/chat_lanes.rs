use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn generate_summary_propagates_llm_failures() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    std::env::remove_var("SUMMARY_API_KEY");
    std::env::remove_var("SILICONFLOW_API_KEY");
    std::env::remove_var("EXTRACT_API_KEY");
    std::env::remove_var("REASONING_API_KEY");
    std::env::remove_var("ZAI_API_KEY");
    std::env::remove_var("BIGMODEL_API_KEY");

    let client = LlmClient::new().expect("client should initialize");
    let err = client
        .generate_summary("this text used to be silently truncated")
        .await
        .expect_err("summary should surface provider/key failures");

    assert!(
        err.contains("Missing API key") || err.contains("API key unavailable"),
        "expected provider error, got: {err}"
    );
    assert!(
        !err.contains("this text used to be silently truncated"),
        "summary errors must not return truncated input as success"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn chat_lane_reports_response_body_read_errors() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind broken provider");
    let port = listener.local_addr().expect("provider addr").port();
    let server_task = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let response =
                b"HTTP/1.1 200 OK\r\ncontent-length: 64\r\ncontent-type: application/json\r\n\r\n{\"choices\"";
            let _ = socket.write_all(response).await;
            let _ = socket.shutdown().await;
        }
    });

    let _base_guard = EnvRestore::set(
        "EXTRACT_BASE_URL",
        format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model_guard = EnvRestore::set("EXTRACT_MODEL", "mock-model");
    let _key_guard = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let client = LlmClient::new().expect("client should initialize");
    let err = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("truncated provider body should be a body read error");

    assert!(
        err.contains("Chat response body read failed after HTTP 200 OK"),
        "expected body read error, got: {err}"
    );
    assert!(
        !err.contains("<read error:"),
        "body read failures must not be converted into synthetic body text"
    );

    server_task.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn chat_lane_waits_for_temporarily_unavailable_pool_key() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let seen = Arc::new(Mutex::new(0usize));
    let app = Router::new()
        .route(
            "/chat/completions",
            post(|State(seen): State<Arc<Mutex<usize>>>| async move {
                *seen.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Json(serde_json::json!({
                    "choices": [
                        {
                            "message": {
                                "role": "assistant",
                                "content": "ok after cooldown"
                            },
                            "finish_reason": "stop"
                        }
                    ],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });

    let _base_guard = EnvRestore::set(
        "EXTRACT_BASE_URL",
        format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model_guard = EnvRestore::set("EXTRACT_MODEL", "mock-model");
    let client = LlmClient::new().expect("client should initialize");
    client.set_provider_secret_pool(
        "EXTRACT_API_KEY",
        vec![ProviderSecret {
            key_id: "EXTRACT_API_KEY_1".to_string(),
            value: "pool-secret-one".to_string(),
        }],
    );
    client.mark_provider_key_rate_limited_for_tests("EXTRACT_API_KEY_1", Some(1));

    let out = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("cooldown retry should eventually use the pool key");
    assert_eq!(out, "ok after cooldown");
    assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), 1);

    server_task.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn chat_lane_retries_with_next_pool_key_after_429() {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let seen_auth = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/chat/completions",
            post(
                |State(seen_auth): State<Arc<Mutex<Vec<String>>>>,
                 headers: HeaderMap,
                 Json(_body): Json<Value>| async move {
                    let auth = headers
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    seen_auth
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(auth.clone());
                    if auth == "Bearer pool-secret-one" {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "120")],
                            "rate limited",
                        )
                            .into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [
                            {
                                "message": {
                                    "role": "assistant",
                                    "content": "ok from second key"
                                },
                                "finish_reason": "stop"
                            }
                        ],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                    .into_response()
                },
            ),
        )
        .with_state(seen_auth.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });

    let _base_guard = EnvRestore::set(
        "EXTRACT_BASE_URL",
        format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model_guard = EnvRestore::set("EXTRACT_MODEL", "mock-model");
    let client = LlmClient::new().expect("client should initialize");
    client.set_provider_secret_pool(
        "EXTRACT_API_KEY",
        vec![
            ProviderSecret {
                key_id: "EXTRACT_API_KEY_1".to_string(),
                value: "pool-secret-one".to_string(),
            },
            ProviderSecret {
                key_id: "EXTRACT_API_KEY_2".to_string(),
                value: "pool-secret-two".to_string(),
            },
        ],
    );

    let out = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("second pool key should succeed after first 429");
    assert_eq!(out, "ok from second key");

    let seen = seen_auth.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        seen,
        vec![
            "Bearer pool-secret-one".to_string(),
            "Bearer pool-secret-two".to_string()
        ]
    );

    let status = client
        .provider_pool_statuses()
        .into_iter()
        .find(|status| status.logical_name == "EXTRACT_API_KEY")
        .expect("provider pool status should include extract key");
    assert_eq!(status.total_keys, 2);
    assert_eq!(status.available_keys, 1);
    assert_eq!(status.rate_limited_keys[0].key_id, "EXTRACT_API_KEY_1");
    assert!(status.rate_limited_keys[0].remaining_seconds >= 1);
    assert_eq!(
        client
            .provider_key_id_for_tests(&["EXTRACT_API_KEY"])
            .as_deref(),
        Some("EXTRACT_API_KEY_2")
    );

    let raw = serde_json::to_string(&status).expect("serialize status");
    assert!(!raw.contains("pool-secret-one"));
    assert!(!raw.contains("pool-secret-two"));

    server_task.abort();
}
