use super::*;

#[test]
#[allow(clippy::await_holding_lock)]
fn foundry_lanes_use_deepseek_defaults_when_only_deepseek_key_is_configured() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env_guards = [
        EnvRestore::unset("DISTILL_API_KEY"),
        EnvRestore::unset("REASONING_API_KEY"),
        EnvRestore::unset("ZAI_API_KEY"),
        EnvRestore::unset("BIGMODEL_API_KEY"),
        EnvRestore::unset("EXTRACT_API_KEY"),
        EnvRestore::unset("SILICONFLOW_API_KEY"),
        EnvRestore::unset("DISTILL_BASE_URL"),
        EnvRestore::unset("EXTRACT_BASE_URL"),
        EnvRestore::unset("SILICONFLOW_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_DISTILL_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_REASONING_BASE_URL"),
        EnvRestore::unset("DISTILL_MODEL"),
        EnvRestore::unset("EXTRACT_MODEL"),
        EnvRestore::unset("SILICONFLOW_MODEL"),
        EnvRestore::unset("DEEPSEEK_MODEL"),
        EnvRestore::unset("DEEPSEEK_DISTILL_MODEL"),
        EnvRestore::unset("DEEPSEEK_REASONING_MODEL"),
        EnvRestore::unset("TACHI_BACKEND_DISTILL_TIER"),
        EnvRestore::unset("TACHI_BACKEND_REASONING_TIER"),
    ];
    let _deepseek = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-test-key");
    let _stale_reasoning_base = EnvRestore::set(
        "REASONING_BASE_URL",
        "https://api.siliconflow.cn/v1/chat/completions",
    );
    let _stale_reasoning_model = EnvRestore::set("REASONING_MODEL", "Qwen/Qwen3.5-27B");

    let client = LlmClient::new().expect("client should initialize");
    let distill = client.lane(ChatLane::Distill);
    let reasoning = client.lane(ChatLane::Reasoning);

    assert_eq!(
        distill.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(distill.model, "deepseek-chat");
    assert_eq!(
        client.provider_key_id_for_tests(&distill.api_key_envs),
        Some("DEEPSEEK_API_KEY".to_string())
    );

    assert_eq!(
        reasoning.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(reasoning.model, "deepseek-reasoner");
    assert_eq!(
        client.provider_key_id_for_tests(&reasoning.api_key_envs),
        Some("DEEPSEEK_API_KEY".to_string())
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn generate_summary_propagates_llm_failures() {
    let _guard = crate::test_support::global_test_lock().lock();
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

    let _guard = crate::test_support::global_test_lock().lock();
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

    let _guard = crate::test_support::global_test_lock().lock();

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
async fn chat_lane_records_success_usage_to_vault_db() {
    use axum::{routing::post, Json, Router};

    let _guard = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            Json(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "usage recorded"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10
                }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });

    let db = tempfile::NamedTempFile::new().expect("usage db");
    let _base_guard = EnvRestore::set(
        "EXTRACT_BASE_URL",
        format!("http://127.0.0.1:{port}/chat/completions"),
    );
    let _model_guard = EnvRestore::set("EXTRACT_MODEL", "mock-usage-model");
    let _key_guard = EnvRestore::set("EXTRACT_API_KEY", "test-key");

    let client = LlmClient::new_with_vault_db(Some(db.path())).expect("client should initialize");
    let out = client
        .call_extract_llm("system", "user payload", None, 0.0, 16)
        .await
        .expect("mock provider should succeed");
    assert_eq!(out, "usage recorded");

    let mut row = None;
    for _ in 0..40 {
        let conn = rusqlite::Connection::open(db.path()).expect("open usage db");
        let table_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'llm_usage'",
                [],
                |r| r.get(0),
            )
            .expect("query sqlite_master");
        if table_exists > 0 {
            let result = conn.query_row(
                "SELECT lane, model, provider_host, provider_logical_name, provider_key_id,
                        prompt_tokens, completion_tokens, total_tokens, max_tokens,
                        request_chars, response_chars
                 FROM llm_usage
                 ORDER BY id DESC
                 LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                        r.get::<_, i64>(10)?,
                    ))
                },
            );
            if let Ok(value) = result {
                row = Some(value);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (
        lane,
        model,
        provider_host,
        provider_logical_name,
        provider_key_id,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        max_tokens,
        request_chars,
        response_chars,
    ) = row.expect("usage row should be persisted");
    assert_eq!(lane, "extract");
    assert_eq!(model, "mock-usage-model");
    assert_eq!(provider_host, "127.0.0.1");
    assert_eq!(provider_logical_name, "EXTRACT_API_KEY");
    assert_eq!(provider_key_id, "EXTRACT_API_KEY");
    assert_eq!(prompt_tokens, Some(7));
    assert_eq!(completion_tokens, Some(3));
    assert_eq!(total_tokens, Some(10));
    assert_eq!(max_tokens, 16);
    assert_eq!(request_chars, "user payload".chars().count() as i64);
    assert_eq!(response_chars, "usage recorded".chars().count() as i64);

    server_task.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn chat_lane_marks_insufficient_balance_as_exhausted() {
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Router};

    let _guard = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                StatusCode::FORBIDDEN,
                r#"{"message":"account balance is insufficient"}"#,
            )
                .into_response()
        }),
    );
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

    let err = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("insufficient balance should still fail this call");
    assert!(err.contains("balance is insufficient"), "got: {err}");

    let status = client
        .provider_pool_statuses()
        .into_iter()
        .find(|status| status.logical_name == "EXTRACT_API_KEY")
        .expect("provider pool status should include extract key");
    assert_eq!(status.available_keys, 0);
    assert_eq!(status.rate_limited_keys.len(), 1);
    assert_eq!(status.rate_limited_keys[0].key_id, "EXTRACT_API_KEY_1");
    assert_eq!(status.rate_limited_keys[0].remaining_seconds, 0);
    let health = client
        .provider_key_health_for_tests("EXTRACT_API_KEY", "EXTRACT_API_KEY_1")
        .expect("key health should be recorded");
    assert_eq!(health.status, "exhausted");
    assert!(!health.auth_failed);
    assert!(health.cooldown_until.is_none());
    assert!(
        health
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("balance is insufficient")),
        "last_error should explain the exhausted balance: {health:?}"
    );

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

    let _guard = crate::test_support::global_test_lock().lock();

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
