use super::*;

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
            .claude_cli_skip_at(failed_at + CLAUDE_CLI_FAILURE_COOLDOWN + Duration::from_secs(1))
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

    client.record_claude_cli_failure_at("claude cli exited 1: model output mentioned timeout", now);
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
    client.mark_provider_key_rate_limited_for_tests(KEY, &key_id, Some(60));

    let state = client
        .provider_state
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(state.secrets.contains_key(KEY));
    assert_eq!(state.indices.get(KEY), Some(&0));
    assert!(state.is_cooling_down(KEY, &key_id));
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
