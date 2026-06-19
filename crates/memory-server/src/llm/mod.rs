// llm.rs — LLM & Embedding client for memory server
//
// Uses raw reqwest for OpenAI-compatible chat completions.
// SiliconFlow/Qwen still gets `enable_thinking: false` to avoid empty content.

#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use reqwest::header::AUTHORIZATION;
#[cfg(test)]
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
use memory_core::vault::VaultKeyHealth;

mod chat_lanes;
mod embedding;
mod helpers;
mod provider_health;

pub(crate) use provider_health::ProviderSecret;
use provider_health::{
    ChatLaneConfig, ClaudeCliFailure, ProviderHealthPersistState, ProviderHealthReloadState,
    ProviderState,
};
#[cfg(test)]
use provider_health::{
    ClaudeCliFailureKind, KeyAvailability, CLAUDE_CLI_FAILURE_COOLDOWN, HEALTH_OK,
    HEALTH_RATE_LIMITED,
};

/// LLM and embedding client using Voyage API for embeddings
/// and lane-specific OpenAI-compatible chat providers.
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    extract: ChatLaneConfig,
    distill: ChatLaneConfig,
    reasoning: ChatLaneConfig,
    summary: ChatLaneConfig,
    vault_db_path: Option<PathBuf>,
    provider_state: Arc<RwLock<ProviderState>>,
    provider_health_reload: Arc<RwLock<ProviderHealthReloadState>>,
    provider_health_persist: Arc<RwLock<ProviderHealthPersistState>>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
}

#[cfg(test)]
mod tests {
    use super::embedding::{non_empty_rerank_documents, parse_voyage_batch_embeddings};
    use super::*;
    use serde_json::json;

    // NOTE: each test below MUST use a unique env-var name. Cargo runs
    // `#[test]` fns in parallel by default, so sharing a process-wide env var
    // causes ordering-dependent flakes (e.g. one test setting the var while
    // another asserts it's unset). See:
    //   crates/memory-server/src/tests.rs::home_test_lock for the pattern we
    //   use when an env var (HOME) genuinely cannot be uniquified.

    struct EnvRestore {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn llm_client_initializes_without_provider_env() {
        // Unique key — guaranteed never set by any other test or by the host
        // shell, so this test is parallel-safe.
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_INIT_NO_ENV";
        std::env::remove_var(KEY);
        let client = LlmClient::new().expect("client should not require API keys at startup");

        assert!(client.provider_secret_for_tests(&[KEY]).is_none());
        assert!(client
            .required_secret(&[KEY])
            .expect_err("missing keys should fail at call time")
            .contains("Missing API key"));
    }

    #[test]
    fn claude_cli_failure_cache_skips_expensive_discovery_failures() {
        let client = LlmClient::new().expect("client should initialize");
        let failed_at = Instant::now();

        assert!(client.claude_cli_skip_at(failed_at).is_none());

        client.record_claude_cli_failure_at("claude cli spawn failed: not found", failed_at);
        let skip = client
            .claude_cli_skip_at(failed_at + Duration::from_secs(1))
            .expect("spawn failure should suppress immediate retries");
        assert_eq!(skip.kind, ClaudeCliFailureKind::SpawnFailed);
        assert!(skip.remaining <= CLAUDE_CLI_FAILURE_COOLDOWN);

        assert!(
            client
                .claude_cli_skip_at(
                    failed_at + CLAUDE_CLI_FAILURE_COOLDOWN + Duration::from_secs(1)
                )
                .is_none(),
            "failure cache should expire so Claude CLI can recover"
        );

        client.record_claude_cli_failure_at(
            "claude cli timeout after 5 minutes",
            failed_at + Duration::from_secs(5),
        );
        assert_eq!(
            client
                .claude_cli_skip_at(failed_at + Duration::from_secs(6))
                .expect("timeout should suppress immediate retries")
                .kind,
            ClaudeCliFailureKind::Timeout
        );

        client.record_claude_cli_success();
        assert!(client
            .claude_cli_skip_at(failed_at + Duration::from_secs(7))
            .is_none());
    }

    #[test]
    fn claude_cli_failure_cache_ignores_prompt_level_errors() {
        let client = LlmClient::new().expect("client should initialize");
        let now = Instant::now();

        client.record_claude_cli_failure_at("claude cli exited 1: bad prompt", now);
        assert!(
            client
                .claude_cli_skip_at(now + Duration::from_secs(1))
                .is_none(),
            "non-availability errors should not disable future CLI attempts"
        );

        client.record_claude_cli_failure_at(
            "claude cli exited 1: model output mentioned timeout",
            now,
        );
        assert!(
            client
                .claude_cli_skip_at(now + Duration::from_secs(1))
                .is_none(),
            "stderr content should not look like a process timeout"
        );

        client.record_claude_cli_failure_at("claude cli exited 1: prompt said spawn failed", now);
        assert!(
            client
                .claude_cli_skip_at(now + Duration::from_secs(1))
                .is_none(),
            "stderr content should not look like a spawn failure"
        );
    }

    #[test]
    fn vault_provider_secret_overrides_env_value() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_VAULT_OVERRIDE";
        std::env::set_var(KEY, "env-value");
        let client = LlmClient::new().expect("client should initialize");

        client.set_provider_secret(KEY, "vault-value");

        assert_eq!(
            client.provider_secret_for_tests(&[KEY]).unwrap(),
            "vault-value"
        );
        std::env::remove_var(KEY);
    }

    #[test]
    fn provider_pool_status_reports_cooldown_without_secret_values() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_POOL_STATUS";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        let first_key = format!("{KEY}_1");
        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]).as_deref(),
            Some(first_key.as_str())
        );
        client.mark_provider_key_rate_limited_for_tests(&first_key, Some(60));

        let statuses = client.provider_pool_statuses();
        let status = statuses
            .iter()
            .find(|status| status.logical_name == KEY)
            .expect("pool status should include logical key");
        assert_eq!(status.total_keys, 2);
        assert_eq!(status.available_keys, 1);
        assert_eq!(status.rate_limited_keys[0].key_id, first_key);
        assert_eq!(status.current_index, 1);
        let raw = serde_json::to_string(&statuses).expect("serialize statuses");
        assert!(!raw.contains("secret-one"));
        assert!(!raw.contains("secret-two"));
    }

    #[test]
    fn provider_runtime_maps_share_one_state_lock() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_PROVIDER_STATE";
        let client = LlmClient::new().expect("client should initialize");
        let key_id = format!("{KEY}_1");
        client.set_provider_secret_pool(
            KEY,
            vec![ProviderSecret {
                key_id: key_id.clone(),
                value: "secret-one".to_string(),
            }],
        );

        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]).as_deref(),
            Some(key_id.as_str())
        );
        client.mark_provider_key_rate_limited_for_tests(&key_id, Some(60));

        let state = client
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(state.secrets.contains_key(KEY));
        assert_eq!(state.indices.get(KEY), Some(&0));
        assert!(state.cooldowns.contains_key(&key_id));
        assert_eq!(
            state
                .health
                .get(KEY)
                .and_then(|members| members.get(&key_id))
                .map(|health| health.status.as_str()),
            Some(HEALTH_RATE_LIMITED)
        );
        assert_eq!(
            state
                .health_snapshots
                .get(KEY)
                .and_then(|members| members.get(&key_id))
                .map(|snapshot| snapshot.availability),
            Some(KeyAvailability::Cooldown)
        );
    }

    #[test]
    fn env_fallback_skips_rate_limited_key() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ENV_COOLDOWN";
        std::env::set_var(KEY, "env-secret");
        let client = LlmClient::new().expect("client should initialize");

        assert_eq!(
            client.provider_secret_for_tests(&[KEY]),
            Some("env-secret".to_string())
        );
        client.mark_provider_key_rate_limited_for_tests(KEY, Some(60));

        assert!(
            client.provider_secret_for_tests(&[KEY]).is_none(),
            "env fallback should not reuse a key while it is cooling down"
        );
        std::env::remove_var(KEY);
    }

    #[test]
    fn all_pool_keys_rate_limited_returns_none_if_all_blocked() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ALL_COOLDOWN";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_1"), Some(60));
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_2"), Some(60));

        assert!(
            client.provider_key_id_for_tests(&[KEY]).is_none(),
            "pool selection should return None when all members are cooling down"
        );

        let err = client
            .required_secret(&[KEY])
            .expect_err("cooling pool should not be reported as a missing key");
        assert!(
            err.contains("temporarily unavailable"),
            "expected cooldown-specific error, got: {err}"
        );
        assert!(
            err.contains("retry after"),
            "expected retry guidance, got: {err}"
        );
    }

    #[test]
    fn retry_delay_adds_bounded_jitter_to_exponential_backoff() {
        let first = LlmClient::retry_delay_with_jitter(1, 0);
        assert!(first >= Duration::from_millis(LlmClient::BASE_RETRY_DELAY_MS));
        assert!(
            first
                <= Duration::from_millis(
                    LlmClient::BASE_RETRY_DELAY_MS
                        + (LlmClient::BASE_RETRY_DELAY_MS * LlmClient::RETRY_JITTER_PERCENT / 100)
                )
        );

        let later = LlmClient::retry_delay_with_jitter(3, 0);
        let later_base = LlmClient::BASE_RETRY_DELAY_MS * 4;
        assert!(later >= Duration::from_millis(later_base));
        assert!(
            later
                <= Duration::from_millis(
                    later_base + (later_base * LlmClient::RETRY_JITTER_PERCENT / 100)
                )
        );
    }

    #[test]
    fn retry_delay_jitter_varies_by_seed() {
        let first = LlmClient::retry_delay_with_jitter(2, 1);
        let second = LlmClient::retry_delay_with_jitter(2, 2);
        assert_ne!(first, second);
    }

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
    fn all_pool_keys_auth_failed_returns_none() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ALL_UNUSABLE";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        client.mark_provider_key_auth_failed_for_tests(KEY, &format!("{KEY}_1"));
        client.mark_provider_key_auth_failed_for_tests(KEY, &format!("{KEY}_2"));
        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]),
            None,
            "pool selection should not return blocked auth-failed members"
        );
        let err = client
            .required_secret(&[KEY])
            .expect_err("auth-failed pool should not be reported as a missing key");
        assert!(
            err.contains("unusable") && err.contains("auth_failed"),
            "expected auth-failed reason, got: {err}"
        );
    }

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
    async fn provider_key_health_blocking_persist_honors_test_disable_env() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

        let health = client.record_provider_key_result_blocking(
            "TACHI_TEST_ONLY_API_KEY_DISABLED_PERSIST",
            "TACHI_TEST_ONLY_API_KEY_DISABLED_PERSIST_1",
            Some(429),
            None,
            Some(30),
            Some("provider throttled"),
        );

        assert_eq!(health.status, HEALTH_RATE_LIMITED);
        assert!(
            !db_path.exists(),
            "blocking persist should not create a DB when test persistence is disabled"
        );
        let status = client.provider_health_status();
        assert!(status.persist_last_attempt_at.is_none());
        assert!(status.persist_last_error.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn provider_key_health_persists_off_async_runtime_thread() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

        let health = client.record_provider_key_result(
            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
            Some(429),
            None,
            Some(30),
            Some("provider throttled"),
        );
        assert_eq!(health.status, HEALTH_RATE_LIMITED);

        let mut persisted = None;
        for _ in 0..50 {
            if db_path.exists() {
                if let Ok(store) = memory_core::MemoryStore::open(db_path.to_str().unwrap()) {
                    persisted = store
                        .vault_get_key_health(
                            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
                            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
                        )
                        .ok()
                        .flatten();
                    if persisted.is_some() {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let persisted = persisted.expect("background key-health persist should finish");
        assert_eq!(persisted.status, HEALTH_RATE_LIMITED);
        assert_eq!(
            persisted.last_error.as_deref(),
            Some("rate limited; retry after 30s")
        );

        let mut status = client.provider_health_status();
        for _ in 0..50 {
            if status.persist_last_success_at.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            status = client.provider_health_status();
        }
        assert!(status.persist_last_attempt_at.is_some());
        assert!(status.persist_last_success_at.is_some());
        assert!(status.persist_last_error.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn provider_key_health_persist_errors_are_visible_in_status() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("missing-parent").join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

        let health = client.record_provider_key_result(
            "TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR",
            "TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR_1",
            Some(429),
            None,
            Some(30),
            Some("provider throttled"),
        );
        assert_eq!(health.status, HEALTH_RATE_LIMITED);

        let mut status = client.provider_health_status();
        for _ in 0..50 {
            if status.persist_last_error.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            status = client.provider_health_status();
        }

        let error = status
            .persist_last_error
            .as_deref()
            .expect("persist error should be visible");
        assert!(status.persist_last_attempt_at.is_some());
        assert!(status.persist_last_success_at.is_none());
        assert!(
            error.contains("persist vault key health for TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR"),
            "unexpected persist error: {error}"
        );
    }

    #[tokio::test]
    async fn provider_key_health_reloads_external_db_cooldowns_before_selection() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_RELOAD_COOLDOWN";
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
        drop(store);

        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );

        let now = Utc::now();
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
        store
            .vault_upsert_key_health(&VaultKeyHealth {
                logical_name: KEY.to_string(),
                key_id: format!("{KEY}_1"),
                status: HEALTH_RATE_LIMITED.to_string(),
                cooldown_until: Some((now + chrono::Duration::seconds(60)).to_rfc3339()),
                last_attempt: Some(now.to_rfc3339()),
                last_error: Some("manual CLI cooldown".to_string()),
                updated_at: now.to_rfc3339(),
                ..VaultKeyHealth::default()
            })
            .expect("write external key health");
        drop(store);

        client.force_provider_health_reload_due_for_tests();
        let selected = client
            .required_selected_secret_or_wait(&[KEY], 1, "test reload")
            .await
            .expect("selection should not fail")
            .expect("second key should be selected");

        assert_eq!(selected.key_id, format!("{KEY}_2"));
        let status = client.provider_health_status();
        assert_eq!(status.source_of_truth, "vault_db");
        assert!(status.last_success_at.is_some());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn provider_key_health_reload_clears_local_cooldown_on_external_success() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_RELOAD_SUCCESS";
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_1"), Some(300));

        let now = Utc::now() + chrono::Duration::seconds(1);
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
        store
            .vault_upsert_key_health(&VaultKeyHealth {
                logical_name: KEY.to_string(),
                key_id: format!("{KEY}_1"),
                status: HEALTH_OK.to_string(),
                last_success: Some(now.to_rfc3339()),
                updated_at: now.to_rfc3339(),
                ..VaultKeyHealth::default()
            })
            .expect("write external success health");
        drop(store);

        client.force_provider_health_reload_due_for_tests();
        let selected = client
            .required_selected_secret_or_wait(&[KEY], 1, "test reload")
            .await
            .expect("selection should not fail")
            .expect("first key should be reinstated");

        assert_eq!(selected.key_id, format!("{KEY}_1"));
        assert!(client
            .provider_pool_statuses()
            .into_iter()
            .find(|status| status.logical_name == KEY)
            .is_some_and(|status| status.available_keys == 2));
    }

    #[test]
    fn expired_cooldown_reinstates_pool_key() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_EXPIRED_COOLDOWN";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![ProviderSecret {
                key_id: format!("{KEY}_1"),
                value: "secret-one".to_string(),
            }],
        );
        let key_id = format!("{KEY}_1");
        client.mark_provider_key_rate_limited_for_tests(&key_id, Some(1));
        std::thread::sleep(Duration::from_millis(1100));

        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]).as_deref(),
            Some(key_id.as_str())
        );
        assert!(client
            .provider_pool_statuses()
            .into_iter()
            .find(|status| status.logical_name == KEY)
            .is_some_and(|status| status.rate_limited_keys.is_empty()));
    }

    #[test]
    fn cooldown_retry_ignores_permanently_failed_pool_members() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_MIXED_HEALTH";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "bad-secret".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "cooling-secret".to_string(),
                },
            ],
        );
        client.mark_provider_key_auth_failed_for_tests(KEY, &format!("{KEY}_1"));
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_2"), Some(30));

        let delay = client
            .selected_secret_retry_delay(&[KEY])
            .expect("cooling key should still drive retry timing");
        assert!(delay > Duration::ZERO);
        assert!(delay <= Duration::from_secs(30));
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
    fn reasoning_lane_declares_zhipu_key_aliases() {
        const KEY: &str = "TACHI_TEST_ONLY_ZAI_ALIAS_KEY";
        std::env::set_var(KEY, "zai-value");

        let client = LlmClient::new().expect("client should initialize");

        assert_eq!(
            client.provider_secret_for_tests(&[KEY]),
            Some("zai-value".to_string())
        );
        assert!(client.reasoning.api_key_envs.contains(&"ZAI_API_KEY"));
        assert!(client.reasoning.api_key_envs.contains(&"BIGMODEL_API_KEY"));
        assert!(client.distill.api_key_envs.contains(&"ZAI_API_KEY"));
        assert!(client.distill.api_key_envs.contains(&"BIGMODEL_API_KEY"));
        std::env::remove_var(KEY);
    }

    #[test]
    fn extract_json_payload_ignores_prefix_and_suffix() {
        let raw = "<think>ignore</think>\n{\"ok\": true}\nextra text";
        assert_eq!(
            LlmClient::extract_json_payload(raw).expect("json payload"),
            "{\"ok\": true}"
        );
    }
}
