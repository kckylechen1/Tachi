use super::*;

fn reset_access_count(server: &crate::MemoryServer, name: &str) {
    server
        .with_global_store(|store| {
            let changed = store
                .connection()
                .execute(
                    "UPDATE vault_entries SET access_count = 0, accessed_at = '' WHERE name = ?1",
                    [name],
                )
                .map_err(|e| e.to_string())?;
            if changed != 1 {
                return Err(format!("fixture entry '{name}' was not reset"));
            }
            Ok(())
        })
        .expect("reset fixture access count");
}

fn access_count(server: &crate::MemoryServer, name: &str) -> i64 {
    server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(name)
                .map_err(|e| e.to_string())
                .map(|entry| entry.expect("fixture entry exists").access_count)
        })
        .expect("read fixture access count")
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // make_server uses process-wide fixture state
async fn provider_pool_materialization_touches_each_successful_concrete_key_exactly_once() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "access-count-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "ACCESS_COUNT_SUCCESS_API_KEY".to_string(),
            values: vec![
                "fixture-pool-key-1".to_string(),
                "fixture-pool-key-2".to_string(),
                "fixture-pool-key-3".to_string(),
            ],
            agent_id: None,
            description: "access count fixture".to_string(),
            allowed_agents: None,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");
    let names = [
        "ACCESS_COUNT_SUCCESS_API_KEY_1",
        "ACCESS_COUNT_SUCCESS_API_KEY_2",
        "ACCESS_COUNT_SUCCESS_API_KEY_3",
    ];
    for name in names {
        reset_access_count(&server, name);
    }

    let first = crate::vault_ops::load_unlocked_api_key_secret_pools(&server)
        .expect("materialize three-member provider pool");
    assert_eq!(first["ACCESS_COUNT_SUCCESS_API_KEY"].len(), 3);
    for name in names {
        assert_eq!(
            access_count(&server, name),
            1,
            "each successfully materialized concrete key must increment exactly once"
        );
    }

    crate::vault_ops::load_unlocked_api_key_secret_pools(&server)
        .expect("materialize provider pool a second time");
    for name in names {
        assert_eq!(
            access_count(&server, name),
            2,
            "each successful loader call must add exactly one per concrete key"
        );
    }

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "ACCESS_COUNT_SUCCESS_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should materialize the pool once");
    let leased: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    let selected = leased["key_id"].as_str().expect("selected key id");
    assert_eq!(leased["access_count"], serde_json::json!(3));
    assert_eq!(
        access_count(&server, selected),
        3,
        "handler audit count must equal the selected key's post-batch count without a second touch"
    );
    for name in names {
        assert_eq!(
            access_count(&server, name),
            3,
            "lease loader must increment every materialized pool member once"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // make_server uses process-wide fixture state
async fn later_member_decrypt_failure_leaves_every_pool_member_untouched() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "access-count-failure-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "ACCESS_COUNT_FAILURE_API_KEY".to_string(),
            values: vec![
                "fixture-pool-key-1".to_string(),
                "fixture-pool-key-2".to_string(),
                "fixture-pool-key-3".to_string(),
            ],
            agent_id: None,
            description: "access count failure fixture".to_string(),
            allowed_agents: None,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");
    let names = [
        "ACCESS_COUNT_FAILURE_API_KEY_1",
        "ACCESS_COUNT_FAILURE_API_KEY_2",
        "ACCESS_COUNT_FAILURE_API_KEY_3",
    ];
    for name in names {
        reset_access_count(&server, name);
    }
    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("ACCESS_COUNT_FAILURE_API_KEY_3")
                .map_err(|e| e.to_string())?
                .expect("fixture entry exists");
            entry.encrypted_value = "not valid base64".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("corrupt fixture ciphertext");

    let err = match crate::vault_ops::load_unlocked_api_key_secret_pools(&server) {
        Ok(_) => panic!("failed decrypt must prevent materialization"),
        Err(err) => err,
    };
    assert!(err.contains("Bad ciphertext base64"), "{err}");
    for name in names {
        assert_eq!(
            access_count(&server, name),
            0,
            "a later decrypt failure must leave every member untouched"
        );
    }
}
