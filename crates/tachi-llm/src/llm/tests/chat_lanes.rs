use super::super::{ChatLaneConfig, LaneFallbackConfig, ProviderRuntimeConfig};
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
async fn generate_summary_propagates_llm_failures() {
    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["__LEAF_2B_UNUSED_KEY"],
        },
        summary: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["__LEAF_2B_MISSING_SUMMARY_KEY"],
        },
        reasoning: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["__LEAF_2B_UNUSED_KEY"],
        },
        distill: ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["__LEAF_2B_UNUSED_KEY"],
        },
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    };
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
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
async fn chat_lane_reports_response_body_read_errors() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
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
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
    client.set_provider_secret_pool(
        "EXTRACT_API_KEY",
        vec![ProviderSecret {
            key_id: "EXTRACT_API_KEY".to_string(),
            value: "test-key".to_string(),
        }],
    );

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
async fn chat_lane_waits_for_temporarily_unavailable_pool_key() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

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

    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
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
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
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
async fn chat_lane_records_success_usage_to_vault_db() {
    use axum::{routing::post, Json, Router};

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
    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-usage-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
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
    let client =
        LlmClient::new_with_config(config, Some(db.path())).expect("client should initialize");
    client.set_provider_secret_pool(
        "EXTRACT_API_KEY",
        vec![ProviderSecret {
            key_id: "EXTRACT_API_KEY".to_string(),
            value: "test-key".to_string(),
        }],
    );
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

/// #1071 fix-round checkpoints 5/6: a lane response whose `finish_reason`
/// is `"length"` (provider cut it off) must be surfaced as `truncated`, and
/// a request that used the lane (not the claude-cli path) must honestly
/// report `used_fallback: true` — the exact two signals the codex
/// 2026-07-17 review found `call_reasoning_llm`'s old `Ok(answer) =>
/// engine_receipt { fallback: false }` path could never produce. The
/// claude-cli path is forced into its failure-cooldown state first so this
/// test is deterministic regardless of whether a `claude` binary happens to
/// be on the test host's `PATH`.
#[tokio::test]
async fn call_reasoning_llm_with_receipt_flags_truncation_and_fallback_honestly() {
    use axum::{routing::post, Json, Router};

    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            Json(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "cut off mid-thou"
                        },
                        "finish_reason": "length"
                    }
                ],
                "usage": {
                    "prompt_tokens": 5,
                    "completion_tokens": 5,
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
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-reasoning-model".to_string(),
            api_key_envs: vec!["REASONING_API_KEY"],
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
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
    client.set_provider_secret_pool(
        "REASONING_API_KEY",
        vec![ProviderSecret {
            key_id: "REASONING_API_KEY".to_string(),
            value: "test-key".to_string(),
        }],
    );
    // Force the claude-cli path into its failure cooldown so the mock lane
    // above deterministically serves the request (see `ClaudeCliFailureKind
    // ::from_error`'s recognized-prefix match for why this exact prefix).
    client.record_claude_cli_failure_at(
        "claude cli spawn failed: forced for deterministic test",
        std::time::Instant::now(),
    );

    let outcome = client
        .call_reasoning_llm_with_receipt("system", "user", None, 0.0, 16)
        .await
        .expect("mock provider should succeed");

    assert_eq!(outcome.text, "cut off mid-thou");
    assert!(
        outcome.truncated,
        "finish_reason=length must be surfaced as truncated, not silently absorbed"
    );
    assert!(
        outcome.used_fallback,
        "the lane path (not claude-cli) served this request — must report fallback: true"
    );

    server_task.abort();
}

/// #1087: `call_reasoning_llm_provider_only` must be a pure HTTP round-trip
/// with zero Claude-CLI involvement — contrast this with the test above
/// (`call_reasoning_llm_with_receipt_flags_truncation_and_fallback_honestly`),
/// which has to force the claude-cli path into its failure cooldown via
/// `record_claude_cli_failure_at` to get a deterministic result, because
/// `call_reasoning_llm_with_receipt` tries a real `claude` subprocess FIRST.
/// This test needs no such ceremony: no cooldown forced, `CLAUDE_BIN`/PATH
/// untouched, yet the mock HTTP lane deterministically serves the request —
/// because this method never consults the CLI at all.
#[tokio::test]
async fn call_reasoning_llm_provider_only_is_pure_http_no_cli_ceremony_needed() {
    use axum::{routing::post, Json, Router};

    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            Json(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "provider-only answer"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                },
                "model": "provider-returned-model",
                "system_fingerprint": "provider-returned-version"
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
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-reasoning-only-model".to_string(),
            api_key_envs: vec!["REASONING_API_KEY"],
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
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
    client.set_provider_secret_pool(
        "REASONING_API_KEY",
        vec![ProviderSecret {
            key_id: "REASONING_API_KEY".to_string(),
            value: "test-key".to_string(),
        }],
    );

    let outcome = client
        .call_reasoning_llm_provider_only_with_receipt("system", "user", None, 0.0, 16)
        .await
        .expect("provider-only call should succeed from the mock lane alone");

    assert_eq!(outcome.text, "provider-only answer");
    assert_eq!(
        outcome.receipt.effective_model.as_deref(),
        Some("provider-returned-model"),
        "the receipt must use the model actually returned by the provider"
    );
    assert_eq!(
        outcome.receipt.effective_version.as_deref(),
        Some("provider-returned-version"),
        "a missing provider fingerprint must stay unknown rather than be invented"
    );
    assert_eq!(outcome.receipt.total_tokens, Some(2));
    assert!(
        !outcome.receipt.degraded && outcome.receipt.fallback_chain.is_empty(),
        "the primary mock tier must not be misreported as a fallback"
    );

    server_task.abort();
}

#[tokio::test]
async fn chat_lane_marks_insufficient_balance_as_exhausted() {
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Router};

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

    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
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
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
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
    assert!(err.contains("class=billing_or_quota"), "got: {err}");
    assert!(err.contains("provider response redacted"), "got: {err}");
    assert!(!err.contains("balance is insufficient"), "got: {err}");

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
            .is_some_and(|error| error.contains("class=billing_or_quota")
                && !error.contains("balance is insufficient")),
        "last_error should classify but redact the provider body: {health:?}"
    );

    server_task.abort();
}

#[tokio::test]
async fn provider_error_boundary_redacts_echoed_prompt_source_and_secret() {
    use axum::{http::HeaderMap, http::StatusCode, response::IntoResponse, routing::post, Router};

    const SOURCE_SENTINEL: &str = "OWNER_SOURCE_MUST_NOT_ESCAPE";
    const SYSTEM_SENTINEL: &str = "SYSTEM_PROMPT_MUST_NOT_ESCAPE";
    const SECRET_SENTINEL: &str = "provider-secret-must-not-escape";
    const BODY_SENTINEL: &str = "RAW_PROVIDER_BODY_MUST_NOT_ESCAPE";

    let app = Router::new().route(
        "/chat/completions",
        post(|headers: HeaderMap, body: String| async move {
            let authorization = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            (
                StatusCode::BAD_REQUEST,
                format!("{BODY_SENTINEL} auth={authorization} request={body}"),
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

    let client = LlmClient::new_with_config(
        config_with_extract(ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
        }),
        None,
    )
    .expect("client should initialize");
    client.set_provider_secret_pool(
        "EXTRACT_API_KEY",
        vec![ProviderSecret {
            key_id: "EXTRACT_API_KEY_1".to_string(),
            value: SECRET_SENTINEL.to_string(),
        }],
    );

    let err = client
        .call_extract_llm(SYSTEM_SENTINEL, SOURCE_SENTINEL, None, 0.0, 16)
        .await
        .expect_err("provider rejection must remain loud");
    let outage = client
        .provider_health_status()
        .lane_outages
        .into_iter()
        .find(|status| status.lane == "extract")
        .and_then(|status| status.last_error)
        .expect("outage aggregation must retain a safe failure");

    for unsafe_value in [
        SOURCE_SENTINEL,
        SYSTEM_SENTINEL,
        SECRET_SENTINEL,
        BODY_SENTINEL,
    ] {
        assert!(!err.contains(unsafe_value), "unsafe provider error: {err}");
        assert!(
            !outage.contains(unsafe_value),
            "unsafe outage aggregation: {outage}"
        );
    }
    assert!(err.contains("provider response redacted"), "got: {err}");

    server_task.abort();
}

#[tokio::test]
async fn chat_lane_retries_with_next_pool_key_after_429() {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

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

    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
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
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
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

#[test]
fn sanitize_llm_keyword_bounds_and_rejects_noise() {
    assert_eq!(
        LlmClient::sanitize_llm_keyword("  ok-term  ").as_deref(),
        Some("ok-term")
    );
    assert_eq!(
        LlmClient::sanitize_llm_keyword("a\u{0001}b").as_deref(),
        Some("ab")
    );
    assert!(LlmClient::sanitize_llm_keyword("!!!").is_none());
    assert!(LlmClient::sanitize_llm_keyword("   ").is_none());
    let long = "z".repeat(80);
    let got = LlmClient::sanitize_llm_keyword(&long).expect("truncate");
    assert_eq!(got.chars().count(), 64);
}

// ── #1096 seam: construction-time config injection ──────────────────────────

/// RED before seam: `new_with_config` and `ProviderRuntimeConfig` did not
/// exist. Uses pure literals — no env, no global_test_lock — and asserts
/// `lane()` / `rerank_config()` return exactly the injected values.
#[test]
fn new_with_config_injects_literal_config_without_env() {
    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: "https://extract.test/v1/chat/completions".to_string(),
            model: "extract-test-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY"],
        },
        summary: ChatLaneConfig {
            base_url: "https://summary.test/v1/chat/completions".to_string(),
            model: "summary-test-model".to_string(),
            api_key_envs: vec!["SUMMARY_API_KEY", "EXTRACT_API_KEY"],
        },
        reasoning: ChatLaneConfig {
            base_url: "https://reasoning.test/v1/chat/completions".to_string(),
            model: "reasoning-test-model".to_string(),
            api_key_envs: vec!["REASONING_API_KEY"],
        },
        distill: ChatLaneConfig {
            base_url: "https://distill.test/v1/chat/completions".to_string(),
            model: "distill-test-model".to_string(),
            api_key_envs: vec!["DISTILL_API_KEY"],
        },
        rerank: RerankConfig {
            provider: RerankProviderKind::Local,
            local_endpoint: Some("http://localhost:8080/rerank".to_string()),
        },
    };
    let client =
        LlmClient::new_with_config(config, None).expect("literal config should construct client");

    assert_eq!(
        client.lane(ChatLane::Extract).base_url,
        "https://extract.test/v1/chat/completions"
    );
    assert_eq!(client.lane(ChatLane::Extract).model, "extract-test-model");
    assert_eq!(
        client.lane(ChatLane::Extract).api_key_envs,
        vec!["EXTRACT_API_KEY"]
    );
    assert_eq!(
        client.lane(ChatLane::Summary).base_url,
        "https://summary.test/v1/chat/completions"
    );
    assert_eq!(client.lane(ChatLane::Summary).model, "summary-test-model");
    assert_eq!(
        client.lane(ChatLane::Reasoning).base_url,
        "https://reasoning.test/v1/chat/completions"
    );
    assert_eq!(
        client.lane(ChatLane::Reasoning).model,
        "reasoning-test-model"
    );
    assert_eq!(
        client.lane(ChatLane::Distill).base_url,
        "https://distill.test/v1/chat/completions"
    );
    assert_eq!(client.lane(ChatLane::Distill).model, "distill-test-model");
    assert_eq!(client.rerank_config().provider, RerankProviderKind::Local);
    assert_eq!(
        client.rerank_config().local_endpoint.as_deref(),
        Some("http://localhost:8080/rerank")
    );
}

/// Golden-value oracle: set explicit env literals and assert every resolved
/// field **equals a hand-written expected value** (not another `from_env()`
/// call). This catches dropped fallback keys, changed trim semantics, and
/// tier-default regressions that a tautological `from_env == from_env` test
/// would silently share.
///
/// Coverage:
/// - **Primary key**: `EXTRACT_API_KEY` set → extract lane uses it.
/// - **Fallback key**: `SUMMARY_API_KEY` unset → summary falls back through
///   to `EXTRACT_API_KEY`.
/// - **Whitespace trim/skip**: `SUMMARY_BASE_URL="   "` → trimmed to empty →
///   skipped → falls back to `EXTRACT_BASE_URL`.
/// - **DeepSeek provider default**: `DEEPSEEK_API_KEY` set → reasoning/distill
///   resolve via the DeepSeek default URL/model chain.
/// - **Explicit rerank**: `local` provider with explicit endpoint.
#[test]
fn from_env_golden_values() {
    let _guard = crate::test_support::global_test_lock().lock();

    // ── Env setup ──
    let _ek = EnvRestore::set("EXTRACT_API_KEY", "golden-extract-key");
    let _eb = EnvRestore::set(
        "EXTRACT_BASE_URL",
        "https://golden.test/extract/chat/completions",
    );
    let _em = EnvRestore::set("EXTRACT_MODEL", "golden-extract-model");
    // Whitespace-only → must be trimmed and skipped, falling back.
    let _sb = EnvRestore::set("SUMMARY_BASE_URL", "   ");
    let _dk = EnvRestore::set("DEEPSEEK_API_KEY", "golden-deepseek");
    let _rp = EnvRestore::set(RERANK_PROVIDER_ENV, "local");
    let _re = EnvRestore::set(RERANK_LOCAL_ENDPOINT_ENV, "https://golden.test/rerank");

    let _cleanup = [
        EnvRestore::unset("SUMMARY_API_KEY"),
        EnvRestore::unset("DISTILL_API_KEY"),
        EnvRestore::unset("REASONING_API_KEY"),
        EnvRestore::unset("ZAI_API_KEY"),
        EnvRestore::unset("BIGMODEL_API_KEY"),
        EnvRestore::unset("SILICONFLOW_API_KEY"),
        // Base URLs
        EnvRestore::unset("SILICONFLOW_BASE_URL"),
        EnvRestore::unset("EXTRACTOR_BASE_URL"),
        EnvRestore::unset("DISTILL_BASE_URL"),
        EnvRestore::unset("REASONING_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_DISTILL_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_REASONING_BASE_URL"),
        // Models
        EnvRestore::unset("SUMMARY_MODEL"),
        EnvRestore::unset("SILICONFLOW_MODEL"),
        EnvRestore::unset("EXTRACTOR_MODEL"),
        EnvRestore::unset("DISTILL_MODEL"),
        EnvRestore::unset("REASONING_MODEL"),
        EnvRestore::unset("DEEPSEEK_MODEL"),
        EnvRestore::unset("DEEPSEEK_DISTILL_MODEL"),
        EnvRestore::unset("DEEPSEEK_REASONING_MODEL"),
        // Tier overrides
        EnvRestore::unset("TACHI_BACKEND_EXTRACT_TIER"),
        EnvRestore::unset("TACHI_BACKEND_SUMMARY_TIER"),
        EnvRestore::unset("TACHI_BACKEND_REASONING_TIER"),
        EnvRestore::unset("TACHI_BACKEND_DISTILL_TIER"),
    ];

    let config = ProviderRuntimeConfig::from_env().expect("from_env should succeed");

    // ── Extract: explicit primary key + explicit base/model ──
    assert_eq!(
        config.extract.api_key_envs,
        vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]
    );
    assert_eq!(
        config.extract.base_url,
        "https://golden.test/extract/chat/completions"
    );
    assert_eq!(config.extract.model, "golden-extract-model");

    // ── Summary: SUMMARY_API_KEY unset → fallback chain to EXTRACT_API_KEY.
    //    SUMMARY_BASE_URL="   " → trimmed → skipped → EXTRACT_BASE_URL fallback.
    //    SUMMARY_MODEL unset → EXTRACT_MODEL fallback. ──
    assert_eq!(
        config.summary.api_key_envs,
        vec!["SUMMARY_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]
    );
    assert_eq!(
        config.summary.base_url,
        "https://golden.test/extract/chat/completions"
    );
    assert_eq!(config.summary.model, "golden-extract-model");

    // ── Reasoning: DEEPSEEK_API_KEY triggers DeepSeek provider default.
    //    No explicit base/model → default URL + deepseek-reasoner. ──
    assert_eq!(
        config.reasoning.api_key_envs,
        vec![
            "DEEPSEEK_API_KEY",
            "REASONING_API_KEY",
            "ZAI_API_KEY",
            "BIGMODEL_API_KEY",
            "DISTILL_API_KEY",
            "EXTRACT_API_KEY",
            "SILICONFLOW_API_KEY"
        ]
    );
    assert_eq!(
        config.reasoning.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(config.reasoning.model, "deepseek-reasoner");

    // ── Distill: same DeepSeek provider default path, deepseek-chat model. ──
    assert_eq!(
        config.distill.api_key_envs,
        vec![
            "DISTILL_API_KEY",
            "DEEPSEEK_API_KEY",
            "REASONING_API_KEY",
            "ZAI_API_KEY",
            "BIGMODEL_API_KEY",
            "EXTRACT_API_KEY",
            "SILICONFLOW_API_KEY"
        ]
    );
    assert_eq!(
        config.distill.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(config.distill.model, "deepseek-chat");

    // ── Rerank: local provider with explicit endpoint. ──
    assert_eq!(config.rerank.provider, RerankProviderKind::Local);
    assert_eq!(
        config.rerank.local_endpoint.as_deref(),
        Some("https://golden.test/rerank")
    );
}

// ── #1096 R2: fail-closed rerank injection (construction-time validation) ──

/// RED before R2 fix 3: `new_with_config` accepted `Local` rerank with no
/// endpoint, deferring failure to runtime. Now it must Err at construction.
#[test]
fn new_with_config_rejects_local_rerank_without_endpoint() {
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
            local_endpoint: None,
        },
    };
    // Not `expect_err`: that would require `LlmClient: Debug`, and deriving
    // Debug on the client is a secret-exposure surface we deliberately avoid.
    let err = match LlmClient::new_with_config(config, None) {
        Ok(_) => panic!("local rerank without endpoint must fail at construction"),
        Err(err) => err,
    };
    assert!(
        err.contains("local rerank provider not configured"),
        "got: {err}"
    );
}

/// Whitespace-only endpoint must also be rejected (same as missing).
#[test]
fn new_with_config_rejects_local_rerank_with_whitespace_endpoint() {
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
            local_endpoint: Some("   ".to_string()),
        },
    };
    let err = match LlmClient::new_with_config(config, None) {
        Ok(_) => panic!("whitespace-only endpoint must fail at construction"),
        Err(err) => err,
    };
    assert!(
        err.contains("local rerank provider not configured"),
        "got: {err}"
    );
}

// ── #1197: cross-provider fallback chain + lane-outage alerting ─────────────

fn unused_lane(key_env: &'static str) -> ChatLaneConfig {
    ChatLaneConfig {
        base_url: "https://unused.test/v1/chat/completions".to_string(),
        model: "unused".to_string(),
        api_key_envs: vec![key_env],
    }
}

fn config_with_extract(extract: ChatLaneConfig) -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        extract,
        summary: unused_lane("__1197_UNUSED_SUMMARY"),
        reasoning: unused_lane("__1197_UNUSED_REASONING"),
        distill: unused_lane("__1197_UNUSED_DISTILL"),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

/// #1197 BUG-1 (codex review, must-fix): a single bad key in the primary
/// pool must NOT immediately escalate to the fallback tier — the contract
/// is "fallback fires when the primary POOL is exhausted", not on one 401.
/// Primary pool has key A (bad, 401) + key B (good) — the call must succeed
/// on B, entirely within the primary tier, and the fallback provider must
/// see **zero** requests.
#[tokio::test]
async fn extract_lane_retries_next_pool_key_on_401_before_falling_back() {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    let seen_auth = Arc::new(Mutex::new(Vec::<String>::new()));
    let primary_app = Router::new()
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
                    if auth == "Bearer bad-key-a" {
                        return (StatusCode::UNAUTHORIZED, "invalid api key").into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": "ok from key b"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                    .into_response()
                },
            ),
        )
        .with_state(seen_auth.clone());
    let primary_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind primary mock provider");
    let primary_port = primary_listener
        .local_addr()
        .expect("primary mock addr")
        .port();
    let primary_task = tokio::spawn(async move {
        axum::serve(primary_listener, primary_app)
            .await
            .expect("primary mock provider");
    });

    let fallback_hits = Arc::new(Mutex::new(0usize));
    let fallback_app = Router::new()
        .route(
            "/chat/completions",
            post(|State(hits): State<Arc<Mutex<usize>>>| async move {
                *hits.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "fallback answered"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        )
        .with_state(fallback_hits.clone());
    let fallback_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fallback mock provider");
    let fallback_port = fallback_listener
        .local_addr()
        .expect("fallback mock addr")
        .port();
    let fallback_task = tokio::spawn(async move {
        axum::serve(fallback_listener, fallback_app)
            .await
            .expect("fallback mock provider");
    });

    let primary = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{primary_port}/chat/completions"),
        model: "primary-model".to_string(),
        api_key_envs: vec!["__1197_BUG1_PRIMARY_KEY"],
    };
    let fallback = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{fallback_port}/chat/completions"),
        model: "fallback-model".to_string(),
        api_key_envs: vec!["__1197_BUG1_FALLBACK_KEY"],
    };

    let client = LlmClient::new_with_config_and_fallbacks(
        config_with_extract(primary),
        LaneFallbackConfig {
            extract: Some(fallback),
            ..Default::default()
        },
        None,
    )
    .expect("client should initialize");
    // Pool order matters: key A (bad) is tried first, key B (good) second.
    client.set_provider_secret_pool(
        "__1197_BUG1_PRIMARY_KEY",
        vec![
            ProviderSecret {
                key_id: "__1197_BUG1_PRIMARY_KEY_A".to_string(),
                value: "bad-key-a".to_string(),
            },
            ProviderSecret {
                key_id: "__1197_BUG1_PRIMARY_KEY_B".to_string(),
                value: "good-key-b".to_string(),
            },
        ],
    );
    client.set_provider_secret_pool(
        "__1197_BUG1_FALLBACK_KEY",
        vec![ProviderSecret {
            key_id: "__1197_BUG1_FALLBACK_KEY".to_string(),
            value: "fallback-secret".to_string(),
        }],
    );

    let out = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("key B in the primary pool should serve the request");
    assert_eq!(
        out, "ok from key b",
        "must succeed via the PRIMARY pool's second key, not the fallback provider"
    );

    let seen = seen_auth.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        seen,
        vec![
            "Bearer bad-key-a".to_string(),
            "Bearer good-key-b".to_string()
        ],
        "primary pool must be exhausted key-by-key before any escalation"
    );
    assert_eq!(
        *fallback_hits.lock().unwrap_or_else(|e| e.into_inner()),
        0,
        "fallback must not be touched while the primary pool still has an unexhausted key"
    );

    let status = client
        .provider_health_status()
        .lane_outages
        .into_iter()
        .find(|s| s.lane == "extract")
        .expect("extract lane outage status should exist");
    assert_eq!(
        status.consecutive_chain_failures, 0,
        "a call that succeeded on the primary pool's second key is not an outage"
    );

    primary_task.abort();
    fallback_task.abort();
}

/// #1197 BUG-1, codex round-2 review: `has_usable_secret_readonly` must
/// mirror `select_secret`'s real vault->env fallback chain, not just check
/// "does this key have a vault pool" and stop there. A single key with a
/// bad (auth-failed) vault entry but a good plain-env-var secret must still
/// be found — `select_secret`'s `vault_value.or_else(...)` shape falls
/// through to the env value once the vault pass yields nothing across the
/// whole key list. A probe that gives up the moment it sees *any* vault
/// pool for a key (even a bad one) under-reports availability and triggers
/// a premature tier failure / fallback escalation.
#[tokio::test]
async fn extract_lane_uses_env_fallback_after_bad_vault_entry_before_escalating() {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    const ENV_KEY: &str = "__1197_BUG1B_PRIMARY_KEY";
    let _env_guard = EnvRestore::set(ENV_KEY, "good-env-secret");

    let seen_auth = Arc::new(Mutex::new(Vec::<String>::new()));
    let primary_app = Router::new()
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
                    if auth == "Bearer bad-vault-secret" {
                        return (StatusCode::UNAUTHORIZED, "invalid api key").into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": "ok from env key"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                    .into_response()
                },
            ),
        )
        .with_state(seen_auth.clone());
    let primary_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind primary mock provider");
    let primary_port = primary_listener
        .local_addr()
        .expect("primary mock addr")
        .port();
    let primary_task = tokio::spawn(async move {
        axum::serve(primary_listener, primary_app)
            .await
            .expect("primary mock provider");
    });

    let fallback_hits = Arc::new(Mutex::new(0usize));
    let fallback_app = Router::new()
        .route(
            "/chat/completions",
            post(|State(hits): State<Arc<Mutex<usize>>>| async move {
                *hits.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "fallback answered"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        )
        .with_state(fallback_hits.clone());
    let fallback_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fallback mock provider");
    let fallback_port = fallback_listener
        .local_addr()
        .expect("fallback mock addr")
        .port();
    let fallback_task = tokio::spawn(async move {
        axum::serve(fallback_listener, fallback_app)
            .await
            .expect("fallback mock provider");
    });

    // Single key in the list — same logical key backed by both a vault
    // pool (one bad entry) and a plain env var (good). No *second* key is
    // involved; this isolates the vault->env fallback within one key.
    let primary = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{primary_port}/chat/completions"),
        model: "primary-model".to_string(),
        api_key_envs: vec![ENV_KEY],
    };
    let fallback = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{fallback_port}/chat/completions"),
        model: "fallback-model".to_string(),
        api_key_envs: vec!["__1197_BUG1B_FALLBACK_KEY"],
    };

    let client = LlmClient::new_with_config_and_fallbacks(
        config_with_extract(primary),
        LaneFallbackConfig {
            extract: Some(fallback),
            ..Default::default()
        },
        None,
    )
    .expect("client should initialize");
    client.set_provider_secret_pool(
        ENV_KEY,
        vec![ProviderSecret {
            key_id: format!("{ENV_KEY}_VAULT_1"),
            value: "bad-vault-secret".to_string(),
        }],
    );
    client.set_provider_secret_pool(
        "__1197_BUG1B_FALLBACK_KEY",
        vec![ProviderSecret {
            key_id: "__1197_BUG1B_FALLBACK_KEY".to_string(),
            value: "fallback-secret".to_string(),
        }],
    );

    let out = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("the env-var fallback for the same key should serve the request");
    assert_eq!(
        out, "ok from env key",
        "must succeed via the same key's env-var fallback, not the fallback provider"
    );

    let seen = seen_auth.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        seen,
        vec![
            "Bearer bad-vault-secret".to_string(),
            "Bearer good-env-secret".to_string()
        ],
        "vault entry must be tried first, then the env value — same key, no premature tier failure"
    );
    assert_eq!(
        *fallback_hits.lock().unwrap_or_else(|e| e.into_inner()),
        0,
        "fallback must not be touched — a bad vault entry with a good env fallback \
         is not a pool exhaustion"
    );

    primary_task.abort();
    fallback_task.abort();
}

/// RED against the pre-#1197 single-tier lane: with no fallback wired, an
/// unconfigured primary key fails the whole call. GREEN once the fallback
/// tier is configured: the same call must succeed by falling through to it.
/// Discriminates "fallback exists as declared config" from "fallback is
/// actually consulted when primary can't even select a key" (the #1197 ask's
/// "key 池整体 exhausted" trigger — no HTTP call is made for the primary at
/// all here, since key selection fails before any network I/O).
#[tokio::test]
async fn extract_lane_falls_through_to_fallback_when_primary_key_is_unconfigured() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let hits = Arc::new(Mutex::new(0usize));
    let app = Router::new()
        .route(
            "/chat/completions",
            post(|State(hits): State<Arc<Mutex<usize>>>| async move {
                *hits.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "fallback answered"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        )
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fallback mock provider");
    let port = listener.local_addr().expect("fallback mock addr").port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("fallback mock provider");
    });

    // Primary: a key env that is never configured anywhere — key selection
    // fails immediately (`Missing API key`), no network attempt at all.
    let primary = unused_lane("__1197_UNCONFIGURED_PRIMARY_KEY");
    let fallback = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{port}/chat/completions"),
        model: "fallback-model".to_string(),
        api_key_envs: vec!["__1197_FALLBACK_KEY"],
    };

    let client = LlmClient::new_with_config_and_fallbacks(
        config_with_extract(primary),
        LaneFallbackConfig {
            extract: Some(fallback),
            ..Default::default()
        },
        None,
    )
    .expect("client should initialize");
    client.set_provider_secret_pool(
        "__1197_FALLBACK_KEY",
        vec![ProviderSecret {
            key_id: "__1197_FALLBACK_KEY".to_string(),
            value: "fallback-secret".to_string(),
        }],
    );

    let out = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("fallback tier should serve the request when primary can't select a key");
    assert_eq!(out, "fallback answered");
    assert_eq!(
        *hits.lock().unwrap_or_else(|e| e.into_inner()),
        1,
        "fallback provider should be hit exactly once"
    );

    // A chain that ultimately succeeded (even via fallback) must not read as
    // an outage.
    let status = client
        .provider_health_status()
        .lane_outages
        .into_iter()
        .find(|s| s.lane == "extract")
        .expect("extract lane outage status should exist");
    assert_eq!(status.consecutive_chain_failures, 0);
    assert!(status.fallback_configured);

    server_task.abort();
}

/// Discriminates the *other* #1197 trigger condition: an open circuit
/// breaker, not just an unconfigured key. Primary's mock provider would
/// answer successfully if hit — proving escalation happened because of the
/// breaker, not because primary was actually broken.
#[tokio::test]
async fn extract_lane_escalates_to_fallback_when_primary_breaker_is_open() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let primary_hits = Arc::new(Mutex::new(0usize));
    let primary_app = Router::new()
        .route(
            "/chat/completions",
            post(|State(hits): State<Arc<Mutex<usize>>>| async move {
                *hits.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "primary answered"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        )
        .with_state(primary_hits.clone());
    let primary_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind primary mock provider");
    let primary_port = primary_listener
        .local_addr()
        .expect("primary mock addr")
        .port();
    let primary_task = tokio::spawn(async move {
        axum::serve(primary_listener, primary_app)
            .await
            .expect("primary mock provider");
    });

    let fallback_app = Router::new().route(
        "/chat/completions",
        post(|| async {
            Json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "fallback answered"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
        }),
    );
    let fallback_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fallback mock provider");
    let fallback_port = fallback_listener
        .local_addr()
        .expect("fallback mock addr")
        .port();
    let fallback_task = tokio::spawn(async move {
        axum::serve(fallback_listener, fallback_app)
            .await
            .expect("fallback mock provider");
    });

    let primary = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{primary_port}/chat/completions"),
        model: "primary-model".to_string(),
        api_key_envs: vec!["__1197_BREAKER_PRIMARY_KEY"],
    };
    let fallback = ChatLaneConfig {
        base_url: format!("http://127.0.0.1:{fallback_port}/chat/completions"),
        model: "fallback-model".to_string(),
        api_key_envs: vec!["__1197_BREAKER_FALLBACK_KEY"],
    };

    let client = LlmClient::new_with_config_and_fallbacks(
        config_with_extract(primary),
        LaneFallbackConfig {
            extract: Some(fallback),
            ..Default::default()
        },
        None,
    )
    .expect("client should initialize");
    client.set_provider_secret_pool(
        "__1197_BREAKER_PRIMARY_KEY",
        vec![ProviderSecret {
            key_id: "__1197_BREAKER_PRIMARY_KEY".to_string(),
            value: "primary-secret".to_string(),
        }],
    );
    client.set_provider_secret_pool(
        "__1197_BREAKER_FALLBACK_KEY",
        vec![ProviderSecret {
            key_id: "__1197_BREAKER_FALLBACK_KEY".to_string(),
            value: "fallback-secret".to_string(),
        }],
    );

    // Force the primary tier's breaker open directly — this is the
    // "breaker 开" trigger condition, independent of key availability.
    for _ in 0..5 {
        client.circuit_breakers.record_failure("chat:extract");
    }

    let out = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("fallback tier should serve the request while primary's breaker is open");
    assert_eq!(out, "fallback answered");
    assert_eq!(
        *primary_hits.lock().unwrap_or_else(|e| e.into_inner()),
        0,
        "an open breaker must fast-reject the primary tier — zero HTTP calls to it"
    );

    primary_task.abort();
    fallback_task.abort();
}

/// GREEN-vs-broken: with **no** fallback configured (or a fallback that is
/// equally unusable), an exhausted lane must surface a loud, typed outage
/// error — never a silently swallowed stall — and the failure must be
/// recorded on the queryable outage surface that `tachi_status` reads.
#[tokio::test]
async fn extract_lane_all_tiers_exhausted_reports_loud_typed_outage_not_a_stall() {
    let primary = unused_lane("__1197_ALL_FAIL_PRIMARY_KEY");
    let fallback = unused_lane("__1197_ALL_FAIL_FALLBACK_KEY");

    let client = LlmClient::new_with_config_and_fallbacks(
        config_with_extract(primary),
        LaneFallbackConfig {
            extract: Some(fallback),
            ..Default::default()
        },
        None,
    )
    .expect("client should initialize");

    let err = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("both tiers unconfigured must fail loudly, not hang or return Ok");

    assert!(
        err.contains("LANE OUTAGE"),
        "failure must be a typed, greppable outage signal, got: {err}"
    );
    assert!(err.contains("extract"), "got: {err}");
    assert!(
        err.contains("2 configured provider tier"),
        "message should name how many tiers were tried, got: {err}"
    );

    let status = client
        .provider_health_status()
        .lane_outages
        .into_iter()
        .find(|s| s.lane == "extract")
        .expect("extract lane outage status should exist");
    assert_eq!(
        status.consecutive_chain_failures, 1,
        "a fully-exhausted chain must bump the outage streak"
    );
    assert!(status.last_outage_at.is_some());
    assert!(status.last_error.is_some());
}

/// Repeated full-chain exhaustion accumulates a streak (the "连续失败超阈值"
/// signal #1197 wants for alert routing), and a subsequent success clears it
/// — a lane that recovers must not keep reporting a stale outage.
#[tokio::test]
async fn extract_lane_outage_streak_accumulates_and_clears_on_recovery() {
    let primary = unused_lane("__1197_STREAK_PRIMARY_KEY");
    let fallback = unused_lane("__1197_STREAK_FALLBACK_KEY");

    let client = LlmClient::new_with_config_and_fallbacks(
        config_with_extract(primary),
        LaneFallbackConfig {
            extract: Some(fallback),
            ..Default::default()
        },
        None,
    )
    .expect("client should initialize");

    for _ in 0..3 {
        let _ = client
            .call_extract_llm("system", "user", None, 0.0, 16)
            .await;
    }
    assert_eq!(
        client
            .provider_health_status()
            .lane_outages
            .into_iter()
            .find(|s| s.lane == "extract")
            .expect("extract lane outage status should exist")
            .consecutive_chain_failures,
        3,
        "three consecutive full-chain failures should accumulate a streak of 3"
    );

    // A lane recovering — via a fallback tier succeeding, or (as asserted
    // directly here against the tracker itself) any tier at all — must clear
    // the streak, not just decrement it. `record_chain_success` is exactly
    // what `call_lane_llm` calls on the first tier that answers.
    client.lane_outage.record_chain_success("extract");
    assert_eq!(
        client
            .provider_health_status()
            .lane_outages
            .into_iter()
            .find(|s| s.lane == "extract")
            .expect("extract lane outage status should exist")
            .consecutive_chain_failures,
        0,
        "a success must clear the outage streak, not just decrement it"
    );
}

/// Golden-value oracle for `LaneFallbackConfig::from_env` (#1197): the
/// concrete worked example from the issue — extract's primary is
/// SiliconFlow, its cross-provider fallback is DeepSeek when
/// `DEEPSEEK_API_KEY` is configured and no explicit `EXTRACT_FALLBACK_*`
/// override is set. Also covers the inverse for a foundry lane (distill),
/// whose fallback is SiliconFlow.
#[test]
fn lane_fallback_config_from_env_resolves_deepseek_default_for_extract() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _deepseek = EnvRestore::set("DEEPSEEK_API_KEY", "golden-deepseek-fallback-key");
    let _siliconflow = EnvRestore::set("SILICONFLOW_API_KEY", "golden-siliconflow-fallback-key");
    let _cleanup = [
        EnvRestore::unset("EXTRACT_FALLBACK_API_KEY"),
        EnvRestore::unset("EXTRACT_FALLBACK_BASE_URL"),
        EnvRestore::unset("EXTRACT_FALLBACK_MODEL"),
        EnvRestore::unset("SUMMARY_FALLBACK_API_KEY"),
        EnvRestore::unset("SUMMARY_FALLBACK_BASE_URL"),
        EnvRestore::unset("SUMMARY_FALLBACK_MODEL"),
        EnvRestore::unset("DISTILL_FALLBACK_API_KEY"),
        EnvRestore::unset("DISTILL_FALLBACK_BASE_URL"),
        EnvRestore::unset("DISTILL_FALLBACK_MODEL"),
        EnvRestore::unset("REASONING_FALLBACK_API_KEY"),
        EnvRestore::unset("REASONING_FALLBACK_BASE_URL"),
        EnvRestore::unset("REASONING_FALLBACK_MODEL"),
        EnvRestore::unset("DEEPSEEK_BASE_URL"),
        EnvRestore::unset("DEEPSEEK_MODEL"),
        EnvRestore::unset("SILICONFLOW_BASE_URL"),
        EnvRestore::unset("SILICONFLOW_MODEL"),
    ];

    let fallbacks = super::super::LaneFallbackConfig::from_env();

    let extract = fallbacks
        .extract
        .expect("extract fallback should resolve to DeepSeek when DEEPSEEK_API_KEY is set");
    assert_eq!(
        extract.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(extract.model, "deepseek-chat");
    assert_eq!(
        extract.api_key_envs,
        vec!["EXTRACT_FALLBACK_API_KEY", "DEEPSEEK_API_KEY"]
    );

    let distill = fallbacks
        .distill
        .expect("distill fallback should resolve to SiliconFlow when SILICONFLOW_API_KEY is set");
    assert_eq!(
        distill.base_url,
        "https://api.siliconflow.cn/v1/chat/completions"
    );
    assert_eq!(distill.model, "Qwen/Qwen3.5-27B");
    assert_eq!(
        distill.api_key_envs,
        vec!["DISTILL_FALLBACK_API_KEY", "SILICONFLOW_API_KEY"]
    );
}

/// A lane with no distinct fallback provider configured must resolve to
/// `None` — #1197's fallback is additive, never invented out of thin air.
#[test]
fn lane_fallback_config_from_env_is_none_when_no_fallback_key_configured() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _cleanup = [
        EnvRestore::unset("DEEPSEEK_API_KEY"),
        EnvRestore::unset("SILICONFLOW_API_KEY"),
        EnvRestore::unset("EXTRACT_FALLBACK_API_KEY"),
        EnvRestore::unset("SUMMARY_FALLBACK_API_KEY"),
        EnvRestore::unset("DISTILL_FALLBACK_API_KEY"),
        EnvRestore::unset("REASONING_FALLBACK_API_KEY"),
    ];

    let fallbacks = super::super::LaneFallbackConfig::from_env();

    assert!(fallbacks.extract.is_none());
    assert!(fallbacks.summary.is_none());
    assert!(fallbacks.distill.is_none());
    assert!(fallbacks.reasoning.is_none());
}
