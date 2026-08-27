use super::*;
use tachi_llm::AliasSkipClass;

/// tachi#1854: a listed API-key row marked auth_failed must not be reported
/// as revocation (`absent from a readable Vault`). Live 2026-08 host printed
/// that lie for `SILICONFLOW_API_KEY=vault:SILICONFLOW_API_KEY`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn listed_auth_failed_alias_is_unusable_not_absent() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "vault:SILICONFLOW_API_KEY");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-auth-failed".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "sf-fixture-key".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "listed-unusable discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set");
    server
        .vault_record_key_result(Parameters(VaultRecordKeyResultParams {
            logical_name: "SILICONFLOW_API_KEY".to_string(),
            key_id: "SILICONFLOW_API_KEY".to_string(),
            status_code: Some(401),
            outcome: None,
            retry_after_secs: None,
            reason: Some("provider auth failed".to_string()),
        }))
        .await
        .expect("record 401");

    let report = crate::provider_config::materialize_for_server(&server)
        .expect("refresh must degrade, not abort");
    let skip = report
        .skipped_aliases
        .iter()
        .find(|(key, _)| key == "SILICONFLOW_API_KEY")
        .expect("alias skip must be recorded");
    assert_eq!(
        report.skip_class_for("SILICONFLOW_API_KEY"),
        AliasSkipClass::ListedUnusableAuthFailed
    );
    assert!(
        skip.1.contains("listed secret is unusable (auth_failed)"),
        "{}",
        skip.1
    );
    assert!(
        !skip.1.contains("absent from a readable Vault"),
        "listed row must not use revocation wording: {}",
        skip.1
    );
    let warning = crate::provider_config::format_skipped_alias_warning(
        "SILICONFLOW_API_KEY",
        false,
        report.skip_class_for("SILICONFLOW_API_KEY"),
    );
    assert!(
        !warning.contains("absent from a readable Vault"),
        "{warning}"
    );
}

/// tachi#1854: storing a URL-shaped name as `other` still lists the row;
/// alias resolution must say wrong type, not absent.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn listed_wrong_type_alias_is_not_absent() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "vault:SILICONFLOW_API_KEY");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-wrong-type".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "https://example.invalid/v1".to_string(),
            agent_id: None,
            secret_type: "other".to_string(),
            description: "wrong-type discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set");

    let report = crate::provider_config::materialize_for_server(&server)
        .expect("refresh must degrade, not abort");
    assert_eq!(
        report.skip_class_for("SILICONFLOW_API_KEY"),
        AliasSkipClass::ListedWrongType
    );
    let reason = &report
        .skipped_aliases
        .iter()
        .find(|(key, _)| key == "SILICONFLOW_API_KEY")
        .expect("skip")
        .1;
    assert!(reason.contains("not an api_key"), "{reason}");
    assert!(!reason.contains("absent from a readable Vault"), "{reason}");
}

/// tachi#1854 control: a genuinely missing target on a readable Vault is
/// still revocation (#1279) and still does not abort the batch.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn missing_alias_target_on_readable_vault_stays_absent() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = crate::test_support::EnvRestore::set(
        "ANTHROPIC_API_KEY",
        "vault:MISSING_ANTHROPIC_INTEGRITY",
    );
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-absent".to_string(),
        }))
        .await
        .expect("vault_init");

    let report = crate::provider_config::materialize_for_server(&server)
        .expect("missing alias must not abort the batch");
    assert_eq!(
        report.skip_class_for("ANTHROPIC_API_KEY"),
        AliasSkipClass::SecretAbsent
    );
    let reason = &report
        .skipped_aliases
        .iter()
        .find(|(key, _)| key == "ANTHROPIC_API_KEY")
        .expect("skip")
        .1;
    assert!(reason.contains("absent from a readable Vault"), "{reason}");
    assert!(!reason.contains("MISSING_ANTHROPIC_INTEGRITY"), "{reason}");
}

/// tachi#1854: empty listed ciphertext is a listed integrity miss, not
/// revocation.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn listed_empty_alias_is_not_absent() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "vault:SILICONFLOW_API_KEY");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-empty".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "   ".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "empty-value discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set whitespace");

    let report = crate::provider_config::materialize_for_server(&server)
        .expect("refresh must degrade, not abort");
    assert_eq!(
        report.skip_class_for("SILICONFLOW_API_KEY"),
        AliasSkipClass::ListedEmpty
    );
    let reason = &report
        .skipped_aliases
        .iter()
        .find(|(key, _)| key == "SILICONFLOW_API_KEY")
        .expect("skip")
        .1;
    assert!(reason.contains("empty value"), "{reason}");
    assert!(!reason.contains("absent from a readable Vault"), "{reason}");
}

/// tachi#1860: drop class is recorded at pool-load time. Mutating health
/// after that scan (the old post-hoc upgrade) must not rewrite the skip
/// as ListedEmpty / SecretAbsent.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn skip_class_is_the_pool_load_snapshot_not_a_later_reread() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "vault:SILICONFLOW_API_KEY");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-snapshot".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "sf-fixture-key".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "snapshot discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set");
    server
        .vault_record_key_result(Parameters(VaultRecordKeyResultParams {
            logical_name: "SILICONFLOW_API_KEY".to_string(),
            key_id: "SILICONFLOW_API_KEY".to_string(),
            status_code: Some(401),
            outcome: None,
            retry_after_secs: None,
            reason: Some("provider auth failed".to_string()),
        }))
        .await
        .expect("record 401");

    let llm = std::sync::Arc::clone(&server.llm);
    let report =
        crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
            let _ = llm.record_provider_key_result_blocking(
                "SILICONFLOW_API_KEY",
                "SILICONFLOW_API_KEY",
                Some(200),
                None,
                None,
                None,
            );
        })
        .expect("refresh");
    assert_eq!(
        report.skip_class_for("SILICONFLOW_API_KEY"),
        AliasSkipClass::ListedUnusableAuthFailed,
        "later health=ok must not rewrite the load-time drop: {:?}",
        report.skipped_aliases
    );
}
