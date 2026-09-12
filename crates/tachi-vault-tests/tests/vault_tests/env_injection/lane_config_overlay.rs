#![allow(clippy::await_holding_lock)]

use super::*;
use std::path::Path;

fn clear_model_provider_env() -> Vec<crate::test_support::EnvRestore> {
    [
        "VOYAGE_BASE_URL",
        "TACHI_EMBEDDING_MODEL",
        "TACHI_EMBEDDING_DIM",
        "VOYAGE_RERANK_API_KEY",
        "VOYAGE_API_KEY",
        "SILICONFLOW_API_KEY",
        "EXTRACT_API_KEY",
        "SUMMARY_API_KEY",
        "DEEPSEEK_API_KEY",
        "DISTILL_API_KEY",
        "ZAI_API_KEY",
        "BIGMODEL_API_KEY",
        "XAI_API_KEY",
        "GROK_API_KEY",
        "ZHIPUAI_API_KEY",
        "KIMI_API_KEY",
        "MOONSHOT_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "MINIMAX_API_KEY",
        "REASONING_API_KEY",
    ]
    .into_iter()
    .map(crate::test_support::EnvRestore::remove)
    .collect()
}

fn materialization_test_config() -> tachi_llm::ProviderRuntimeConfig {
    tachi_llm::ProviderRuntimeConfig {
        extract: tachi_llm::ChatLaneConfig {
            base_url: "https://baseline-extract.example/v1/chat/completions".to_string(),
            model: "baseline-extract-model".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        summary: tachi_llm::ChatLaneConfig {
            base_url: "https://baseline-summary.example/v1/chat/completions".to_string(),
            model: "baseline-summary-model".to_string(),
            api_key_envs: vec!["SUMMARY_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        reasoning: tachi_llm::ChatLaneConfig {
            base_url: "https://baseline-reasoning.example/v1/chat/completions".to_string(),
            model: "baseline-reasoning-model".to_string(),
            api_key_envs: vec!["DEEPSEEK_API_KEY", "REASONING_API_KEY"],
        },
        distill: tachi_llm::ChatLaneConfig {
            base_url: "https://baseline-distill.example/v1/chat/completions".to_string(),
            model: "baseline-distill-model".to_string(),
            api_key_envs: vec!["DISTILL_API_KEY", "DEEPSEEK_API_KEY"],
        },
        rerank: tachi_llm::RerankConfig {
            provider: tachi_llm::RerankProviderKind::Local,
            local_endpoint: Some("http://127.0.0.1:9/rerank".to_string()),
        },
    }
}

fn materialization_test_server() -> crate::tests::TestServer {
    let mut server = make_server();
    server.replace_llm(
        tachi_llm::LlmClient::new_with_config(materialization_test_config(), None)
            .expect("materialization test client"),
    );
    server
}

async fn initialize_vault(server: &crate::MemoryServer, password: &str) {
    server
        .vault_init(Parameters(VaultInitParams {
            password: password.to_string(),
        }))
        .await
        .expect("vault_init should succeed");
}

async fn set_vault_value(
    server: &crate::MemoryServer,
    name: &str,
    value: &str,
    secret_type: &str,
) -> String {
    server
        .vault_set(Parameters(VaultSetParams {
            name: name.to_string(),
            value: value.to_string(),
            agent_id: None,
            secret_type: secret_type.to_string(),
            description: "PR #1862 materialization discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set should succeed")
}

async fn seed_provider_and_lane(
    server: &crate::MemoryServer,
    password: &str,
    provider_name: &str,
    provider_value: &str,
    lane_url: &str,
    lane_model: &str,
) {
    initialize_vault(server, password).await;
    if crate::vault_ops::is_lane_slot_secret_name(provider_name) {
        // Keep custody outside this fixture's independent lane fallback lists:
        // these assertions exercise the explicit slot and its overlay, not a
        // second provider pool newly made reachable by seeding the account.
        set_vault_value(server, "OPENAI_API_KEY", provider_value, "api_key").await;
        set_vault_value(server, provider_name, "vault:OPENAI_API_KEY", "api_key").await;
    } else {
        set_vault_value(server, provider_name, provider_value, "api_key").await;
    }
    set_vault_value(server, "EXTRACT_BASE_URL", lane_url, "other").await;
    set_vault_value(server, "EXTRACT_MODEL", lane_model, "other").await;
}

fn env_catalog_rows(server: &crate::MemoryServer) -> Vec<memcore::catalog::ModelDeployment> {
    server
        .with_global_store_read(|store| {
            memcore::db::model_catalog::list_model_deployments_by_source(
                store.connection(),
                memcore::catalog::CatalogSource::Env,
            )
            .map_err(|err| err.to_string())
        })
        .expect("read env catalog rows")
}

fn published_snapshot(
    server: &crate::MemoryServer,
    provider_keys: &[&str],
) -> (
    tachi_llm::ProviderRuntimeConfig,
    Option<String>,
    Vec<memcore::catalog::ModelDeployment>,
) {
    (
        server.llm.runtime_config(),
        server.llm.provider_secret_for_tests(provider_keys),
        env_catalog_rows(server),
    )
}

fn install_catalog_failure_trigger(server: &crate::MemoryServer) {
    let db_path = server.global_db_path_buf();
    crate::test_support::with_unrestricted_fixture_connection(&db_path, |connection| {
        connection.execute_batch(
            "CREATE TRIGGER pr1862_fail_catalog_refresh
             BEFORE UPDATE OF fetched_at ON model_deployments
             WHEN NEW.deployment_id = 'env:summary'
             BEGIN
                 SELECT RAISE(ABORT, 'pr1862 injected catalog failure');
             END;",
        )
    })
    .expect("install catalog failure trigger");
}

fn drop_catalog_failure_trigger(server: &crate::MemoryServer) {
    let db_path = server.global_db_path_buf();
    crate::test_support::with_unrestricted_fixture_connection(&db_path, |connection| {
        connection.execute_batch("DROP TRIGGER IF EXISTS pr1862_fail_catalog_refresh")
    })
    .expect("drop catalog failure trigger");
}

/// tachi#1856: vault_set EXTRACT_BASE_URL must change the live lane after
/// refresh, not wait for a process restart that re-reads dotenv.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_extract_base_url_wins_over_dotenv_after_refresh() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dotenv_url = "https://dotenv-siliconflow.example/v1/chat/completions";
    let vault_url = "https://api.deepseek.com/chat/completions";
    let _env = crate::test_support::EnvRestore::set("EXTRACT_BASE_URL", dotenv_url);

    let mut server = make_server();
    server.replace_llm(
        tachi_llm::LlmClient::new_with_config(
            tachi_llm::ProviderRuntimeConfig {
                extract: tachi_llm::ChatLaneConfig {
                    base_url: dotenv_url.to_string(),
                    model: "env-model".to_string(),
                    api_key_envs: vec!["EXTRACT_API_KEY"],
                },
                summary: tachi_llm::ChatLaneConfig {
                    base_url: dotenv_url.to_string(),
                    model: "env-model".to_string(),
                    api_key_envs: vec!["SUMMARY_API_KEY"],
                },
                reasoning: tachi_llm::ChatLaneConfig {
                    base_url: dotenv_url.to_string(),
                    model: "env-model".to_string(),
                    api_key_envs: vec!["REASONING_API_KEY"],
                },
                distill: tachi_llm::ChatLaneConfig {
                    base_url: dotenv_url.to_string(),
                    model: "env-model".to_string(),
                    api_key_envs: vec!["DISTILL_API_KEY"],
                },
                rerank: tachi_llm::RerankConfig {
                    provider: tachi_llm::RerankProviderKind::Local,
                    local_endpoint: Some("http://127.0.0.1:9/rerank".to_string()),
                },
            },
            None,
        )
        .expect("literal client"),
    );

    server
        .vault_init(Parameters(VaultInitParams {
            password: "lane-overlay-password".to_string(),
        }))
        .await
        .expect("init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "EXTRACT_BASE_URL".to_string(),
            value: vault_url.to_string(),
            agent_id: None,
            secret_type: "other".to_string(),
            description: "lane overlay discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set url");

    crate::provider_config::materialize_for_server(&server).expect("refresh");
    let live = server.llm.runtime_config();
    assert_eq!(
        live.extract.base_url, vault_url,
        "vault EXTRACT_BASE_URL must win over dotenv after refresh"
    );
    assert_eq!(
        live.distill.base_url, dotenv_url,
        "unset DISTILL_BASE_URL vault row must leave distill on env baseline"
    );
}

/// A Vault decrypt failure occurs after the provider snapshot is prepared but
/// before its lane overlay/catalog companion can commit. Every published
/// surface must therefore remain byte-for-byte unchanged.
#[tokio::test]
async fn vault_lane_decrypt_failure_keeps_previous_runtime_snapshot() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let _siliconflow =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "ambient-siliconflow-key");
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-decrypt-password",
        "EXTRACT_API_KEY",
        "vault-siliconflow-key",
        "https://api.siliconflow.cn/v1/chat/completions",
        "vault-extract-model",
    )
    .await;
    let before = published_snapshot(&server, &["SILICONFLOW_API_KEY"]);

    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("EXTRACT_BASE_URL")
                .map_err(|err| err.to_string())?
                .expect("extract URL entry should exist");
            entry.encrypted_value = "not valid base64".to_string();
            store
                .vault_upsert_entry(&entry)
                .map_err(|err| err.to_string())
        })
        .expect("corrupt extract URL ciphertext");

    let err = crate::provider_config::materialize_for_server(&server)
        .expect_err("corrupt lane ciphertext must refuse refresh");
    assert!(err.contains("Bad ciphertext base64"), "{err}");
    assert_eq!(
        before,
        published_snapshot(&server, &["SILICONFLOW_API_KEY"]),
        "decrypt failure must leave pools, overlay, and catalog unchanged"
    );
}

/// A failure after the first catalog row has been attempted must roll back the
/// catalog transaction before the provider state publication point is reached.
#[tokio::test]
async fn vault_catalog_failure_cannot_publish_a_mixed_state() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let _siliconflow =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "ambient-siliconflow-key");
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-catalog-password",
        "EXTRACT_API_KEY",
        "vault-siliconflow-key",
        "https://api.siliconflow.cn/v1/chat/completions",
        "vault-extract-model",
    )
    .await;
    let before = published_snapshot(&server, &["SILICONFLOW_API_KEY"]);
    install_catalog_failure_trigger(&server);

    let response =
        set_vault_value(&server, "EXTRACT_MODEL", "vault-extract-model-v2", "other").await;
    drop_catalog_failure_trigger(&server);

    assert!(
        response.contains("provider_secret_refresh_warning"),
        "the Vault write must report the failed refresh: {response}"
    );
    assert!(
        response.contains("pr1862 injected catalog failure"),
        "the warning must identify the injected catalog failure: {response}"
    );
    assert_eq!(
        before,
        published_snapshot(&server, &["SILICONFLOW_API_KEY"]),
        "catalog rollback must prevent a mixed catalog/provider publication"
    );
}

/// Provider pools and lane config are decrypted from one durable entries
/// snapshot. A Vault write after that scan must become visible only on the
/// next refresh, never as a mixed old-key/new-model publication.
#[tokio::test]
async fn vault_refresh_publishes_one_source_epoch_across_pool_and_lane_config() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-source-epoch-password",
        "EXTRACT_API_KEY",
        "epoch-one-key",
        "https://baseline-extract.example/v1/chat/completions",
        "epoch-one-model",
    )
    .await;
    set_vault_value(&server, "STAGED_API_KEY", "epoch-two-key", "api_key").await;
    set_vault_value(&server, "STAGED_MODEL", "epoch-two-model", "other").await;

    let db_path = server.global_db_path_buf();
    crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
        crate::test_support::with_unrestricted_fixture_connection(&db_path, |connection| {
            connection.execute_batch(
                "UPDATE vault_entries
                    SET encrypted_value = (SELECT encrypted_value FROM vault_entries WHERE name = 'STAGED_API_KEY'),
                        nonce = (SELECT nonce FROM vault_entries WHERE name = 'STAGED_API_KEY')
                  WHERE name = 'OPENAI_API_KEY';
                 UPDATE vault_entries
                    SET encrypted_value = (SELECT encrypted_value FROM vault_entries WHERE name = 'STAGED_MODEL'),
                        nonce = (SELECT nonce FROM vault_entries WHERE name = 'STAGED_MODEL')
                  WHERE name = 'EXTRACT_MODEL';",
            )
        })
        .expect("stage the next durable Vault generation");
    })
    .expect("publish the already-scanned generation");

    let first = server.llm.runtime_config();
    assert_eq!(first.extract.model, "epoch-one-model");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .as_deref(),
        Some("epoch-one-key")
    );

    crate::provider_config::materialize_for_server(&server)
        .expect("publish the next complete durable generation");
    let second = server.llm.runtime_config();
    assert_eq!(second.extract.model, "epoch-two-model");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .as_deref(),
        Some("epoch-two-key")
    );
}

/// Vault lane URLs are validated before either the catalog or the provider
/// cache sees them. The rejected value must not appear in the error surface.
#[tokio::test]
async fn credential_bearing_vault_lane_url_is_rejected_before_publish() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let _siliconflow =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "ambient-siliconflow-key");
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-url-password",
        "EXTRACT_API_KEY",
        "vault-siliconflow-key",
        "https://api.siliconflow.cn/v1/chat/completions",
        "vault-extract-model",
    )
    .await;
    let before = published_snapshot(&server, &["SILICONFLOW_API_KEY"]);

    let response = set_vault_value(
        &server,
        "EXTRACT_BASE_URL",
        "https://svc-account:sk-live-SECRET@proxy.internal:8443/v1/chat",
        "other",
    )
    .await;
    let err = crate::provider_config::materialize_for_server(&server)
        .expect_err("credential-bearing Vault URL must refuse refresh");

    assert!(
        response.contains("provider_secret_refresh_warning"),
        "the Vault write must report URL validation failure: {response}"
    );
    assert!(err.contains("refused before publication"), "{err}");
    assert!(!response.contains("sk-live-SECRET"), "{response}");
    assert!(!err.contains("sk-live-SECRET"), "{err}");
    assert_eq!(
        before,
        published_snapshot(&server, &["SILICONFLOW_API_KEY"]),
        "credential-bearing URL rejection must leave the prior snapshot intact"
    );
}

/// A Vault endpoint is explicit provider identity. It must be validated
/// against the exact logical credential pool before catalog or runtime state
/// is published, rather than being silently rebound at request time.
#[tokio::test]
async fn mismatched_known_provider_overlay_is_rejected_before_publish() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let server = materialization_test_server();
    initialize_vault(&server, "pr1862-provider-mismatch-password").await;
    set_vault_value(
        &server,
        "SILICONFLOW_API_KEY",
        "vault-siliconflow-key",
        "api_key",
    )
    .await;
    set_vault_value(
        &server,
        "EXTRACT_BASE_URL",
        "https://api.deepseek.com/chat/completions",
        "other",
    )
    .await;

    let before = published_snapshot(&server, &["SILICONFLOW_API_KEY"]);
    let err = crate::provider_config::materialize_for_server(&server)
        .expect_err("known-provider endpoint/key mismatch must refuse refresh");
    assert!(
        err.contains("refusing credential-bearing request"),
        "unexpected mismatch error: {err}"
    );
    assert_eq!(
        before,
        published_snapshot(&server, &["SILICONFLOW_API_KEY"]),
        "mismatched provider identity must publish neither catalog nor runtime state"
    );
}

/// Validation covers every logical key the production selector may reach,
/// not just the first currently healthy alias. Otherwise health/cooldown
/// failover could cross-bind a later known-provider credential after publish.
#[tokio::test]
async fn secondary_provider_pool_mismatch_is_rejected_before_publish() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let _alias =
        crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "vault:SILICONFLOW_API_KEY");
    let server = materialization_test_server();
    initialize_vault(&server, "pr1862-secondary-provider-password").await;
    set_vault_value(
        &server,
        "SILICONFLOW_API_KEY",
        "vault-siliconflow-key",
        "api_key",
    )
    .await;
    set_vault_value(
        &server,
        "EXTRACT_BASE_URL",
        "https://custom-lane.example/v1/chat/completions",
        "other",
    )
    .await;

    let before = published_snapshot(&server, &["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]);
    let err = crate::provider_config::materialize_for_server(&server)
        .expect_err("secondary known-provider pool mismatch must refuse refresh");
    assert!(
        err.contains("refusing credential-bearing request"),
        "unexpected secondary mismatch error: {err}"
    );
    assert_eq!(
        before,
        published_snapshot(&server, &["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]),
        "secondary provider mismatch must publish no partial snapshot"
    );
}

/// User-initiated lock purges both Vault credentials and Vault-derived lane
/// URL/model state. Auto-lock remains key-only by design.
#[tokio::test]
async fn explicit_vault_lock_clears_lane_overlay() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-lock-password",
        "EXTRACT_API_KEY",
        "vault-extract-key",
        "https://vault-lock.example/v1/chat/completions",
        "vault-lock-model",
    )
    .await;
    assert_eq!(
        server.llm.runtime_config().extract.base_url,
        "https://vault-lock.example/v1/chat/completions"
    );

    server.vault_lock().await.expect("explicit vault lock");

    let live = server.llm.runtime_config();
    assert_eq!(
        live.extract.base_url,
        "https://baseline-extract.example/v1/chat/completions"
    );
    assert_eq!(live.extract.model, "baseline-extract-model");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .is_none(),
        "explicit lock must purge the Vault provider pool"
    );
}

/// A readable missing alias is a revocation, while the same missing alias from
/// an unreadable Vault is retained as last-known-good provider state.
#[tokio::test]
async fn readable_missing_alias_revokes_but_unavailable_alias_retains() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let _alias =
        crate::test_support::EnvRestore::set("EXTRACT_API_KEY", "vault:SILICONFLOW_API_KEY");
    // macOS-only: this external test crate compiles the server without
    // `cfg(test)`, so the product auto-unlock path shells out to the ambient
    // `security` executable. A PATH-local missing-entry fixture keeps this test
    // off the host Keychain without bypassing the real product reader. The
    // tempdir binding is declared before the PATH guard so it outlives PATH
    // restoration (locals drop in reverse declaration order).
    #[cfg(target_os = "macos")]
    let security_fixture_dir = tempfile::tempdir().expect("security fixture directory");
    #[cfg(target_os = "macos")]
    let security_fixture_marker = security_fixture_dir.path().join("security.args");
    #[cfg(target_os = "macos")]
    let _security_fixture_path = {
        use std::os::unix::fs::PermissionsExt;
        let security = security_fixture_dir.path().join("security");
        // The marker path is derived inside the shell from `$0`, so no
        // filesystem path is ever interpolated into this script text.
        std::fs::write(
            &security,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$0.args\"\nexit 1\n",
        )
        .expect("write missing-entry security fixture");
        std::fs::set_permissions(&security, std::fs::Permissions::from_mode(0o700))
            .expect("executable security fixture");
        let mut paths = vec![security_fixture_dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let joined = std::env::join_paths(paths).expect("join security fixture PATH");
        crate::test_support::EnvRestore::set_path("PATH", Path::new(&joined))
    };
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-alias-password",
        "SILICONFLOW_API_KEY",
        "vault-alias-key",
        "https://api.siliconflow.cn/v1/chat/completions",
        "vault-extract-model",
    )
    .await;
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .is_some(),
        "the readable alias must initially materialize"
    );

    server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            agent_id: None,
        }))
        .await
        .expect("readable alias removal should succeed");
    crate::provider_config::materialize_for_server(&server)
        .expect("readable alias removal refresh");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .is_none(),
        "a readable missing alias must revoke its cached pool"
    );

    set_vault_value(
        &server,
        "SILICONFLOW_API_KEY",
        "vault-alias-key-restored",
        "api_key",
    )
    .await;
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .is_some(),
        "the alias must be materialized again before the unavailable case"
    );

    {
        let mut vault = server.vault_write();
        vault.key = None;
        vault.unlock_time = None;
    }
    server
        .with_global_store(|store| {
            store
                .vault_delete_entry("SILICONFLOW_API_KEY")
                .map_err(|err| err.to_string())
        })
        .expect("remove alias from the now-unavailable fixture source");

    let report = crate::provider_config::materialize_for_server_without_keychain_for_tests(&server)
        .expect("an unavailable Vault should retain the last-known-good alias pool");
    assert!(
        report
            .retained_from_last_known_good
            .iter()
            .any(|name| name == "EXTRACT_API_KEY"),
        "the unavailable alias disposition must be reported"
    );
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .is_some(),
        "an unavailable missing alias must retain its cached pool"
    );

    // The fixture must have been exercised by the real product reader: the
    // missing-entry result above is only deterministic if `security` resolved
    // to this PATH-local shim and received the expected request.
    #[cfg(target_os = "macos")]
    {
        let invoked = std::fs::read_to_string(&security_fixture_marker)
            .expect("the missing-entry security fixture must have been invoked");
        let args: Vec<&str> = invoked.lines().collect();
        assert!(!args.is_empty(), "security fixture must have been invoked");
        assert_eq!(
            args.len() % 6,
            0,
            "security fixture witness must be whole requests: {invoked}"
        );
        let expected = [
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ];
        for request in args.chunks_exact(6) {
            assert_eq!(
                request,
                expected.as_slice(),
                "security fixture must receive the exact find-generic-password request"
            );
        }
    }
}

/// Standalone bootstrap/probe materialization reads the same Vault lane
/// overlay from durable custody as the server refresh path.
#[tokio::test]
async fn standalone_materialization_sees_vault_lane_url_and_model() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let server = materialization_test_server();
    initialize_vault(&server, "pr1862-standalone-password").await;
    set_vault_value(
        &server,
        "EXTRACT_BASE_URL",
        "https://standalone-vault.example/v1/chat/completions",
        "other",
    )
    .await;
    set_vault_value(&server, "EXTRACT_MODEL", "standalone-vault-model", "other").await;

    let standalone = tachi_llm::LlmClient::new_with_config(materialization_test_config(), None)
        .expect("standalone client");
    crate::provider_config::materialize_standalone_with_password_for_tests(
        &standalone,
        &server.global_db_path_buf(),
        "pr1862-standalone-password",
        None,
    )
    .expect("standalone provider materialization");
    let live = standalone.runtime_config();
    assert_eq!(
        live.extract.base_url,
        "https://standalone-vault.example/v1/chat/completions"
    );
    assert_eq!(live.extract.model, "standalone-vault-model");
}

/// When standalone custody falls back from an uninitialized custom DB to the
/// default Vault, provider pools and lane config must come from that same
/// selected durable source.
#[tokio::test]
async fn standalone_default_vault_fallback_uses_matching_lane_overlay() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let server = materialization_test_server();
    let default_db = server.global_db_path_buf();
    let fixture_root = default_db
        .parent()
        .and_then(Path::parent)
        .expect("fixture root")
        .to_path_buf();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &fixture_root);
    seed_provider_and_lane(
        &server,
        "pr1862-default-fallback-password",
        "EXTRACT_API_KEY",
        "default-vault-extract-key",
        "https://default-vault.example/v1/chat/completions",
        "default-vault-model",
    )
    .await;

    let custom_db = fixture_root
        .join("custom")
        .join(memcore::MEMORY_DB_FILENAME);
    let standalone = tachi_llm::LlmClient::new_with_config(materialization_test_config(), None)
        .expect("standalone fallback client");
    crate::provider_config::materialize_standalone_with_password_for_tests(
        &standalone,
        &custom_db,
        "pr1862-default-fallback-password",
        None,
    )
    .expect("standalone default Vault fallback");

    let live = standalone.runtime_config();
    assert_eq!(
        live.extract.base_url,
        "https://default-vault.example/v1/chat/completions"
    );
    assert_eq!(live.extract.model, "default-vault-model");
    assert_eq!(
        standalone
            .provider_secret_for_tests(&["EXTRACT_API_KEY"])
            .as_deref(),
        Some("default-vault-extract-key")
    );
}

/// The rows committed by the serve materialization must be the exact identity
/// projection of the effective client snapshot, including the Vault overlay.
#[tokio::test]
async fn vault_catalog_rows_match_the_effective_runtime_snapshot() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider_env = clear_model_provider_env();
    let _siliconflow =
        crate::test_support::EnvRestore::set("SILICONFLOW_API_KEY", "ambient-siliconflow-key");
    let server = materialization_test_server();
    seed_provider_and_lane(
        &server,
        "pr1862-catalog-match-password",
        "EXTRACT_API_KEY",
        "vault-siliconflow-key",
        "https://api.siliconflow.cn/v1/chat/completions",
        "catalog-match-model",
    )
    .await;

    let effective = server.llm.runtime_config();
    let expected = tachi_llm::env_chat_lane_deployments(&effective, "snapshot-observed")
        .expect("effective chat lanes should project");
    let rows = env_catalog_rows(&server);
    for expected_lane in expected {
        let actual = rows
            .iter()
            .find(|row| row.deployment_id == expected_lane.deployment.deployment_id)
            .unwrap_or_else(|| panic!("missing catalog row {}", expected_lane.lane));
        assert_eq!(
            actual.content_digest(),
            expected_lane.deployment.content_digest(),
            "catalog row {} must match the effective runtime snapshot",
            expected_lane.lane
        );
    }
}
