use super::access::{
    lease_authorized_api_key_with_hook_for_tests,
    load_unlocked_api_key_secret_pools_with_acl_hook_for_tests,
    materialize_unrestricted_vault_entries_from_store_with_hook_for_tests,
    record_successful_vault_access,
    select_authorized_vault_entry_and_record_access_with_hook_for_tests,
};
use super::handlers::{
    handle_vault_get, handle_vault_init, handle_vault_lease_api_key, handle_vault_list,
    handle_vault_lock, handle_vault_remove, handle_vault_set, handle_vault_set_api_key_pool,
    handle_vault_setup_rotation, handle_vault_unlock,
};
use super::params::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams, VaultRemoveParams,
    VaultSetApiKeyPoolParams, VaultSetParams, VaultSetupRotationParams, VaultUnlockParams,
};
use super::session::{read_unlock_password_fifo, with_vault_key};
use crate::server_state::MemoryServer;
use crate::test_support::EnvRestore;
use std::time::{Duration, Instant};

mod slot_binding_acl;
mod slot_events;
mod slot_materialization;

#[test]
fn authorized_vault_read_serializes_acl_revocation_with_selection_and_touch() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-acl-read-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let mut reader_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open reader store");
    let writer_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open writer store");
    let now = chrono::Utc::now().to_rfc3339();
    let key = [7u8; 32];
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(&key, b"race-secret").expect("encrypt race fixture");
    let entry = memcore::vault::VaultEntry {
        name: "ACL_RACE_API_KEY".to_string(),
        encrypted_value,
        nonce,
        secret_type: "api_key".to_string(),
        description: "ACL race fixture".to_string(),
        allowed_agents: None,
        created_at: now.clone(),
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };
    reader_store
        .vault_upsert_entry(&entry)
        .expect("seed unrestricted entry");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer_handle = std::cell::RefCell::new(None);
    let writer_barrier = std::sync::Arc::clone(&barrier);
    let mut restricted = entry.clone();
    restricted.allowed_agents = Some(vec!["agent-a".to_string()]);

    let (_selected, value, access_count) =
        select_authorized_vault_entry_and_record_access_with_hook_for_tests(
            &mut reader_store,
            &VaultGetParams {
                name: entry.name.clone(),
                agent_id: None,
                auto_rotate: false,
            },
            None,
            &key,
            || {
                let handle = std::thread::spawn(move || {
                    attempt_tx.send(()).expect("announce ACL revocation");
                    writer_barrier.wait();
                    writer_store
                        .vault_upsert_entry(&restricted)
                        .expect("commit ACL revocation");
                    done_tx.send(()).expect("announce committed revocation");
                });
                attempt_rx
                    .recv()
                    .expect("writer reached revocation boundary");
                barrier.wait();
                assert!(
                    done_rx.recv_timeout(Duration::from_millis(250)).is_err(),
                    "ACL revocation must not commit between selection and authorization"
                );
                writer_handle.replace(Some(handle));
            },
        )
        .expect("read linearizes before the blocked ACL revocation");
    assert_eq!(value, "race-secret");
    assert_eq!(access_count, 1);
    writer_handle
        .into_inner()
        .expect("writer handle")
        .join()
        .expect("ACL writer thread");
    done_rx.recv().expect("ACL revocation committed");

    let denied = match select_authorized_vault_entry_and_record_access_with_hook_for_tests(
        &mut reader_store,
        &VaultGetParams {
            name: entry.name,
            agent_id: None,
            auto_rotate: false,
        },
        None,
        &key,
        || {},
    ) {
        Err(error) => error,
        Ok(_) => panic!("subsequent anonymous read must observe the committed ACL revocation"),
    };
    assert!(denied.contains("agent_id is required"), "{denied}");
}

#[test]
fn identityless_cli_materialization_serializes_acl_revocation_with_decrypt_and_touch() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-cli-materialize-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let mut reader_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open reader");
    let writer_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open writer");
    let key = [9u8; 32];
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(&key, b"legacy-export-secret").expect("encrypt");
    let now = chrono::Utc::now().to_rfc3339();
    let entry = memcore::vault::VaultEntry {
        name: "LEGACY_EXPORT_API_KEY".to_string(),
        encrypted_value,
        nonce,
        secret_type: "api_key".to_string(),
        description: String::new(),
        allowed_agents: None,
        created_at: now.clone(),
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };
    reader_store
        .vault_upsert_entry(&entry)
        .expect("seed unrestricted entry");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = std::sync::Arc::clone(&barrier);
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer_handle = std::cell::RefCell::new(None);
    let mut restricted = entry.clone();
    restricted.allowed_agents = Some(vec!["agent-a".to_string()]);

    let materialized = materialize_unrestricted_vault_entries_from_store_with_hook_for_tests(
        &mut reader_store,
        &key,
        |_| true,
        || {
            let handle = std::thread::spawn(move || {
                attempt_tx.send(()).expect("announce ACL revocation");
                writer_barrier.wait();
                writer_store
                    .vault_upsert_entry(&restricted)
                    .expect("commit ACL revocation");
                done_tx.send(()).expect("announce committed revocation");
            });
            attempt_rx
                .recv()
                .expect("writer reached revocation boundary");
            barrier.wait();
            assert!(
                done_rx.recv_timeout(Duration::from_millis(250)).is_err(),
                "ACL revocation must not commit between snapshot and decrypt/touch"
            );
            writer_handle.replace(Some(handle));
        },
    )
    .expect("materialization linearizes before ACL revocation");
    assert_eq!(
        materialized,
        vec![(entry.name.clone(), "legacy-export-secret".to_string())]
    );
    writer_handle
        .into_inner()
        .expect("writer handle")
        .join()
        .expect("writer thread");
    done_rx.recv().expect("ACL revocation committed");
    let retained = reader_store
        .vault_get_entry(&entry.name)
        .expect("read entry")
        .expect("entry remains");
    assert_eq!(retained.access_count, 1);

    let subsequent = materialize_unrestricted_vault_entries_from_store_with_hook_for_tests(
        &mut reader_store,
        &key,
        |_| true,
        || {},
    )
    .expect("subsequent materialization");
    assert!(
        subsequent.is_empty(),
        "restricted row must no longer materialize"
    );
}

#[test]
fn mcp_api_key_lease_serializes_acl_revocation_with_selection_rotation_and_touch() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-mcp-lease-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create server");
    let key = [10u8; 32];
    {
        let mut vault = server.vault_write();
        vault.key = Some(crate::CachedVaultKey::copy_from(&key));
        vault.unlock_time = Some(Instant::now());
    }
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(&key, b"leased-secret").expect("encrypt");
    let now = chrono::Utc::now().to_rfc3339();
    let entry = memcore::vault::VaultEntry {
        name: "LEASE_RACE_API_KEY_1".to_string(),
        encrypted_value,
        nonce,
        secret_type: "api_key".to_string(),
        description: String::new(),
        allowed_agents: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        accessed_at: String::new(),
        access_count: 0,
    };
    server
        .with_global_store(|store| {
            store
                .vault_upsert_entry(&entry)
                .map_err(|e| e.to_string())?;
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "LEASE_RACE_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 1,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|e| e.to_string())
        })
        .expect("seed pool");
    let writer_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open writer");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = std::sync::Arc::clone(&barrier);
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer_handle = std::cell::RefCell::new(None);
    let mut restricted = entry.clone();
    restricted.allowed_agents = Some(vec!["agent-a".to_string()]);

    let lease =
        lease_authorized_api_key_with_hook_for_tests(&server, "LEASE_RACE_API_KEY", None, || {
            let handle = std::thread::spawn(move || {
                attempt_tx.send(()).expect("announce ACL revocation");
                writer_barrier.wait();
                writer_store
                    .vault_upsert_entry(&restricted)
                    .expect("commit ACL revocation");
                done_tx.send(()).expect("announce committed revocation");
            });
            attempt_rx
                .recv()
                .expect("writer reached revocation boundary");
            barrier.wait();
            assert!(
                done_rx.recv_timeout(Duration::from_millis(250)).is_err(),
                "ACL revocation must not commit between lease selection and rotation/touch"
            );
            writer_handle.replace(Some(handle));
        })
        .expect("lease linearizes before ACL revocation");
    assert_eq!(lease.logical_name, "LEASE_RACE_API_KEY");
    assert_eq!(lease.key_id, entry.name);
    assert_eq!(lease.value, "leased-secret");
    assert_eq!(lease.access_count, 1);
    writer_handle
        .into_inner()
        .expect("writer handle")
        .join()
        .expect("writer thread");
    done_rx.recv().expect("ACL revocation committed");

    let denied =
        lease_authorized_api_key_with_hook_for_tests(&server, "LEASE_RACE_API_KEY", None, || {})
            .expect_err("subsequent anonymous lease must observe committed ACL");
    assert!(
        denied.contains("restricted") || denied.contains("No usable"),
        "{denied}"
    );
}

#[test]
fn provider_materialization_serializes_acl_revocation_with_decrypt_and_touch() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-acl-materialize-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create test server");
    let key = [8u8; 32];
    {
        let mut vault = server.vault_write();
        vault.key = Some(crate::CachedVaultKey::copy_from(&key));
        vault.unlock_time = Some(Instant::now());
    }
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(&key, b"materialized-secret").expect("encrypt fixture");
    let now = chrono::Utc::now().to_rfc3339();
    let entry = memcore::vault::VaultEntry {
        name: "OPENAI_API_KEY".to_string(),
        encrypted_value,
        nonce,
        secret_type: "api_key".to_string(),
        description: "materialization ACL race fixture".to_string(),
        allowed_agents: None,
        created_at: now.clone(),
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };
    server
        .with_global_store(|store| {
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .expect("seed unrestricted provider entry");
    let writer_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open writer store");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = std::sync::Arc::clone(&barrier);
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer_handle = std::cell::RefCell::new(None);
    let mut restricted = entry;
    restricted.allowed_agents = Some(vec!["agent-a".to_string()]);

    let pools = load_unlocked_api_key_secret_pools_with_acl_hook_for_tests(&server, || {
        let handle = std::thread::spawn(move || {
            attempt_tx.send(()).expect("announce ACL revocation");
            writer_barrier.wait();
            writer_store
                .vault_upsert_entry(&restricted)
                .expect("commit ACL revocation");
            done_tx.send(()).expect("announce committed revocation");
        });
        attempt_rx
            .recv()
            .expect("writer reached materialization revocation boundary");
        barrier.wait();
        assert!(
            done_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "ACL revocation must not commit between provider snapshot and decrypt/touch"
        );
        writer_handle.replace(Some(handle));
    })
    .expect("materialization linearizes before the blocked ACL revocation");
    assert_eq!(pools["OPENAI_API_KEY"][0].value, "materialized-secret");
    writer_handle
        .into_inner()
        .expect("writer handle")
        .join()
        .expect("ACL writer thread");
    done_rx.recv().expect("ACL revocation committed");

    let after = crate::vault_ops::load_unlocked_api_key_secret_pools(&server)
        .expect("scan after ACL revocation");
    assert!(
        !after.contains_key("OPENAI_API_KEY"),
        "subsequent materialization must observe the committed ACL fence"
    );
}

#[test]
fn provider_publication_refuses_acl_revision_drift_after_resolution() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::set("OPENAI_API_KEY", "vault:OPENAI_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-acl-publish-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create test server");
    let key = [10u8; 32];
    {
        let mut vault = server.vault_write();
        vault.key = Some(crate::CachedVaultKey::copy_from(&key));
        vault.unlock_time = Some(Instant::now());
    }
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(&key, b"must-not-publish").expect("encrypt fixture");
    let now = chrono::Utc::now().to_rfc3339();
    let entry = memcore::vault::VaultEntry {
        name: "OPENAI_API_KEY".to_string(),
        encrypted_value,
        nonce,
        secret_type: "api_key".to_string(),
        description: "publication ACL drift fixture".to_string(),
        allowed_agents: None,
        created_at: now.clone(),
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };
    server
        .with_global_store(|store| {
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .expect("seed provider entry");
    let writer_store =
        memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).expect("open writer store");
    let mut restricted = entry;
    restricted.allowed_agents = Some(vec!["agent-a".to_string()]);

    let error =
        crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
            writer_store
                .vault_upsert_entry(&restricted)
                .expect("commit ACL revocation after resolution");
        })
        .expect_err("ACL revision drift must refuse stale provider publication");
    assert!(error.contains("changed before publication"), "{error}");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_none(),
        "stale resolved plaintext must not publish after ACL revocation"
    );

    memcore::store::vault::VaultMutationFence::acquire(&db_path)
        .expect("stale publication fence released after refusal")
        .rollback()
        .expect("release probe fence");

    crate::provider_config::materialize_for_server(&server)
        .expect("fresh refresh observes restricted provider entry");
    assert!(server
        .llm
        .provider_secret_for_tests(&["OPENAI_API_KEY"])
        .is_none());
}

#[tokio::test]
async fn with_vault_key_drops_vault_lock_before_running_work() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-lock-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        v.unlock_time = Some(Instant::now());
    }

    with_vault_key(&server, |cached_key| {
        assert_eq!(cached_key, &key);
        assert!(
            server.vault.try_write().is_ok(),
            "vault lock should not be held while user work runs"
        );
        Ok(())
    })
    .expect("vault key should be available");
}

// Discrimination test (a) for #400: auto-lock must clear the vault master key
// but PRESERVE provider secrets. Provider secrets are materialized from the
// vault at unlock time and have a lifecycle independent of the master key;
// auto-lock is "don't keep the key in memory long-term", NOT "stop the service".
//
// RED before fix: the old `with_vault_key` auto-lock path called
// `clear_provider_secrets()` + `re_materialize_provider_secrets_after_auto_lock()`,
// wiping the materialized key. On a headless Linux router (no Keychain) the
// re-materialize failed silently, leaving zero keys → the assertion
// `== Some("cached")` would fail because the value was gone.
#[tokio::test]
async fn auto_lock_clears_key_but_preserves_provider_secrets() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-auto-lock-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        v.unlock_time = Some(Instant::now() - Duration::from_secs(60));
        v.auto_lock_after_secs = 30;
    }
    assert!(server.llm.set_provider_secret("OPENAI_API_KEY", "cached"));

    let err = with_vault_key(&server, |_| Ok(())).expect_err("expired key should auto-lock");

    assert!(err.contains("Vault auto-locked"), "{err}");
    {
        let v = server.vault_read();
        assert!(v.key.is_none());
        assert!(v.unlock_time.is_none());
    }
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("cached"),
        "auto-lock must NOT clear provider secrets — they have a lifecycle independent of the vault master key"
    );
}

// Holds `global_test_lock` across awaits on purpose: the guard serializes
// process-wide provider/env state for the whole init -> set -> lock sequence
// this race test stages, so releasing it at any await would let a sibling test
// interleave and destroy the condition under test.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn explicit_lock_dominates_refresh_with_prelock_resolved_vault_pools() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-prelock-materialization-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server =
        std::sync::Arc::new(MemoryServer::new(db_path, None).expect("create isolated test server"));
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "test-only-password".to_string(),
        },
    )
    .await
    .expect("vault init");
    handle_vault_set(
        &server,
        VaultSetParams {
            name: "OPENAI_API_KEY".to_string(),
            value: "fixture-provider-secret".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "R9 race fixture".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("vault set");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("fixture-provider-secret")
    );

    let (resolved_tx, resolved_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let refresh_server = std::sync::Arc::clone(&server);
    let refresh = std::thread::spawn(move || {
        crate::provider_config::materialize_for_server_with_hook_for_tests(
            &refresh_server,
            move || {
                resolved_tx.send(()).expect("announce resolved vault pools");
                release_rx.recv().expect("release stale resolved pools");
            },
        )
    });
    resolved_rx
        .recv()
        .expect("refresh reached post-resolution window");

    let refresh_holds_transaction = server.llm.provider_materialization_lock_is_held_for_tests();
    let (lock_started_tx, lock_started_rx) = std::sync::mpsc::channel();
    let (lock_done_tx, lock_done_rx) = std::sync::mpsc::channel();
    let lock_server = std::sync::Arc::clone(&server);
    let lock = std::thread::spawn(move || {
        lock_started_tx.send(()).expect("announce explicit lock");
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("lock runtime")
            .block_on(handle_vault_lock(&lock_server));
        lock_done_tx.send(()).expect("announce lock completion");
        result
    });
    lock_started_rx.recv().expect("explicit lock started");

    if refresh_holds_transaction {
        release_tx
            .send(())
            .expect("release transaction-held refresh before lock");
        refresh
            .join()
            .expect("refresh thread")
            .expect("refresh result");
        lock_done_rx.recv().expect("explicit lock completed last");
    } else {
        lock_done_rx
            .recv()
            .expect("pre-fix explicit lock completed before stale refresh");
        release_tx
            .send(())
            .expect("release stale pools after early lock");
        refresh
            .join()
            .expect("refresh thread")
            .expect("refresh result");
    }
    lock.join()
        .expect("lock thread")
        .expect("explicit lock result");

    assert!(server.vault_read().key.is_none());
    assert_eq!(
        server.llm.provider_secret_count(),
        0,
        "explicit lock must finish after any refresh that resolved Vault pools before lock"
    );
}

#[tokio::test]
async fn explicit_vault_lock_reports_provider_transaction_poison() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-lock-poison-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create isolated test server");
    let key = [7u8; 32];
    {
        let mut vault = server.vault_write();
        vault.key = Some(crate::CachedVaultKey::copy_from(&key));
        vault.unlock_time = Some(Instant::now());
    }
    assert!(server
        .llm
        .set_provider_secret("OPENAI_API_KEY", "fixture-cached-secret"));
    server.llm.poison_provider_materialization_lock_for_tests();

    let err = handle_vault_lock(&server)
        .await
        .expect_err("vault lock must not report success when provider clear is refused");

    assert!(err.contains("transaction lock is poisoned"), "{err}");
    assert!(
        server.vault_read().key.is_some(),
        "custody state must not partially lock before the shared transaction starts"
    );
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("fixture-cached-secret")
    );
}

// Gated with the same `#[cfg(target_os = "macos")]` as their only callers (the
// three Keychain auto-unlock tests below — see the block comment there and
// tachi#1466). Nothing in these two helpers is macOS-specific; without the gate
// they would simply be `dead_code` on Linux, and CI runs
// `cargo clippy --workspace --all-targets -- -D warnings` on ubuntu-24.04.
#[cfg(target_os = "macos")]
fn fixture_vault_access_count(server: &MemoryServer, name: &str) -> i64 {
    server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(name)
                .map_err(|e| e.to_string())
                .map(|entry| entry.expect("fixture Vault entry exists").access_count)
        })
        .expect("read fixture Vault access count")
}

#[cfg(target_os = "macos")]
fn reset_fixture_vault_access_count(server: &MemoryServer, name: &str) {
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE vault_entries SET access_count = 0, accessed_at = '' WHERE name = ?1",
                    [name],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("reset fixture Vault access count");
}

// tachi#1466 — the three tests below are macOS-only, by dependency, not by
// oversight. THIS BEHAVIOUR IS UNVERIFIED ON LINUX.
//
// They exercise Keychain *auto-unlock*, and that path does not exist off macOS:
// `provider_config::auto_unlock_vault_key_from_keychain` returns `Ok(false)` for
// `!cfg!(target_os = "macos")` BEFORE it consults the
// `TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK` / `TACHI_TEST_KEYCHAIN_PASSWORD`
// injection seam. (Note the seam itself is platform-neutral — the `use_keychain`
// unlock tests further down do run on Linux, because
// `vault_crypto::read_password_from_macos_keychain` checks its `#[cfg(test)]`
// override before its platform check. It is only the auto-unlock entry point
// that refuses early.) So on Linux the Vault is never auto-unlocked,
// `resolve_vault_pools` classifies the locked read as a benign miss, and
// materialization returns `Ok(MaterializeReport { loaded: 0, from_vault: 0, .. })`
// — the vault stays locked and the loud failure never arrives, which is how all
// three used to fail in a container with nothing in the output naming the
// platform.
//
// `#[cfg(target_os = "macos")]` rather than a runtime skip, deliberately:
//   * it matches the only other platform gate in this module
//     (`keychain_auto_unlock_smoke`, bottom of this file);
//   * the subject under test is itself macOS-only, so a macOS-only test is
//     exact rather than a workaround; and
//   * unlike `#[ignore]`, no invocation (`--run-ignored all`) can re-arm the
//     false red that #1466 exists to remove. A runtime early-`return` was
//     rejected outright: it would report a green on Linux for behaviour nothing
//     verified there.
//
// Known cost, accepted: the bodies below are not type-checked by a Linux-only
// build, so a refactor can rot them until the suite is next run on macOS.
// No assertion is weakened — a fake provider that fails loudly by construction
// would prove nothing. Real Linux coverage means giving the keychain provider a
// Linux-viable fallback (#1466 option 2), which is a separate slice.
#[cfg(target_os = "macos")]
#[test]
fn locked_provider_refresh_auto_unlocks_once_without_recursive_materialization() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _allow_auto_unlock = EnvRestore::set("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK", "1");
    let _keychain_password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "r10-password");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-keychain-refresh-r10-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server =
        std::sync::Arc::new(MemoryServer::new(db_path, None).expect("create isolated test server"));
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "r10-password".to_string(),
                },
            )
            .await
            .expect("vault init");
            handle_vault_set(
                &server,
                VaultSetParams {
                    name: "OPENAI_API_KEY".to_string(),
                    value: "r10-fixture-provider-secret".to_string(),
                    agent_id: None,
                    secret_type: "api_key".to_string(),
                    description: "R10 recursive auto-unlock fixture".to_string(),
                    allowed_agents: None,
                    enable_rotation: false,
                    rotation_strategy: None,
                    rebind: false,
                },
            )
            .await
            .expect("vault set");
            handle_vault_lock(&server).await.expect("vault lock");
        });
    reset_fixture_vault_access_count(&server, "OPENAI_API_KEY");

    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let refresh_server = std::sync::Arc::clone(&server);
    let refresh = std::thread::spawn(move || {
        let result = refresh_server.refresh_llm_provider_secrets_from_vault();
        result_tx.send(result).expect("send refresh result");
    });
    // This remains a finite deadlock assertion, but allows the production
    // Argon2 profile to contend with default-parallel Vault tests. A 2-second
    // wall-clock bound was a scheduler/KDF benchmark, not a lock invariant.
    let report = result_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("locked provider refresh must not deadlock during Keychain auto-unlock")
        .expect("locked provider refresh must succeed");
    refresh.join().expect("refresh thread");

    assert!(server.vault_read().key.is_some());
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("r10-fixture-provider-secret")
    );
    assert_eq!(
        report.from_vault, 1,
        "the outer transaction must materialize the one Vault-backed fixture key once"
    );
    assert_eq!(
        fixture_vault_access_count(&server, "OPENAI_API_KEY"),
        1,
        "one outer refresh must decrypt/touch each concrete key exactly once"
    );
}

// macOS-only: requires the Keychain auto-unlock path (tachi#1466 — see the
// block comment above `locked_provider_refresh_auto_unlocks_once_...`).
#[cfg(target_os = "macos")]
#[test]
fn failed_keychain_auto_unlock_keeps_vault_locked_and_refresh_fails_loudly() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _allow_auto_unlock = EnvRestore::set("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK", "1");
    let _keychain_password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "wrong-password");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-keychain-refresh-failure-r10-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create isolated test server");
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "correct-password".to_string(),
                },
            )
            .await
            .expect("vault init");
            handle_vault_set(
                &server,
                VaultSetParams {
                    name: "OPENAI_API_KEY".to_string(),
                    value: "r10-failed-unlock-fixture".to_string(),
                    agent_id: None,
                    secret_type: "api_key".to_string(),
                    description: "R10 failed auto-unlock fixture".to_string(),
                    allowed_agents: None,
                    enable_rotation: false,
                    rotation_strategy: None,
                    rebind: false,
                },
            )
            .await
            .expect("vault set");
            handle_vault_lock(&server).await.expect("vault lock");
        });
    reset_fixture_vault_access_count(&server, "OPENAI_API_KEY");

    let err = server
        .refresh_llm_provider_secrets_from_vault()
        .expect_err("wrong Keychain password must fail provider refresh loudly");

    assert!(
        err.contains("Failed to unlock Vault provider secrets"),
        "{err}"
    );
    assert!(err.contains("Wrong password"), "{err}");
    let vault = server.vault_read();
    assert!(vault.key.is_none());
    assert!(vault.unlock_time.is_none());
    drop(vault);
    assert_eq!(server.llm.provider_secret_count(), 0);
    assert_eq!(fixture_vault_access_count(&server, "OPENAI_API_KEY"), 0);
}

// macOS-only: requires the Keychain auto-unlock path (tachi#1466 — see the
// block comment above `locked_provider_refresh_auto_unlocks_once_...`).
#[cfg(target_os = "macos")]
#[test]
fn bootstrap_auto_unlock_owns_one_provider_refresh() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _allow_auto_unlock = EnvRestore::set("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK", "1");
    let _keychain_password =
        EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "r10-bootstrap-password");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-keychain-bootstrap-r10-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create isolated test server");
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "r10-bootstrap-password".to_string(),
                },
            )
            .await
            .expect("vault init");
            handle_vault_set(
                &server,
                VaultSetParams {
                    name: "OPENAI_API_KEY".to_string(),
                    value: "r10-bootstrap-provider-secret".to_string(),
                    agent_id: None,
                    secret_type: "api_key".to_string(),
                    description: "R10 bootstrap refresh owner fixture".to_string(),
                    allowed_agents: None,
                    enable_rotation: false,
                    rotation_strategy: None,
                    rebind: false,
                },
            )
            .await
            .expect("vault set");
            handle_vault_lock(&server).await.expect("vault lock");
        });
    reset_fixture_vault_access_count(&server, "OPENAI_API_KEY");

    crate::provider_config::bootstrap_provider_runtime(&server);

    assert!(server.vault_read().key.is_some());
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("r10-bootstrap-provider-secret")
    );
    assert_eq!(
        fixture_vault_access_count(&server, "OPENAI_API_KEY"),
        1,
        "bootstrap key installation must be followed by exactly one provider refresh"
    );
}

#[tokio::test]
async fn vault_unlock_rejects_password_and_fifo_path_together() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-unlock-fifo-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: Some("/tmp/tachi-unlock-test.fifo".to_string()),
            use_keychain: false,
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("exactly one of password, password_fifo_path, or use_keychain"),
        "expected mixed-transport rejection, got: {err}"
    );
}

// codex 3.3 (fix-round): the mutual-exclusion check (`sources_given > 1`)
// covers all three pairwise combinations of password/password_fifo_path/
// use_keychain, but only the password+fifo pair had a discriminating test.
// These two cover the remaining pairs. Both must be rejected BEFORE any
// actual Keychain/FIFO read is attempted (the `sources_given` check runs
// first in `handle_vault_unlock`), so neither test needs a working FIFO or
// a Keychain injection seam — a nonexistent FIFO path and no
// `TACHI_TEST_KEYCHAIN_PASSWORD` override are both fine, since the mutex
// check must short-circuit before either is touched.
#[tokio::test]
async fn vault_unlock_rejects_password_and_keychain_together() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-unlock-keychain-password-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
            use_keychain: true,
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("exactly one of password, password_fifo_path, or use_keychain"),
        "expected mixed-transport rejection, got: {err}"
    );
    let v = server.vault_read();
    assert!(
        v.key.is_none(),
        "vault must stay locked when the request is rejected as mixed-transport"
    );
}

#[tokio::test]
async fn vault_unlock_rejects_fifo_and_keychain_together() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-unlock-keychain-fifo-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: String::new(),
            password_fifo_path: Some("/tmp/tachi-unlock-test-fifo-keychain.fifo".to_string()),
            use_keychain: true,
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("exactly one of password, password_fifo_path, or use_keychain"),
        "expected mixed-transport rejection, got: {err}"
    );
    let v = server.vault_read();
    assert!(
        v.key.is_none(),
        "vault must stay locked when the request is rejected as mixed-transport"
    );
}

// tachi#1187 fix-round (attack-pass finding A1; leader adjudication,
// Option B): the pure-seam tests in `vault_crypto.rs`
// (`seam_rejects_algorithm_mismatch_as_typed_error_not_wrong_password`,
// `seam_algorithm_mismatch_does_not_feed_lockout_counter`) prove
// `derive_verified_key_from_stored_config` itself never routes
// algorithm-axis drift into a wrong-password outcome — but that guarantee
// was hollow as a production safety claim, because the LIVE
// `handle_vault_unlock` RPC handler did not call the seam at all: it
// re-implemented parse+derive+verify inline and never read
// `config.kdf_algorithm`. This test drives the real handler end-to-end (not
// a synthetic match) and asserts both that the error is the typed
// algorithm-mismatch message (never a generic wrong-password/lockout
// message) AND that `record_vault_unlock_failure`'s counter
// (`vault_read().failed_attempts.0`, the actual production lockout sink,
// `session.rs:156-168`) stays at zero.
//
// RED before the handler was wired to the seam: `handle_vault_unlock` never
// read `config.kdf_algorithm`, so it derived with Argon2id regardless of
// the stored label. This fixture keeps a legitimate Argon2id verifier (the
// real-world drift shape: a fork's `{m,t,p}` param shape matches, only the
// label differs), so `verify_password` returned `Ok(false)`, the handler
// matched that as a plain wrong-password miss, and `record_vault_unlock_failure`
// incremented the counter to 1 — feeding the brute-force lockout for a
// config-integrity problem, not a password problem.
#[tokio::test]
async fn vault_unlock_kdf_algorithm_mismatch_does_not_feed_lockout_counter() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-unlock-kdf-algorithm-mismatch-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    // Simulate algorithm-axis drift: rewrite the stored config's
    // `kdf_algorithm` to a value this build does not implement, keeping
    // salt/kdf_params/verifier exactly as `handle_vault_init` wrote them.
    let mut config = server
        .with_global_store(|store| store.vault_get_config().map_err(|e| e.to_string()))
        .expect("read vault config")
        .expect("vault config must exist after init");
    config.kdf_algorithm = "argon2i".to_string();
    server
        .with_global_store(|store| store.vault_set_config(&config).map_err(|e| e.to_string()))
        .expect("rewrite vault config with mismatched kdf_algorithm");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        },
    )
    .await
    .expect_err(
        "kdf_algorithm drift must fail the live unlock handler even with the correct password",
    );

    assert!(
        err.contains("kdf_algorithm") && err.contains("not a password error"),
        "expected the typed KdfAlgorithmMismatch message, got: {err}"
    );
    assert!(
        !err.contains("Wrong password") && !err.contains("Too many failed"),
        "algorithm-axis drift must never surface as a wrong-password/lockout message; got: {err}"
    );
    assert_eq!(
        server.vault_read().failed_attempts.0,
        0,
        "algorithm-axis drift must not increment the live vault unlock lockout counter"
    );
    assert!(
        server.vault_read().key.is_none(),
        "vault must stay locked when the stored kdf_algorithm is unsupported"
    );
}

// tachi#1175: `use_keychain` walks the same shared low-level primitive as
// the CLI's `tachi vault unlock --keychain`
// (`vault_crypto::read_password_from_macos_keychain`) so an agent never has
// to put a plaintext password in the tool call or shell. These two tests
// drive that primitive through its `TACHI_TEST_KEYCHAIN_PASSWORD` /
// `TACHI_TEST_FORCE_KEYCHAIN_MISSING` injection seam rather than the real
// macOS Keychain (a unit test must never depend on — or mutate — whatever
// `tachi-vault`/`default` Keychain entry happens to exist on the machine
// running the suite). `global_test_lock` serializes them against every other
// test that mutates process-global env vars (see
// `auto_lock_clears_key_but_preserves_provider_secrets` above for the same
// pattern).
// Plain `#[test]` + `block_on` (not `#[tokio::test]`), matching the
// `global_test_lock` convention used elsewhere in this crate (e.g.
// `dispatch_ops/prompt.rs`, `bootstrap::serve::stdio::tests`): the guard
// serializes the process-wide `TACHI_TEST_KEYCHAIN_PASSWORD` env var against
// other tests, so it must stay held across the whole init/lock/unlock
// sequence including its internal awaits — `block_on` runs that future to
// completion synchronously on this thread, so there is no `.await`
// expression in scope for clippy's `await_holding_lock` lint, while the
// guard's actual coverage is unchanged.
#[test]
fn vault_unlock_use_keychain_succeeds_via_injected_password() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _keychain_env = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "correct-password");

    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-unlock-keychain-ok-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let body = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "correct-password".to_string(),
                },
            )
            .await
            .expect("vault init should succeed");
            handle_vault_lock(&server)
                .await
                .expect("vault lock should succeed");

            handle_vault_unlock(
                &server,
                VaultUnlockParams {
                    password: String::new(),
                    password_fifo_path: None,
                    use_keychain: true,
                },
            )
            .await
            .expect("use_keychain unlock should succeed via the injected Keychain password")
        });

    let body_json: serde_json::Value =
        serde_json::from_str(&body).expect("vault_unlock response should be JSON");
    assert_eq!(body_json["unlocked"], serde_json::json!(true));
    let v = server.vault_read();
    assert!(
        v.key.is_some(),
        "vault key should be cached after a successful use_keychain unlock"
    );
}

// Same `#[test]` + `block_on` conversion as
// `vault_unlock_use_keychain_succeeds_via_injected_password` above — the
// guard here serializes `TACHI_TEST_FORCE_KEYCHAIN_MISSING`.
#[test]
fn vault_unlock_use_keychain_reports_missing_entry_without_leaking_process_error() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _keychain_env = EnvRestore::set("TACHI_TEST_FORCE_KEYCHAIN_MISSING", "1");

    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-unlock-keychain-missing-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let err = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "correct-password".to_string(),
                },
            )
            .await
            .expect("vault init should succeed");
            handle_vault_lock(&server)
                .await
                .expect("vault lock should succeed");

            handle_vault_unlock(
                &server,
                VaultUnlockParams {
                    password: String::new(),
                    password_fifo_path: None,
                    use_keychain: true,
                },
            )
            .await
            .expect_err("missing Keychain entry should fail, not silently succeed")
        });

    assert!(
        err.contains("use_keychain unlock failed"),
        "expected the use_keychain wrapper context, got: {err}"
    );
    assert!(
        err.contains("no vault password found in Keychain"),
        "expected the real missing-entry message, got: {err}"
    );
    let v = server.vault_read();
    assert!(
        v.key.is_none(),
        "vault must stay locked when the Keychain entry is missing"
    );
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_reads_secure_runtime_fifo() {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    // #1096 leaf-2a: `read_unlock_password_fifo` now takes `home` as an
    // explicit parameter instead of re-deriving it from `TACHI_HOME` env
    // internally (only env read the function body had), so this test no
    // longer needs to mutate process env or hold `global_test_lock` against
    // other env-dependent tests. NOT independently re-run here (edit-only
    // lane) — Oz must confirm this passes under parallel `cargo test`
    // before the lock removal is trusted.
    let temp = tempfile::tempdir().expect("temp tachi home");
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let fifo_path = unlock_dir.join("unlock-test.fifo");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

    let writer_path = fifo_path.clone();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open fifo writer");
        file.write_all(b"fifo-password").expect("write fifo");
    });

    let password =
        read_unlock_password_fifo(temp.path(), fifo_path.to_str().unwrap()).expect("read fifo");
    writer.join().expect("writer thread");
    assert_eq!(password, "fifo-password");
    assert!(!fifo_path.exists(), "daemon reader should remove FIFO");
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_rejects_invalid_utf8_without_echoing_payload() {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temp tachi home");
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let fifo_path = unlock_dir.join("invalid-utf8.fifo");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

    let writer_path = fifo_path.clone();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open fifo writer");
        file.write_all(&[0xff, 0xfe, b's', b'e', b'c', b'r', b'e', b't'])
            .expect("write fifo");
    });

    let err = read_unlock_password_fifo(temp.path(), fifo_path.to_str().unwrap())
        .expect_err("invalid UTF-8 must fail");
    writer.join().expect("writer thread");
    assert_eq!(err, "unlock FIFO password is not valid UTF-8");
    assert!(
        !err.contains("secret"),
        "error must not expose FIFO payload: {err}"
    );
    assert!(!fifo_path.exists(), "daemon reader should remove FIFO");
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_rejects_oversized_payload_and_removes_fifo() {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temp tachi home");
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let fifo_path = unlock_dir.join("oversized.fifo");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

    let writer_path = fifo_path.clone();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open fifo writer");
        file.write_all(&vec![b'x'; 4097]).expect("write fifo");
    });

    let err = read_unlock_password_fifo(temp.path(), fifo_path.to_str().unwrap())
        .expect_err("oversized FIFO password must fail");
    writer.join().expect("writer thread");
    assert_eq!(err, "unlock FIFO password exceeded maximum length");
    assert!(!fifo_path.exists(), "daemon reader should remove FIFO");
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_rejects_regular_file_without_removing_it() {
    use std::os::unix::fs::PermissionsExt;

    // #1096 leaf-2a: see comment in
    // `read_unlock_password_fifo_reads_secure_runtime_fifo` above — `home` is
    // now an explicit parameter, so no env mutation / global_test_lock needed.
    let temp = tempfile::tempdir().expect("temp tachi home");
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let regular_path = unlock_dir.join("not-a-fifo");
    std::fs::write(&regular_path, b"not a fifo").expect("regular file");

    let err = read_unlock_password_fifo(temp.path(), regular_path.to_str().unwrap())
        .expect_err("regular file");
    assert!(
        err.contains("must be a FIFO"),
        "expected FIFO rejection, got: {err}"
    );
    assert!(
        regular_path.exists(),
        "rejected non-FIFO input should not be removed"
    );
}

// G-B5 (updated for #400): auto-lock no longer clears provider secrets.
// The original re-materialization path (`re_materialize_provider_secrets_after_auto_lock`)
// was production dead code after the split — it had no callers outside tests —
// so both the function and the test that exercised it were removed (正本清源).
// The surviving discrimination test (a) `auto_lock_clears_key_but_preserves_provider_secrets`
// already proves auto-lock preserves provider secrets.

// Discrimination test (b) for #400: user-initiated `vault lock` must clear BOTH
// the vault master key AND provider secrets — the original full-clear semantics
// must be preserved when the split was introduced.
//
// This is a regression guard: it passes both before and after the fix by
// construction (the fix does not touch `handle_vault_lock`). Its purpose is to
// prove the split did not accidentally weaken the user-facing lock command.
#[test]
fn vault_lock_clears_key_and_provider_secrets() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-lock-full-clear-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        v.unlock_time = Some(Instant::now());
    }
    assert!(server.llm.set_provider_secret("OPENAI_API_KEY", "cached"));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create Tokio runtime")
        .block_on(handle_vault_lock(&server))
        .expect("vault lock should succeed");

    {
        let v = server.vault_read();
        assert!(v.key.is_none(), "vault lock must clear the master key");
        assert!(v.unlock_time.is_none(), "vault lock must clear unlock_time");
    }
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        None,
        "vault lock must clear provider secrets (full clear)"
    );
}

// Discrimination test (c) for #400: TACHI_VAULT_AUTOLOCK_SECS=0 disables
// auto-lock entirely — the key survives even past the normal timeout.
//
// RED before fix: there was no env knob; `auto_lock_after_secs` was hardcoded
// to 1800. The expired key would auto-lock regardless of any env value, and the
// assertion `key.is_some()` would fail.
#[tokio::test]
async fn autolock_disabled_when_env_zero() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::set("TACHI_VAULT_AUTOLOCK_SECS", "0");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-autolock-disabled-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");

    assert_eq!(
        server.vault_read().auto_lock_after_secs,
        0,
        "TACHI_VAULT_AUTOLOCK_SECS=0 must set auto_lock_after_secs to 0"
    );

    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        // Expired well beyond any normal timeout.
        v.unlock_time = Some(Instant::now() - Duration::from_secs(9999));
    }
    assert!(server.llm.set_provider_secret("OPENAI_API_KEY", "cached"));

    // With auto-lock disabled, the key is still valid even though unlock_time
    // is far in the past.
    let result = with_vault_key(&server, |k| {
        assert_eq!(
            k, &key,
            "key must still be accessible with auto-lock disabled"
        );
        Ok(())
    });
    result.expect("vault key should be available with auto-lock disabled");

    {
        let v = server.vault_read();
        assert!(
            v.key.is_some(),
            "key must survive past timeout when auto-lock is disabled"
        );
        assert!(
            v.unlock_time.is_some(),
            "unlock_time must not be cleared when auto-lock is disabled"
        );
    }
}

// Discrimination test (d) for #400: when auto-lock is disabled
// (`TACHI_VAULT_AUTOLOCK_SECS=0`), the runtime status surface must report
// `vault.unlocked: true` even if `unlock_time` is far in the past.
//
// RED before fix: `runtime_observability_json` computed `unlocked` with the
// bare predicate `elapsed <= auto_lock_after_secs`, which has no `0 = never
// expires` sentinel. With `auto_lock_after_secs == 0` and any `elapsed > 0`
// (here 9999s), `9999 <= 0` is `false`, so the status path reported
// `unlocked: false` one second after unlock — contradicting the enforcement
// path, which still held the key live and usable. A monitoring poll reading
// the runtime block would see `locked: true` while the daemon was happily
// serving with a live key.
#[tokio::test]
async fn autolock_disabled_reports_unlocked_in_runtime_status() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::set("TACHI_VAULT_AUTOLOCK_SECS", "0");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-autolock-disabled-status-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create test server");

    assert_eq!(
        server.vault_read().auto_lock_after_secs,
        0,
        "TACHI_VAULT_AUTOLOCK_SECS=0 must set auto_lock_after_secs to 0"
    );

    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        // Expired well beyond any normal timeout.
        v.unlock_time = Some(Instant::now() - Duration::from_secs(9999));
    }

    let runtime = crate::status_ops::runtime_observability_json(
        &server,
        &db_path,
        Some(&crate::status_ops::DaemonStatus::None),
        false,
    );
    let vault = &runtime["vault"];
    assert_eq!(
        vault["unlocked"],
        serde_json::json!(true),
        "with auto-lock disabled, runtime status must report unlocked: true even past the normal timeout (status path must match the enforcement path)"
    );
    assert_eq!(
        vault["auto_lock_after_seconds"],
        serde_json::json!(0),
        "runtime status should still surface the configured auto_lock_after_secs"
    );
}

// macOS Keychain auto-unlock integration smoke. Ignored by default because it
// touches the real developer Keychain; run explicitly with `--ignored`.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "touches real macOS Keychain; run explicitly with --ignored"]
async fn keychain_auto_unlock_smoke() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-keychain-smoke-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");

    // The keychain-availability probe and the auto-unlock path must run without
    // panicking regardless of whether a real `tachi-vault/default` entry exists.
    let available = crate::provider_config::keychain_vault_password_entry_available()
        .expect("keychain availability probe should return a Result, not panic");
    let unlocked = crate::provider_config::auto_unlock_vault_from_keychain(&server)
        .expect("auto-unlock attempt should return a Result, not panic");
    eprintln!("keychain_available={available} auto_unlocked={unlocked}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_set_infers_config_for_lane_urls_and_refuses_api_key() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-lane-config-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "lane-config-classifier".to_string(),
        },
    )
    .await
    .expect("vault init");

    let set = handle_vault_set(
        &server,
        VaultSetParams {
            name: "EXTRACT_BASE_URL".to_string(),
            value: "https://api.deepseek.com/chat/completions".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "lane config".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("inferred config write");
    assert!(set.contains("config"), "{set}");

    let refused = handle_vault_set(
        &server,
        VaultSetParams {
            name: "DISTILL_MODEL".to_string(),
            value: "deepseek-v4-flash".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "must refuse".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("explicit api_key on lane config must be refused");
    assert!(
        refused.contains("lane config") && refused.contains("api_key"),
        "{refused}"
    );

    handle_vault_set(
        &server,
        VaultSetParams {
            name: "DEEPSEEK_API_KEY".to_string(),
            value: "sk-test-not-a-real-key".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "real key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("key write");

    let listed = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list");
    let body: serde_json::Value = serde_json::from_str(&listed).expect("list json");
    assert_eq!(body["config"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["config"][0]["name"], "EXTRACT_BASE_URL");
    assert_eq!(body["config"][0]["secret_type"], "config");
    assert_eq!(body["config"][0]["group"], "config");
    let creds = body["credentials"].as_array().expect("credentials");
    assert!(
        creds.iter().any(|row| row["name"] == "DEEPSEEK_API_KEY"),
        "{body}"
    );

    let leak = handle_vault_set(
        &server,
        VaultSetParams {
            name: "REASONING_BASE_URL".to_string(),
            value: "https://user:pass@api.deepseek.com/chat/completions".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "leaky url".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("userinfo URL must be refused");
    assert!(
        leak.contains("userinfo") || leak.contains("credential"),
        "{leak}"
    );

    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    let pool_names: Vec<&str> = pools.keys().map(String::as_str).collect();
    assert!(
        !pools.contains_key("EXTRACT_BASE_URL"),
        "config rows must not enter API-key pools: {pool_names:?}"
    );
    assert!(pools.contains_key("DEEPSEEK_API_KEY"), "{pool_names:?}");

    handle_vault_set(
        &server,
        VaultSetParams {
            name: "SUMMARY_MODEL".to_string(),
            value: "legacy-other".to_string(),
            agent_id: None,
            secret_type: "other".to_string(),
            description: "leftover other".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("legacy other write");
    let config_only = handle_vault_list(
        &server,
        VaultListParams {
            secret_type: Some("config".to_string()),
        },
    )
    .await
    .expect("filter config");
    let config_body: serde_json::Value = serde_json::from_str(&config_only).expect("json");
    let config_names: Vec<&str> = config_body["config"]
        .as_array()
        .expect("config")
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(
        config_names.contains(&"SUMMARY_MODEL"),
        "legacy other lane-config names must list as config: {config_body}"
    );

    let enable = handle_vault_set(
        &server,
        VaultSetParams {
            name: "ENABLE_FALLBACK_API_KEY".to_string(),
            value: "1".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "flag that also looks like a key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("ENABLE_* infers config even with _API_KEY suffix");
    assert!(enable.contains("config"), "{enable}");

    let leak_member = handle_vault_set(
        &server,
        VaultSetParams {
            name: "EXTRACT_BASE_URL_1".to_string(),
            value: "https://user:pass@api.deepseek.com/chat/completions".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "rotated url".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("rotation member URL must still run the leak gate");
    assert!(
        leak_member.contains("userinfo") || leak_member.contains("credential"),
        "{leak_member}"
    );

    let rotation = handle_vault_set(
        &server,
        VaultSetParams {
            name: "EXTRACT_BASE_URL_1".to_string(),
            value: "https://api.deepseek.com/chat/completions".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "no rotation".to_string(),
            allowed_agents: None,
            enable_rotation: true,
            rotation_strategy: Some("round_robin".to_string()),
            rebind: false,
        },
    )
    .await
    .expect_err("config members must not attach API-key rotation");
    assert!(
        rotation.contains("config") && rotation.contains("rotation"),
        "{rotation}"
    );

    let custom_rotation = handle_vault_set(
        &server,
        VaultSetParams {
            name: "CUSTOM_ENDPOINT_9".to_string(),
            value: "https://custom.example.test/v1".to_string(),
            agent_id: None,
            secret_type: "config".to_string(),
            description: "explicit config member".to_string(),
            allowed_agents: None,
            enable_rotation: true,
            rotation_strategy: Some("round_robin".to_string()),
            rebind: false,
        },
    )
    .await
    .expect_err("explicit config members must not create rotation state through vault_set");
    assert!(
        custom_rotation.contains("config") && custom_rotation.contains("refusing"),
        "{custom_rotation}"
    );
    let custom_entry = server
        .with_global_store_read(|store| {
            store
                .vault_get_entry("CUSTOM_ENDPOINT_9")
                .map_err(|error| error.to_string())
        })
        .expect("read refused custom member");
    assert!(
        custom_entry.is_none(),
        "refused config rotation member must not persist its entry"
    );

    for idx in 1..=2 {
        handle_vault_set(
            &server,
            VaultSetParams {
                name: format!("CUSTOM_POOL_{idx}"),
                value: format!("config-value-{idx}"),
                agent_id: None,
                secret_type: "config".to_string(),
                description: "existing explicit config member".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            },
        )
        .await
        .expect("seed explicit config member through vault_set");
    }
    let mixed_rotation = handle_vault_set(
        &server,
        VaultSetParams {
            name: "CUSTOM_POOL_3".to_string(),
            value: "api-key-value-3".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "attempt mixed rotation".to_string(),
            allowed_agents: None,
            enable_rotation: true,
            rotation_strategy: Some("round_robin".to_string()),
            rebind: false,
        },
    )
    .await
    .expect_err("existing config members must block a mixed rotation");
    assert!(
        mixed_rotation.contains("CUSTOM_POOL_1")
            && mixed_rotation.contains("config")
            && mixed_rotation.contains("refusing rotation"),
        "{mixed_rotation}"
    );
    let (mixed_entry, mixed_state) = server
        .with_global_store_read(|store| {
            let entry = store
                .vault_get_entry("CUSTOM_POOL_3")
                .map_err(|error| error.to_string())?;
            let rotation = store
                .vault_get_rotation("CUSTOM_POOL")
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((entry, rotation))
        })
        .expect("read mixed-rotation rollback state");
    assert!(
        mixed_entry.is_none() && mixed_state.is_none(),
        "mixed rotation refusal must roll back the new member and rotation state"
    );

    handle_vault_set_api_key_pool(
        &server,
        VaultSetApiKeyPoolParams {
            prefix: "MUTABLE_POOL_API_KEY".to_string(),
            values: vec!["key-one".to_string(), "key-two".to_string()],
            agent_id: None,
            strategy: "round_robin".to_string(),
            description: "valid pool".to_string(),
            allowed_agents: None,
        },
    )
    .await
    .expect("create valid rotation before mutation attempt");
    let remove_member = handle_vault_remove(
        &server,
        VaultRemoveParams {
            name: "MUTABLE_POOL_API_KEY_2".to_string(),
            agent_id: None,
        },
    )
    .await
    .expect_err("configured rotation members must not be deleted");
    assert!(
        remove_member.contains("rotation member") && remove_member.contains("refusing deletion"),
        "{remove_member}"
    );
    let downgrade = handle_vault_set(
        &server,
        VaultSetParams {
            name: "MUTABLE_POOL_API_KEY_1".to_string(),
            value: "config-downgrade".to_string(),
            agent_id: None,
            secret_type: "config".to_string(),
            description: "attempt member downgrade".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("existing rotation member cannot be downgraded without enable_rotation");
    assert!(
        downgrade.contains("MUTABLE_POOL_API_KEY_1") && downgrade.contains("config"),
        "{downgrade}"
    );
    let retained = server
        .with_global_store_read(|store| {
            store
                .vault_get_entry("MUTABLE_POOL_API_KEY_1")
                .map_err(|error| error.to_string())
        })
        .expect("read retained rotation member")
        .expect("member remains after rollback");
    assert_eq!(retained.secret_type, "api_key");

    let append = handle_vault_set(
        &server,
        VaultSetParams {
            name: "MUTABLE_POOL_API_KEY_3".to_string(),
            value: "key-three".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "attempt unconfigured append".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("member append must not leave total_keys stale");
    assert!(
        append.contains("declares 2 keys") && append.contains("has 3"),
        "{append}"
    );
    assert!(server
        .with_global_store_read(|store| store
            .vault_get_entry("MUTABLE_POOL_API_KEY_3")
            .map_err(|error| error.to_string()))
        .expect("read refused append")
        .is_none());

    let zero_member = handle_vault_set(
        &server,
        VaultSetParams {
            name: "MUTABLE_POOL_API_KEY_0".to_string(),
            value: "key-zero".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "attempt zero-index member".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("zero-index member must not bypass configured rotation validation");
    assert!(
        zero_member.contains("non-contiguous member index 0") && zero_member.contains("expected 1"),
        "{zero_member}"
    );

    plant_vault_secret(
        &server,
        "MUTABLE_POOL_API_KEY_3",
        "legacy-key-three",
        "api_key",
    );
    let poisoned_lease = handle_vault_lease_api_key(
        &server,
        VaultLeaseApiKeyParams {
            name: "MUTABLE_POOL_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        },
    )
    .await
    .expect_err("MCP lease must reject a stale persisted rotation count");
    assert!(
        poisoned_lease.contains("declares 2 keys") && poisoned_lease.contains("has 3"),
        "{poisoned_lease}"
    );

    plant_vault_secret(&server, "EXTRA_POOL_API_KEY_1", "key-one", "api_key");
    plant_vault_secret(&server, "EXTRA_POOL_API_KEY_2", "key-two", "api_key");
    plant_vault_secret(&server, "EXTRA_POOL_API_KEY_3", "config-extra", "config");
    let extra_setup = handle_vault_setup_rotation(
        &server,
        VaultSetupRotationParams {
            prefix: "EXTRA_POOL_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
            agent_id: None,
        },
    )
    .await
    .expect_err("setup must inspect numeric members beyond the declared count");
    assert!(
        extra_setup.contains("EXTRA_POOL_API_KEY_3") && extra_setup.contains("config"),
        "{extra_setup}"
    );
    let extra_set_pool = handle_vault_set_api_key_pool(
        &server,
        VaultSetApiKeyPoolParams {
            prefix: "EXTRA_POOL_API_KEY".to_string(),
            values: vec!["replacement-one".to_string(), "replacement-two".to_string()],
            agent_id: None,
            strategy: "round_robin".to_string(),
            description: "must refuse extra config".to_string(),
            allowed_agents: None,
        },
    )
    .await
    .expect_err("set-pool must refuse every structural config member");
    assert!(
        extra_set_pool.contains("EXTRA_POOL_API_KEY_3") && extra_set_pool.contains("config"),
        "{extra_set_pool}"
    );

    let pool = handle_vault_set_api_key_pool(
        &server,
        VaultSetApiKeyPoolParams {
            prefix: "EXTRACT_BASE_URL".to_string(),
            values: vec!["https://api.deepseek.com/chat/completions".to_string()],
            agent_id: None,
            strategy: "round_robin".to_string(),
            description: String::new(),
            allowed_agents: None,
        },
    )
    .await
    .expect_err("API-key pool writer must refuse config prefixes");
    assert!(
        pool.contains("lane config") && pool.contains("api_key"),
        "{pool}"
    );

    let nested_leak = handle_vault_set(
        &server,
        VaultSetParams {
            name: "EXTRACT_BASE_URL_1_1".to_string(),
            value: "https://user:pass@api.deepseek.com/chat/completions".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "double suffix".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect_err("nested rotation suffix must still be a config URL");
    assert!(
        nested_leak.contains("userinfo") || nested_leak.contains("credential"),
        "{nested_leak}"
    );

    let leased = handle_vault_lease_api_key(
        &server,
        VaultLeaseApiKeyParams {
            name: "EXTRACT_BASE_URL".to_string(),
            env_name: None,
            agent_id: None,
        },
    )
    .await
    .expect_err("config URLs must not lease as API keys");
    assert!(leased.contains("lane config"), "{leased}");
}

#[tokio::test]
async fn rotated_get_rejects_legacy_mixed_members_and_access_advance_preserves_new_count() {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-rotation-final-state-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "rotation-final-state".to_string(),
        },
    )
    .await
    .expect("vault init");

    plant_vault_secret(&server, "LEGACY_MIXED_1", "key-one", "api_key");
    plant_vault_secret(&server, "LEGACY_MIXED_2", "not-a-key", "config");
    server
        .with_global_store(|store| {
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "LEGACY_MIXED".to_string(),
                    current_index: 2,
                    total_keys: 2,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-01T00:00:00Z".to_string(),
                })
                .map_err(|error| error.to_string())
        })
        .expect("seed legacy mixed rotation");
    let mixed_get = handle_vault_get(
        &server,
        VaultGetParams {
            name: "LEGACY_MIXED".to_string(),
            agent_id: None,
            auto_rotate: true,
        },
    )
    .await
    .expect_err("rotated get must reject a legacy config member");
    assert!(
        mixed_get.contains("LEGACY_MIXED_2") && mixed_get.contains("config"),
        "{mixed_get}"
    );
    let explicit_mixed_get = handle_vault_get(
        &server,
        VaultGetParams {
            name: "LEGACY_MIXED_2".to_string(),
            agent_id: None,
            auto_rotate: false,
        },
    )
    .await
    .expect_err("explicit configured member get must validate the canonical rotation");
    assert!(
        explicit_mixed_get.contains("LEGACY_MIXED_2") && explicit_mixed_get.contains("config"),
        "{explicit_mixed_get}"
    );

    handle_vault_set_api_key_pool(
        &server,
        VaultSetApiKeyPoolParams {
            prefix: "RACE_POOL_API_KEY".to_string(),
            values: vec!["one".to_string(), "two".to_string()],
            agent_id: None,
            strategy: "round_robin".to_string(),
            description: String::new(),
            allowed_agents: None,
        },
    )
    .await
    .expect("create two-member pool");
    let stale_rotation = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("RACE_POOL_API_KEY")
                .map_err(|error| error.to_string())
        })
        .expect("read stale rotation")
        .expect("rotation exists");
    handle_vault_set_api_key_pool(
        &server,
        VaultSetApiKeyPoolParams {
            prefix: "RACE_POOL_API_KEY".to_string(),
            values: vec![
                "one-new".to_string(),
                "two-new".to_string(),
                "three".to_string(),
            ],
            agent_id: None,
            strategy: "round_robin".to_string(),
            description: String::new(),
            allowed_agents: None,
        },
    )
    .await
    .expect("expand pool to three members");
    server
        .with_global_store(|store| {
            record_successful_vault_access(store, "RACE_POOL_API_KEY_2", Some(&stale_rotation))
        })
        .expect("stale access completion must re-read current rotation");
    let final_rotation = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("RACE_POOL_API_KEY")
                .map_err(|error| error.to_string())
        })
        .expect("read final rotation")
        .expect("rotation remains");
    assert_eq!(final_rotation.total_keys, 3);
    assert_eq!(final_rotation.current_index, 3);
}

fn plant_vault_secret(server: &MemoryServer, name: &str, value: &str, secret_type: &str) {
    with_vault_key(server, |key| {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key, value.as_bytes()).map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        server
            .with_global_store(|store| {
                store
                    .vault_upsert_entry(&memcore::vault::VaultEntry {
                        name: name.to_string(),
                        encrypted_value,
                        nonce,
                        secret_type: secret_type.to_string(),
                        description: "leftover plant".to_string(),
                        allowed_agents: None,
                        created_at: now.clone(),
                        updated_at: now,
                        accessed_at: String::new(),
                        access_count: 0,
                    })
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("plant {name}: {e}"))
    })
    .unwrap_or_else(|e| panic!("plant {name}: {e}"));
}

/// Pre-#1857 omitted types defaulted to `api_key`. Leftover
/// `EXTRACT_BASE_URL` rows must list as config. Explicit `config` on a
/// non-lane-config name (`CUSTOM_ENDPOINT`) must still refuse to lease.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn leftover_api_key_lane_config_lists_as_config_and_config_rows_do_not_lease() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-leftover-api-key-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "leftover-api-key".to_string(),
        },
    )
    .await
    .expect("vault init");

    plant_vault_secret(
        &server,
        "EXTRACT_BASE_URL",
        "https://api.deepseek.com/chat/completions",
        "api_key",
    );
    plant_vault_secret(
        &server,
        "CUSTOM_ENDPOINT",
        "https://example.test/v1",
        "config",
    );
    plant_vault_secret(
        &server,
        "CUSTOM_ENDPOINT_1",
        "https://one.example.test/v1",
        "config",
    );
    plant_vault_secret(
        &server,
        "CUSTOM_ENDPOINT_2",
        "https://two.example.test/v1",
        "config",
    );

    let setup = handle_vault_setup_rotation(
        &server,
        VaultSetupRotationParams {
            prefix: "CUSTOM_ENDPOINT".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
            agent_id: None,
        },
    )
    .await
    .expect_err("explicit config members must not form an API-key rotation");
    assert!(
        setup.contains("CUSTOM_ENDPOINT_1")
            && setup.contains("config")
            && setup.contains("refusing rotation"),
        "{setup}"
    );
    let stored_rotation = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("CUSTOM_ENDPOINT")
                .map_err(|error| error.to_string())
        })
        .expect("read rotation state");
    assert!(
        stored_rotation.is_none(),
        "refused config rotation must persist no rotation state"
    );

    let listed = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list");
    let body: serde_json::Value = serde_json::from_str(&listed).expect("list json");
    let config_names: Vec<&str> = body["config"]
        .as_array()
        .expect("config")
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    let cred_names: Vec<&str> = body["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(
        config_names.contains(&"EXTRACT_BASE_URL"),
        "leftover api_key EXTRACT_BASE_URL must list as config: {body}"
    );
    assert!(
        config_names.contains(&"CUSTOM_ENDPOINT"),
        "explicit config CUSTOM_ENDPOINT must list as config: {body}"
    );
    assert!(
        !cred_names.contains(&"EXTRACT_BASE_URL"),
        "leftover EXTRACT_BASE_URL must not stay in credentials: {body}"
    );
    assert!(
        !cred_names.contains(&"CUSTOM_ENDPOINT"),
        "CUSTOM_ENDPOINT config must not list as a credential: {body}"
    );

    let leftover = body["config"]
        .as_array()
        .expect("config")
        .iter()
        .find(|row| row["name"] == "EXTRACT_BASE_URL")
        .expect("leftover row");
    assert_eq!(leftover["secret_type"], "config");
    assert_eq!(leftover["group"], "config");

    let keys_only = handle_vault_list(
        &server,
        VaultListParams {
            secret_type: Some("api_key".to_string()),
        },
    )
    .await
    .expect("filter api_key");
    let keys_body: serde_json::Value = serde_json::from_str(&keys_only).expect("json");
    let key_names: Vec<&str> = keys_body["secrets"]
        .as_array()
        .expect("secrets")
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(
        !key_names.contains(&"EXTRACT_BASE_URL"),
        "secret_type=api_key filter must hide leftover lane config: {keys_body}"
    );

    let leased_url = handle_vault_lease_api_key(
        &server,
        VaultLeaseApiKeyParams {
            name: "EXTRACT_BASE_URL".to_string(),
            env_name: None,
            agent_id: None,
        },
    )
    .await
    .expect_err("leftover lane-config url must not lease");
    assert!(
        leased_url.contains("lane config") || leased_url.contains("not a credential"),
        "{leased_url}"
    );

    let leased_custom = handle_vault_lease_api_key(
        &server,
        VaultLeaseApiKeyParams {
            name: "CUSTOM_ENDPOINT".to_string(),
            env_name: None,
            agent_id: None,
        },
    )
    .await
    .expect_err("explicit config rows must not lease even when the name is not lane-config-shaped");
    assert!(
        leased_custom.contains("config") && leased_custom.contains("not a credential"),
        "{leased_custom}"
    );

    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("EXTRACT_BASE_URL"),
        "leftover api_key EXTRACT_BASE_URL must not enter API-key pools: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
    assert!(
        !pools.contains_key("CUSTOM_ENDPOINT"),
        "config CUSTOM_ENDPOINT must not enter API-key pools: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
}

fn vault_set_params(name: &str, value: &str, rebind: bool) -> VaultSetParams {
    VaultSetParams {
        name: name.to_string(),
        value: value.to_string(),
        agent_id: None,
        secret_type: String::new(),
        description: String::new(),
        allowed_agents: None,
        enable_rotation: false,
        rotation_strategy: None,
        rebind,
    }
}

/// Slot set stores `vault:ACCOUNT`, never a second copy of the account secret.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_set_binds_account_instead_of_copying_ciphertext() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-account-bind-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "account-bind".to_string(),
        },
    )
    .await
    .expect("vault init");

    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("account write");

    let bound = handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("slot bind");
    let bound_json: serde_json::Value = serde_json::from_str(&bound).expect("json");
    assert_eq!(bound_json["bound_account"], "DEEPSEEK_API_KEY");
    assert_ne!(bound_json["fingerprint"], "");

    let got = handle_vault_get(
        &server,
        VaultGetParams {
            name: "EXTRACT_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        },
    )
    .await
    .expect("get slot");
    let got_json: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(got_json["value"], "vault:DEEPSEEK_API_KEY");
    assert_ne!(got_json["value"], "deepseek-secret-bytes");

    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    let extract = pools
        .get("EXTRACT_API_KEY")
        .expect("slot pool must resolve through the account");
    assert_eq!(extract[0].value, "deepseek-secret-bytes");
    assert_eq!(extract[0].key_id, "DEEPSEEK_API_KEY");

    let orphan = handle_vault_set(
        &server,
        vault_set_params("DISTILL_API_KEY", "glm-orphan-secret", true),
    )
    .await
    .expect_err("unmatched bytes must not copy into a slot even with rebind");
    assert!(
        orphan.contains("second copy") || orphan.contains("provider account"),
        "{orphan}"
    );
    assert!(!orphan.contains("glm-orphan-secret"), "{orphan}");

    handle_vault_set(
        &server,
        vault_set_params("SILICONFLOW_API_KEY", "siliconflow-secret-bytes", false),
    )
    .await
    .expect("second account");
    let refused = handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "siliconflow-secret-bytes", false),
    )
    .await
    .expect_err("family change needs rebind");
    assert!(
        refused.contains("--rebind") || refused.contains("rebind=true"),
        "{refused}"
    );

    let rebound = handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "siliconflow-secret-bytes", true),
    )
    .await
    .expect("rebind pointer");
    let rebound_json: serde_json::Value = serde_json::from_str(&rebound).expect("json");
    assert_eq!(rebound_json["bound_account"], "SILICONFLOW_API_KEY");
    assert_eq!(rebound_json["rebind"], true);

    let got_rebind = handle_vault_get(
        &server,
        VaultGetParams {
            name: "EXTRACT_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        },
    )
    .await
    .expect("get rebound slot");
    let rebind_json: serde_json::Value = serde_json::from_str(&got_rebind).expect("json");
    assert_eq!(rebind_json["value"], "vault:SILICONFLOW_API_KEY");

    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-rotated-bytes", false),
    )
    .await
    .expect("account rotation does not need rebind");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_same_pointer_write_persists_allowed_agents() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-metadata-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-metadata".to_string(),
        },
    )
    .await
    .expect("vault init");
    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("account write");
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("first bind");

    let stored = handle_vault_set(
        &server,
        VaultSetParams {
            name: "EXTRACT_API_KEY".to_string(),
            value: "vault:DEEPSEEK_API_KEY".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: "extract lane".to_string(),
            allowed_agents: Some(vec!["lane-bot".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("metadata write");
    let stored_json: serde_json::Value = serde_json::from_str(&stored).expect("json");
    assert_eq!(stored_json["noop"], true);
    assert_eq!(stored_json["stored"], true);

    let got = handle_vault_get(
        &server,
        VaultGetParams {
            name: "EXTRACT_API_KEY".to_string(),
            agent_id: Some("lane-bot".to_string()),
            auto_rotate: false,
        },
    )
    .await
    .expect("get slot");
    let got_json: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(got_json["value"], "vault:DEEPSEEK_API_KEY");
    assert_eq!(got_json["description"], "extract lane");
    assert_eq!(got_json["allowed_agents"][0], "lane-bot");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_pool_skips_restricted_or_unhealthy_target() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-target-policy-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-target-policy".to_string(),
        },
    )
    .await
    .expect("vault init");
    handle_vault_set(
        &server,
        VaultSetParams {
            name: "DEEPSEEK_API_KEY".to_string(),
            value: "deepseek-secret-bytes".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: String::new(),
            allowed_agents: Some(vec!["owner-bot".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("restricted account");
    let mut bind = vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", false);
    bind.agent_id = Some("owner-bot".to_string());
    handle_vault_set(&server, bind)
        .await
        .expect("unrestricted slot bind");

    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("EXTRACT_API_KEY"),
        "unrestricted slot must not leak a restricted account: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
    let leased = handle_vault_lease_api_key(
        &server,
        VaultLeaseApiKeyParams {
            name: "EXTRACT_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        },
    )
    .await
    .expect_err("restricted target must not lease through the slot");
    assert!(
        leased.contains("No usable API key") || leased.contains("not"),
        "{leased}"
    );

    handle_vault_set(
        &server,
        VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "siliconflow-secret-bytes".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: String::new(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("healthy account");
    handle_vault_set(
        &server,
        VaultSetParams {
            name: "SUMMARY_API_KEY".to_string(),
            value: "siliconflow-secret-bytes".to_string(),
            agent_id: None,
            secret_type: String::new(),
            description: String::new(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .expect("summary bind");
    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
                    logical_name: "SUMMARY_API_KEY".to_string(),
                    key_id: "SILICONFLOW_API_KEY".to_string(),
                    status: "disabled".to_string(),
                    disabled: true,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("persist slot-target health");
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("SUMMARY_API_KEY"),
        "disabled (slot, target) health must skip materialization: {:?}",
        pools.keys().collect::<Vec<_>>()
    );

    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
                    logical_name: "SUMMARY_API_KEY".to_string(),
                    key_id: "SILICONFLOW_API_KEY".to_string(),
                    status: "ok".to_string(),
                    disabled: false,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())?;
            let mut entry = store
                .vault_get_entry("SILICONFLOW_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("account");
            entry.secret_type = memcore::SECRET_TYPE_CONFIG.to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("healthy but non-api_key target");
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("SUMMARY_API_KEY"),
        "non-api_key target must not materialize through the slot: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn leftover_slot_ciphertext_is_not_materialized() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-leftover-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-leftover".to_string(),
        },
    )
    .await
    .expect("vault init");
    with_vault_key(&server, |key| {
        let (encrypted_value, nonce) = crate::vault_crypto::encrypt(key, b"leftover-slot-bytes")?;
        let now = chrono::Utc::now().to_rfc3339();
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: "EXTRACT_API_KEY".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                    description: String::new(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now,
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed leftover ciphertext");
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("EXTRACT_API_KEY"),
        "leftover slot ciphertext must not enter the API-key pool: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_cannot_be_a_rotation_pool() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-pool-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-pool".to_string(),
        },
    )
    .await
    .expect("vault init");
    let pool_err = handle_vault_set_api_key_pool(
        &server,
        VaultSetApiKeyPoolParams {
            prefix: "EXTRACT_API_KEY".to_string(),
            values: vec!["k1".to_string(), "k2".to_string()],
            agent_id: None,
            strategy: "round_robin".to_string(),
            description: String::new(),
            allowed_agents: None,
        },
    )
    .await
    .expect_err("slot prefix must not become a pool");
    assert!(
        pool_err.contains("Lane slot") || pool_err.contains("rotation pool"),
        "{pool_err}"
    );
    let rotation_err = handle_vault_setup_rotation(
        &server,
        VaultSetupRotationParams {
            prefix: "DISTILL_API_KEY".to_string(),
            total_keys: 2,
            agent_id: None,
            strategy: "round_robin".to_string(),
        },
    )
    .await
    .expect_err("slot prefix must not attach rotation");
    assert!(
        rotation_err.contains("Lane slot") || rotation_err.contains("rotation pool"),
        "{rotation_err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn child_env_all_mode_does_not_inject_leftover_slot_bytes() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _child_env = EnvRestore::set("TACHI_VAULT_CHILD_ENV", "all");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-child-env-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-child-env".to_string(),
        },
    )
    .await
    .expect("vault init");
    with_vault_key(&server, |key| {
        let (encrypted_value, nonce) = crate::vault_crypto::encrypt(key, b"leftover-slot-bytes")?;
        let now = chrono::Utc::now().to_rfc3339();
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: "EXTRACT_API_KEY".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                    description: String::new(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now,
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed leftover ciphertext");
    let secrets = crate::vault_ops::load_unlocked_env_secrets_for_child_env(&server, None)
        .expect("child env");
    assert!(
        secrets
            .iter()
            .all(|(name, value)| name != "EXTRACT_API_KEY" && value != "leftover-slot-bytes"),
        "{secrets:?}"
    );

    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("account");
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", true),
    )
    .await
    .expect("bind leftover to account");
    let secrets = crate::vault_ops::load_unlocked_env_secrets_for_child_env(&server, None)
        .expect("child env");
    let extract = secrets
        .iter()
        .find(|(name, _)| name == "EXTRACT_API_KEY")
        .expect("bound slot must enter child env through the resolved pool");
    assert_eq!(extract.1, "deepseek-secret-bytes");
    assert_ne!(extract.1, "vault:DEEPSEEK_API_KEY");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_binding_resolves_lane_slot_pointer() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-project-bind-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-project-bind".to_string(),
        },
    )
    .await
    .expect("vault init");
    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("account");
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("bind");
    let tachi_dir = dir.path().join(".tachi");
    std::fs::create_dir_all(&tachi_dir).expect("mkdir");
    std::fs::write(
        tachi_dir.join("vault.env"),
        "MY_KEY=vault:EXTRACT_API_KEY\n",
    )
    .expect("write bindings");
    let secrets =
        crate::vault_ops::load_unlocked_env_secrets_for_child_env(&server, Some(dir.path()))
            .expect("child env");
    let injected = secrets
        .iter()
        .find(|(name, _)| name == "MY_KEY")
        .expect("project binding");
    assert_eq!(injected.1, "deepseek-secret-bytes");
    assert_ne!(injected.1, "vault:DEEPSEEK_API_KEY");
    assert_ne!(injected.1, "vault:EXTRACT_API_KEY");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn read_unlocked_vault_secret_resolves_lane_slot_pointer() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-usable-read-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-usable-read".to_string(),
        },
    )
    .await
    .expect("vault init");
    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("account");
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("bind");
    let usable =
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
            .expect("usable slot");
    assert_eq!(usable, "deepseek-secret-bytes");
    let got = handle_vault_get(
        &server,
        VaultGetParams {
            name: "EXTRACT_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        },
    )
    .await
    .expect("inventory get");
    let got_json: serde_json::Value = serde_json::from_str(&got).expect("json");
    assert_eq!(got_json["value"], "vault:DEEPSEEK_API_KEY");

    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
                    logical_name: "DEEPSEEK_API_KEY".to_string(),
                    key_id: "DEEPSEEK_API_KEY".to_string(),
                    status: "disabled".to_string(),
                    disabled: true,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("disable target");
    let err = crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
        .expect_err("disabled target must not be a usable slot secret");
    assert!(
        err.contains("unusable") || err.contains("Lane slot"),
        "{err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_read_ignores_legacy_rotation_members() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-legacy-rotation-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-legacy-rotation".to_string(),
        },
    )
    .await
    .expect("vault init");
    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("account");
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "deepseek-secret-bytes", false),
    )
    .await
    .expect("bind");
    with_vault_key(&server, |key| {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key, b"leftover-rotation-member")?;
        let now = chrono::Utc::now().to_rfc3339();
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: "EXTRACT_API_KEY_1".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                    description: String::new(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .map_err(|e| e.to_string())?;
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "EXTRACT_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 1,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed leftover slot rotation");
    let usable =
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, true)
            .expect("slot must ignore leftover rotation");
    assert_eq!(usable, "deepseek-secret-bytes");
    assert_ne!(usable, "leftover-rotation-member");
}
