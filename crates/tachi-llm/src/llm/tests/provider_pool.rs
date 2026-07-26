use super::*;

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
    client.mark_provider_key_rate_limited_for_tests(KEY, &first_key, Some(60));

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
fn env_fallback_skips_rate_limited_key() {
    const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ENV_COOLDOWN";
    std::env::set_var(KEY, "env-secret");
    let client = LlmClient::new().expect("client should initialize");

    assert_eq!(
        client.provider_secret_for_tests(&[KEY]),
        Some("env-secret".to_string())
    );
    client.mark_provider_key_rate_limited_for_tests(KEY, KEY, Some(60));

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
    client.mark_provider_key_rate_limited_for_tests(KEY, &format!("{KEY}_1"), Some(60));
    client.mark_provider_key_rate_limited_for_tests(KEY, &format!("{KEY}_2"), Some(60));

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

#[test]
fn stale_auth_failed_pool_key_is_retryable() {
    const KEY: &str = "TACHI_TEST_ONLY_API_KEY_STALE_AUTH_FAILURE";
    let client = LlmClient::new().expect("client should initialize");
    let key_id = format!("{KEY}_1");
    client.set_provider_secret_pool(
        KEY,
        vec![ProviderSecret {
            key_id: key_id.clone(),
            value: "secret-one".to_string(),
        }],
    );
    client.mark_provider_key_auth_failed_for_tests(KEY, &key_id);
    assert_eq!(
        client.provider_key_id_for_tests(&[KEY]),
        None,
        "fresh auth failures must still block provider selection"
    );

    client.expire_provider_key_auth_failure_for_tests(KEY, &key_id);
    assert_eq!(
        client.provider_key_id_for_tests(&[KEY]).as_deref(),
        Some(key_id.as_str()),
        "stale auth failures should be eligible for a live retry"
    );
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
    client.mark_provider_key_rate_limited_for_tests(KEY, &key_id, Some(1));
    client.expire_provider_key_cooldown_for_tests(KEY, &key_id);

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
    client.mark_provider_key_rate_limited_for_tests(KEY, &format!("{KEY}_2"), Some(30));

    let delay = client
        .selected_secret_retry_delay(&[KEY])
        .expect("cooling key should still drive retry timing");
    assert!(delay > Duration::ZERO);
    assert!(delay <= Duration::from_secs(30));
}
