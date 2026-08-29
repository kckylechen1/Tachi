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
            rebind: false,
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
            rebind: false,
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
            rebind: false,
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
            rebind: false,
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

/// Rotation aliases resolve the prefix. When every member is empty, the
/// prefix drop must be recorded in the same scan — otherwise materialization
/// reports `SecretAbsent`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn empty_rotation_members_classify_the_prefix_as_listed_empty() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "vault:SILICONFLOW_API_KEY");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-rotation-empty".to_string(),
        }))
        .await
        .expect("vault_init");
    for name in ["SILICONFLOW_API_KEY_1", "SILICONFLOW_API_KEY_2"] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: "   ".to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "empty rotation member".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            }))
            .await
            .expect("vault_set empty member");
    }
    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "SILICONFLOW_API_KEY".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation");

    let report = crate::provider_config::materialize_for_server(&server)
        .expect("refresh must degrade, not abort");
    assert_eq!(
        report.skip_class_for("SILICONFLOW_API_KEY"),
        AliasSkipClass::ListedEmpty,
        "prefix alias must inherit member empty class: {:?}",
        report.skipped_aliases
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

/// An unconfigured rotation-member-shaped alias must materialize identically
/// through the unlocked-server and Keychain/standalone scan classifiers.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn unconfigured_rotation_member_alias_materializes_from_unlocked_server() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "vault:VOYAGE_API_KEY_1");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-unconfigured-member".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "VOYAGE_API_KEY_1".to_string(),
            value: "voyage-member-fixture".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "unconfigured rotation member".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set member");
    let report = crate::provider_config::materialize_for_server(&server).expect("refresh");
    assert_eq!(
        report.from_alias, 1,
        "alias must resolve from the raw member pool"
    );
    assert!(
        report
            .skipped_aliases
            .iter()
            .all(|(name, _)| name != "VOYAGE_API_KEY"),
        "resolved member alias must not be reported as skipped: {:?}",
        report.skipped_aliases
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn unhealthy_configured_rotation_member_cannot_fall_back_as_standalone() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = crate::test_support::EnvRestore::set("VOYAGE_API_KEY", "vault:VOYAGE_API_KEY_1");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-configured-unhealthy".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "VOYAGE_API_KEY_1".to_string(),
            value: "unhealthy-member-fixture".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "configured unhealthy member".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set member");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "VOYAGE_API_KEY_2".to_string(),
            value: "healthy-member-fixture".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "configured healthy control member".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set control member");
    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "VOYAGE_API_KEY".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation");
    let leased: serde_json::Value = serde_json::from_str(
        &server
            .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
                name: "VOYAGE_API_KEY_1".to_string(),
                env_name: None,
                agent_id: None,
            }))
            .await
            .expect("first raw-member lease"),
    )
    .expect("lease json");
    assert_eq!(leased["logical_name"], "VOYAGE_API_KEY");
    assert_eq!(leased["key_id"], "VOYAGE_API_KEY_1");

    let key = {
        let vault = server.vault_read();
        *vault.key.as_ref().expect("unlocked key").bytes()
    };
    server
        .with_global_store(|store| {
            let mut rotation = store
                .vault_get_rotation("VOYAGE_API_KEY")
                .map_err(|error| error.to_string())?
                .expect("configured rotation");
            rotation.current_index = 1;
            store
                .vault_set_rotation(&rotation)
                .map_err(|error| error.to_string())
        })
        .expect("reset rotation before CLI lease");
    let (cli_logical_name, cli_key_id, mut cli_value) = server
        .with_global_store(|store| {
            crate::bootstrap::lease_api_key_from_store(store, &key, "VOYAGE_API_KEY_1")
                .map_err(|error| error.to_string())
        })
        .expect("first CLI raw-member lease");
    assert_eq!(cli_logical_name, "VOYAGE_API_KEY");
    assert_eq!(cli_key_id, "VOYAGE_API_KEY_1");
    assert_eq!(
        server
            .with_global_store_read(|store| {
                store
                    .vault_get_rotation("VOYAGE_API_KEY")
                    .map_err(|error| error.to_string())
            })
            .expect("read rotation")
            .expect("configured rotation")
            .current_index,
        2,
        "CLI raw-member lease must advance the canonical prefix rotation"
    );
    crate::vault_crypto::zero_string(&mut cli_value);

    server
        .vault_record_key_result(Parameters(VaultRecordKeyResultParams {
            logical_name: leased["logical_name"].as_str().unwrap().to_string(),
            key_id: leased["key_id"].as_str().unwrap().to_string(),
            status_code: Some(401),
            outcome: None,
            retry_after_secs: None,
            reason: Some("provider auth failed".to_string()),
        }))
        .await
        .expect("record returned lease identity");

    let lease_error = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "VOYAGE_API_KEY_1".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect_err("raw configured member lease must honor prefix health");
    assert!(lease_error.contains("No usable API key"), "{lease_error}");

    let cli_error = server
        .with_global_store_read(|store| {
            crate::bootstrap::lease_api_key_from_store(store, &key, "VOYAGE_API_KEY_1")
                .map_err(|error| error.to_string())
        })
        .expect_err("CLI raw configured member lease must honor prefix health");
    assert!(cli_error.contains("No usable API key"), "{cli_error}");

    let report = crate::provider_config::materialize_for_server(&server)
        .expect("refresh must classify, not abort");
    assert_eq!(report.from_alias, 0);
    assert_eq!(
        report.skip_class_for("VOYAGE_API_KEY"),
        AliasSkipClass::ListedUnusableAuthFailed
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn unlocked_rotation_prefix_drop_uses_lowest_member_across_current_index() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "alias-integrity-mixed-rotation".to_string(),
        }))
        .await
        .expect("vault_init");
    for member in ["VOYAGE_API_KEY_1", "VOYAGE_API_KEY_2"] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: member.to_string(),
                value: format!("{member}-fixture"),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "mixed rotation control member".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            }))
            .await
            .expect("vault_set member");
    }
    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "VOYAGE_API_KEY".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "VOYAGE_API_KEY_1".to_string(),
            value: "wrong-type-member".to_string(),
            agent_id: None,
            secret_type: "other".to_string(),
            description: "lowest member determines prefix class".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("replace member one");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "VOYAGE_API_KEY_2".to_string(),
            value: "fenced-member".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "higher fenced member".to_string(),
            allowed_agents: Some(vec!["agent-a".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("replace member two");

    for current_index in [1, 2] {
        server
            .with_global_store(|store| {
                let mut rotation = store
                    .vault_get_rotation("VOYAGE_API_KEY")
                    .map_err(|error| error.to_string())?
                    .expect("configured rotation");
                rotation.current_index = current_index;
                store
                    .vault_set_rotation(&rotation)
                    .map_err(|error| error.to_string())
            })
            .expect("set current index");
        let scan = crate::vault_ops::load_unlocked_api_key_secret_pools_with_drops(&server)
            .expect("scan configured rotation");
        assert_eq!(
            scan.dropped.get("VOYAGE_API_KEY"),
            Some(&AliasSkipClass::ListedWrongType),
            "current index {current_index} must not change the prefix class"
        );
    }
}

/// Corrupt listed payloads fail the real refresh seam without exposing the
/// alias target or the decoder's byte-position/length details (#1854).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn invalid_utf8_alias_payload_fails_closed_with_public_safe_error() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let unlock_input = "alias-integrity-invalid-utf8";
    let alias_target = "HIDDEN_TARGET_API_KEY";
    let alias = format!("vault:{alias_target}");
    let _env = crate::test_support::EnvRestore::set("ANTHROPIC_API_KEY", &alias);
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: unlock_input.to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: alias_target.to_string(),
            value: "initial-valid-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "invalid UTF-8 redaction discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set");

    let config = server
        .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
        .expect("read vault config")
        .expect("vault config");
    let key = crate::vault_crypto::derive_verified_key_from_stored_config(&config, unlock_input)
        .expect("derive fixture key");
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(key.bytes(), &[0xff, 0xfe]).expect("encrypt raw bytes");
    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry(alias_target)
                .map_err(|e| e.to_string())?
                .expect("fixture entry");
            entry.encrypted_value = encrypted_value;
            entry.nonce = nonce;
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("install invalid UTF-8 payload");

    let error = server
        .refresh_llm_provider_secrets_from_vault()
        .expect_err("invalid UTF-8 must fail the provider refresh");
    assert!(
        error.ends_with(crate::vault_ops::VAULT_MATERIALIZATION_INVALID_UTF8),
        "unexpected public-safe refusal: {error}"
    );
    assert!(
        !error.contains(alias_target),
        "alias target leaked: {error}"
    );
    assert!(!error.contains("byte"), "decoder length leaked: {error}");
    assert!(!error.contains("0xff"), "payload detail leaked: {error}");
}
