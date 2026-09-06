#![allow(clippy::await_holding_lock)]

use super::*;
use crate::test_support::EnvRestore;
use memcore::vault::VaultKeyHealth;

const SLOT: &str = "EXTRACT_API_KEY";
const ACCOUNT: &str = "SILICONFLOW_API_KEY";
const PASSWORD: &str = "health-publication-fixture-password";
const SECRET: &str = "health-publication-fixture-secret";

fn controlled_env() -> Vec<EnvRestore> {
    let names = crate::provider_config::provider_env_keys()
        .into_iter()
        .flat_map(|name| {
            crate::status_ops::status_health::family_env_names_for_env_name(&name)
                .expect("admitted provider name has a registry family")
        })
        .collect::<std::collections::HashSet<_>>();
    let mut guards = names
        .into_iter()
        .map(EnvRestore::remove)
        .collect::<Vec<_>>();
    guards.extend(
        [
            "VOYAGE_BASE_URL",
            "TACHI_EMBEDDING_MODEL",
            "TACHI_EMBEDDING_DIM",
            "EXTRACT_BASE_URL",
            "EXTRACT_MODEL",
            "SUMMARY_BASE_URL",
            "SUMMARY_MODEL",
            "REASONING_BASE_URL",
            "REASONING_MODEL",
            "DISTILL_BASE_URL",
            "DISTILL_MODEL",
            "EXTRACT_FALLBACK_API_KEY",
            "SUMMARY_FALLBACK_API_KEY",
            "REASONING_FALLBACK_API_KEY",
            "DISTILL_FALLBACK_API_KEY",
        ]
        .into_iter()
        .map(EnvRestore::remove),
    );
    guards.push(EnvRestore::set("TACHI_TEST_FORCE_KEYCHAIN_MISSING", "1"));
    guards
}

fn client_config() -> tachi_llm::ProviderRuntimeConfig {
    let lane = |key| tachi_llm::ChatLaneConfig {
        base_url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
        model: "health-publication-model".to_string(),
        api_key_envs: vec![key],
    };
    tachi_llm::ProviderRuntimeConfig {
        extract: lane(SLOT),
        summary: lane("SUMMARY_API_KEY"),
        reasoning: lane("REASONING_API_KEY"),
        distill: lane("DISTILL_API_KEY"),
        rerank: tachi_llm::RerankConfig {
            provider: tachi_llm::RerankProviderKind::Local,
            local_endpoint: Some("http://127.0.0.1:9/rerank".to_string()),
        },
    }
}

async fn bound_server(persist_health: bool) -> crate::tests::TestServer {
    let mut server = make_server();
    let db_path = server.global_db_path_buf();
    server.replace_llm(
        tachi_llm::LlmClient::new_with_config(
            client_config(),
            persist_health.then_some(db_path.as_path()),
        )
        .expect("fixture client"),
    );
    assert_eq!(
        server.llm.provider_health_status().source_of_truth,
        if persist_health {
            "vault_db"
        } else {
            "memory_only"
        }
    );
    server
        .vault_init(Parameters(VaultInitParams {
            password: PASSWORD.to_string(),
        }))
        .await
        .expect("vault_init");
    for (name, value) in [(ACCOUNT, SECRET), (SLOT, "vault:SILICONFLOW_API_KEY")] {
        let result = server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "health publication boundary fixture".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            }))
            .await
            .expect("seed actual account and bound slot");
        if name == SLOT {
            let result: Value = serde_json::from_str(&result).expect("slot result JSON");
            assert_eq!(result["bound_account"], ACCOUNT);
        }
    }
    let scan = crate::vault_ops::load_validated_unlocked_api_key_secret_pools_with_drops(&server)
        .expect("scan actual binding");
    assert_eq!(scan.pools[SLOT].len(), 1);
    assert_eq!(scan.pools[SLOT][0].key_id, ACCOUNT);
    assert_eq!(scan.pools[SLOT][0].value, SECRET);
    assert_eq!(
        server.llm.provider_secret_for_tests(&[SLOT]).as_deref(),
        Some(SECRET)
    );
    assert!(health_rows(&server).is_empty());
    assert!(server.llm.provider_health_memory_snapshot().is_empty());
    server
}

fn health_rows(server: &crate::MemoryServer) -> Vec<VaultKeyHealth> {
    server
        .with_global_store_read(|store| {
            store.vault_list_key_health(None).map_err(|e| e.to_string())
        })
        .expect("persisted health rows")
}

fn runtime_snapshot(
    server: &crate::MemoryServer,
) -> (
    tachi_llm::ProviderRuntimeConfig,
    Vec<memcore::catalog::ModelDeployment>,
) {
    let rows = server
        .with_global_store_read(|store| {
            memcore::db::model_catalog::list_model_deployments_by_source(
                store.connection(),
                memcore::catalog::CatalogSource::Env,
            )
            .map_err(|e| e.to_string())
        })
        .expect("catalog snapshot");
    (server.llm.runtime_config(), rows)
}

fn assert_failed_identity(row: &VaultKeyHealth, logical: &str) {
    assert_eq!(row.logical_name, logical);
    assert_eq!(row.key_id, ACCOUNT);
    assert_eq!(row.status, "auth_failed");
    assert!(row.auth_failed);
    assert!(!row.disabled);
    assert_eq!(row.error_count, 1);
}

async fn reject_runtime_health_change(logical: &'static str, persist_health: bool) {
    let server = bound_server(persist_health).await;
    let before = runtime_snapshot(&server);
    let llm = std::sync::Arc::clone(&server.llm);
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let error =
        crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
            let row = if persist_health {
                llm.record_provider_key_result_blocking(
                    logical,
                    ACCOUNT,
                    Some(401),
                    None,
                    None,
                    None,
                )
            } else {
                llm.record_provider_key_result(logical, ACCOUNT, Some(401), None, None, None)
            };
            assert_failed_identity(&row, logical);
            observed_tx
                .send(row)
                .expect("capture post-scan observation");
        })
        .expect_err("a new failure for an admitted bound credential must reject publication");
    assert!(
        error.contains("health") && error.contains("before publication"),
        "{error}"
    );
    let observed = observed_rx.try_recv().expect("post-scan hook ran");
    server
        .llm
        .await_provider_health_persistence()
        .await
        .expect("drain fixture persistence");

    assert_eq!(
        runtime_snapshot(&server),
        before,
        "failed refresh is content-atomic"
    );
    let memory = server.llm.provider_health_memory_snapshot();
    assert_eq!(
        memory.len(),
        1,
        "do not synthesize health rows for other identities"
    );
    assert_eq!(memory[logical].len(), 1);
    assert_eq!(
        serde_json::to_value(&memory[logical][ACCOUNT]).unwrap(),
        serde_json::to_value(observed).unwrap()
    );
    let persisted = health_rows(&server);
    if persist_health {
        assert_eq!(persisted.len(), 1);
        assert_failed_identity(&persisted[0], logical);
    } else {
        assert!(
            persisted.is_empty(),
            "no-DB client must actually leave durable health untouched"
        );
    }
    assert_eq!(
        server.llm.provider_secret_for_tests(&[SLOT]),
        None,
        "bound slot must be unselectable"
    );
    let statuses = server.llm.provider_pool_statuses();
    let slot = statuses
        .iter()
        .find(|pool| pool.logical_name == SLOT)
        .expect("prior slot pool");
    assert_eq!((slot.total_keys, slot.available_keys), (1, 0));
    assert_eq!(slot.rate_limited_keys.len(), 1);
    assert_eq!(slot.rate_limited_keys[0].key_id, ACCOUNT);
    if logical == ACCOUNT {
        assert_eq!(server.llm.provider_secret_for_tests(&[ACCOUNT]), None);
    } else {
        assert_eq!(
            server.llm.provider_secret_for_tests(&[ACCOUNT]).as_deref(),
            Some(SECRET),
            "a slot-specific failure must not fabricate an account-wide failure"
        );
    }
}

#[tokio::test]
async fn persisted_slot_health_change_refuses_bound_slot_publication() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    reject_runtime_health_change(SLOT, true).await;
}

#[tokio::test]
async fn persisted_account_health_change_refuses_bound_slot_publication() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    reject_runtime_health_change(ACCOUNT, true).await;
}

#[tokio::test]
async fn memory_only_slot_health_change_refuses_bound_slot_publication() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    reject_runtime_health_change(SLOT, false).await;
}

#[tokio::test]
async fn memory_only_account_health_change_refuses_bound_slot_publication() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    reject_runtime_health_change(ACCOUNT, false).await;
}

#[tokio::test]
async fn successful_publication_preserves_new_success_and_unrelated_memory_failure() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    let server = bound_server(false).await;
    let llm = std::sync::Arc::clone(&server.llm);
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let report =
        crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
            let success =
                llm.record_provider_key_result(SLOT, ACCOUNT, Some(200), None, None, None);
            let unrelated = llm.record_provider_key_result(
                "UNRELATED_API_KEY",
                "UNRELATED_API_KEY",
                Some(401),
                None,
                None,
                None,
            );
            observed_tx
                .send((success, unrelated))
                .expect("capture outcomes");
        })
        .expect("success and unrelated failure cannot invalidate the admitted credential");
    let (success, unrelated) = observed_rx.try_recv().expect("post-scan hook ran");
    let memory = server.llm.provider_health_memory_snapshot();
    assert_eq!(
        serde_json::to_value(&memory[SLOT][ACCOUNT]).unwrap(),
        serde_json::to_value(success).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&memory["UNRELATED_API_KEY"]["UNRELATED_API_KEY"]).unwrap(),
        serde_json::to_value(unrelated).unwrap()
    );
    assert!(report.skipped_aliases.is_empty());
    assert!(health_rows(&server).is_empty());
    assert_eq!(
        server.llm.provider_secret_for_tests(&[SLOT]).as_deref(),
        Some(SECRET)
    );
}

#[tokio::test]
async fn legacy_slot_self_health_does_not_poison_a_bound_account_publication() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    for persist_health in [false, true] {
        let server = bound_server(persist_health).await;
        let llm = std::sync::Arc::clone(&server.llm);
        crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
            let legacy =
                llm.record_provider_key_result_blocking(SLOT, SLOT, Some(401), None, None, None);
            assert!(legacy.auth_failed);
        })
        .expect("legacy copied-slot health is not the bound account identity");
        server
            .llm
            .await_provider_health_persistence()
            .await
            .expect("drain fixture persistence");
        let memory = server.llm.provider_health_memory_snapshot();
        assert!(
            memory[SLOT][SLOT].auth_failed,
            "preserve the actual legacy observation"
        );
        assert!(
            !memory[SLOT].contains_key(ACCOUNT),
            "do not rewrite it onto the account"
        );
        assert_eq!(
            server.llm.provider_secret_for_tests(&[SLOT]).as_deref(),
            Some(SECRET)
        );
        assert_eq!(
            server.llm.provider_secret_for_tests(&[ACCOUNT]).as_deref(),
            Some(SECRET)
        );
        let rows = health_rows(&server);
        assert_eq!(rows.len(), usize::from(persist_health));
        if persist_health {
            assert_eq!(
                (rows[0].logical_name.as_str(), rows[0].key_id.as_str()),
                (SLOT, SLOT)
            );
            assert!(rows[0].auth_failed);
        }
    }
}

fn persist_external_failure(
    path: &std::path::Path,
    logical: &str,
    key_id: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> VaultKeyHealth {
    let row = memcore::vault::health::record_key_outcome(
        None,
        logical,
        key_id,
        memcore::vault::health::TypedOutcome::AuthFailed,
        memcore::vault::health::EvidenceKind::SelfReported,
        None,
        at,
    )
    .health;
    let store = memcore::MemoryStore::open(path.to_str().expect("fixture DB path"))
        .expect("external writer");
    store
        .vault_upsert_key_health(&row)
        .expect("persist external observation");
    row
}

#[tokio::test]
async fn unrelated_durable_change_does_not_revalidate_an_unchanged_stale_health_row() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    let server = bound_server(false).await;
    let path = server.global_db_path_buf();
    let stale = persist_external_failure(
        &path,
        ACCOUNT,
        ACCOUNT,
        chrono::Utc::now() - chrono::Duration::days(2),
    );
    let success =
        server
            .llm
            .record_provider_key_result(ACCOUNT, ACCOUNT, Some(200), None, None, None);
    assert!(success.updated_at > stale.updated_at);
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let report =
        crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
            let row = persist_external_failure(
                &path,
                "UNRELATED_API_KEY",
                "UNRELATED_API_KEY",
                chrono::Utc::now(),
            );
            observed_tx.send(row).expect("capture external row");
        })
        .expect("unrelated drift must not defeat the scan's newer in-memory success");
    let unrelated = observed_rx.try_recv().expect("post-scan writer ran");
    let rows = health_rows(&server);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        serde_json::to_value(rows.iter().find(|row| row.logical_name == ACCOUNT).unwrap()).unwrap(),
        serde_json::to_value(stale).unwrap()
    );
    assert_eq!(
        serde_json::to_value(
            rows.iter()
                .find(|row| row.logical_name == "UNRELATED_API_KEY")
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(unrelated).unwrap()
    );
    assert!(report.skipped_aliases.is_empty());
    assert_eq!(
        server.llm.provider_secret_for_tests(&[SLOT]).as_deref(),
        Some(SECRET)
    );
}

#[tokio::test]
async fn unlocked_and_keychain_publication_refuse_external_bound_health_changes() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    for keychain in [false, true] {
        for logical in [SLOT, ACCOUNT] {
            let server = bound_server(false).await;
            let before = runtime_snapshot(&server);
            server
                .llm
                .clear_provider_secrets()
                .expect("empty cache discriminator");
            assert_eq!(server.llm.provider_secret_count(), 0);
            let path = server.global_db_path_buf();
            let writer_path = path.clone();
            let (observed_tx, observed_rx) = std::sync::mpsc::channel();
            let after_scan = move || {
                observed_tx
                    .send(persist_external_failure(
                        &writer_path,
                        logical,
                        ACCOUNT,
                        chrono::Utc::now(),
                    ))
                    .expect("capture external row");
            };
            let result = if keychain {
                let _missing = EnvRestore::remove("TACHI_TEST_FORCE_KEYCHAIN_MISSING");
                let _password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", PASSWORD);
                crate::provider_config::materialize_standalone_with_hook_for_tests(
                    server.llm.as_ref(),
                    &path,
                    after_scan,
                )
            } else {
                crate::provider_config::materialize_for_server_with_hook_for_tests(
                    &server, after_scan,
                )
            };
            let error = result.expect_err("durable-only failure must fence the captured slot");
            assert!(
                error.contains("health revision changed"),
                "keychain={keychain}, logical={logical}: {error}"
            );
            let observed = observed_rx
                .try_recv()
                .expect("external post-scan write ran");
            assert_failed_identity(&observed, logical);
            let rows = health_rows(&server);
            assert_eq!(rows.len(), 1);
            assert_eq!(
                serde_json::to_value(&rows[0]).unwrap(),
                serde_json::to_value(observed).unwrap()
            );
            assert!(
                server.llm.provider_health_memory_snapshot().is_empty(),
                "the revision fence, not an LLM outcome, must discriminate"
            );
            assert_eq!(server.llm.provider_secret_count(), 0);
            assert_eq!(server.llm.provider_secret_for_tests(&[SLOT]), None);
            assert_eq!(runtime_snapshot(&server), before);
        }
    }
}

#[tokio::test]
async fn unlocked_and_keychain_publication_preserve_captured_credential_generation() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = controlled_env();
    for keychain in [false, true] {
        let server = bound_server(false).await;
        let before = runtime_snapshot(&server);
        server
            .llm
            .clear_provider_secrets()
            .expect("empty cache discriminator");
        let path = server.global_db_path_buf();
        let key = {
            *server
                .vault_read()
                .key
                .as_ref()
                .expect("unlocked fixture key")
                .bytes()
        };
        let (encrypted, nonce) = crate::vault_crypto::encrypt(&key, b"replacement-fixture-secret")
            .expect("encrypt fixture replacement");
        let entry = server
            .with_global_store_read(|store| {
                store.vault_get_entry(ACCOUNT).map_err(|e| e.to_string())
            })
            .expect("read entry")
            .expect("account entry");
        let timestamp = entry.updated_at.clone();
        assert_ne!(entry.encrypted_value, encrypted);
        assert_ne!(entry.nonce, nonce);
        let writer_path = path.clone();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let after_scan =
            move || {
                let connection = rusqlite::Connection::open(&writer_path).expect("fixture writer");
                assert_eq!(connection.execute(
                "UPDATE vault_entries SET encrypted_value = ?1, nonce = ?2 WHERE name = ?3",
                rusqlite::params![encrypted, nonce, ACCOUNT],
            ).expect("replace bytes without timestamp mutation"), 1);
                let actual: (String, String, String) = connection.query_row(
                "SELECT encrypted_value, nonce, updated_at FROM vault_entries WHERE name = ?1",
                [ACCOUNT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).expect("read raw replacement");
                assert_eq!(actual, (encrypted, nonce, timestamp));
                observed_tx.send(()).expect("capture content mutation");
            };
        let result = if keychain {
            let _missing = EnvRestore::remove("TACHI_TEST_FORCE_KEYCHAIN_MISSING");
            let _password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", PASSWORD);
            crate::provider_config::materialize_standalone_with_hook_for_tests(
                server.llm.as_ref(),
                &path,
                after_scan,
            )
        } else {
            crate::provider_config::materialize_for_server_with_hook_for_tests(&server, after_scan)
        };
        result.expect("publish the complete captured generation");
        observed_rx
            .try_recv()
            .expect("post-scan content mutation ran");
        assert_eq!(
            server.llm.provider_secret_for_tests(&[SLOT]).as_deref(),
            Some(SECRET)
        );
        assert_eq!(server.llm.runtime_config(), before.0);
        if keychain {
            let _missing = EnvRestore::remove("TACHI_TEST_FORCE_KEYCHAIN_MISSING");
            let _password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", PASSWORD);
            crate::provider_config::materialize_standalone(server.llm.as_ref(), &path)
                .expect("publish the next complete Keychain generation");
        } else {
            crate::provider_config::materialize_for_server(&server)
                .expect("publish the next complete unlocked generation");
        }
        assert_eq!(
            server.llm.provider_secret_for_tests(&[SLOT]).as_deref(),
            Some("replacement-fixture-secret")
        );
        assert_eq!(server.llm.runtime_config(), before.0);
        assert!(health_rows(&server).is_empty());
    }
}
