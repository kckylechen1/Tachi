use super::*;

fn reset_access_counts(server: &crate::server_state::MemoryServer, names: &[&str]) {
    server
        .with_global_store(|store| {
            for name in names {
                store
                    .connection()
                    .execute(
                        "UPDATE vault_entries SET access_count = 0 WHERE name = ?1",
                        [*name],
                    )
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("reset access_count should succeed");
}

fn access_count(server: &crate::server_state::MemoryServer, name: &str) -> i64 {
    server
        .with_global_store_read(|store| {
            let entry = store
                .vault_get_entry(name)
                .map_err(|e| e.to_string())?
                .unwrap_or_else(|| panic!("{name} should exist"));
            Ok(entry.access_count)
        })
        .unwrap_or_else(|e| panic!("read access_count for {name}: {e}"))
}

/// #1393-L6: materialize/vault-pool reads must bump `access_count` the same way
/// `vault_get` does via `record_successful_vault_access`.
#[tokio::test]
async fn materialize_for_server_bumps_vault_api_key_access_count() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "access-count-fidelity".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "VOYAGE_API_KEY".to_string(),
            value: "voyage-materialize-secret".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "materialize access_count probe".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    // vault_set refreshes provider secrets; reset so the materialize call below
    // is the sole access that should bump the counter.
    reset_access_counts(&server, &["VOYAGE_API_KEY"]);

    assert_eq!(
        access_count(&server, "VOYAGE_API_KEY"),
        0,
        "seeded vault api_key should start at access_count=0"
    );

    crate::provider_config::materialize_for_server(&server)
        .expect("materialize_for_server should read unlocked vault pools");

    assert_eq!(
        access_count(&server, "VOYAGE_API_KEY"),
        1,
        "materialize vault_pools path must bump access_count exactly once"
    );
}

/// Lease must bump only the selected key, exactly once (loader must not also bump).
#[tokio::test]
async fn vault_lease_api_key_bumps_selected_access_count_exactly_once() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "lease-access-count".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "ROT_PROBE_API_KEY".to_string(),
            agent_id: None,
            values: vec!["lease-key-1".to_string(), "lease-key-2".to_string()],
            strategy: "round_robin".to_string(),
            description: "lease access_count probe".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");

    reset_access_counts(&server, &["ROT_PROBE_API_KEY_1", "ROT_PROBE_API_KEY_2"]);
    assert_eq!(access_count(&server, "ROT_PROBE_API_KEY_1"), 0);
    assert_eq!(access_count(&server, "ROT_PROBE_API_KEY_2"), 0);

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "ROT_PROBE_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should succeed");
    let leased_json: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    assert_eq!(leased_json["key_id"], json!("ROT_PROBE_API_KEY_1"));

    assert_eq!(
        access_count(&server, "ROT_PROBE_API_KEY_1"),
        1,
        "selected key must receive exactly one access_count bump on lease"
    );
    assert_eq!(
        access_count(&server, "ROT_PROBE_API_KEY_2"),
        0,
        "non-selected rotation member must not be bumped by lease pool load"
    );
}

/// Failed decrypt must not bump access_count for the unbroken entry path that never
/// successfully includes the corrupt key in a returned pool.
#[tokio::test]
async fn failed_decrypt_does_not_bump_access_count() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "decrypt-no-bump".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "CORRUPT_API_KEY".to_string(),
            value: "will-be-corrupted".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "failed decrypt access_count probe".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("CORRUPT_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("CORRUPT_API_KEY should exist");
            entry.encrypted_value = "not valid base64".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("corrupt ciphertext");

    reset_access_counts(&server, &["CORRUPT_API_KEY"]);
    assert_eq!(access_count(&server, "CORRUPT_API_KEY"), 0);

    // Call the vault pool loader directly: materialize_for_server swallows
    // vault decrypt Err and falls back to keychain.
    let err = match crate::provider_config::vault_api_key_pools_from_server(&server) {
        Ok(_) => panic!("corrupt ciphertext must fail vault pool decrypt"),
        Err(err) => err,
    };
    assert!(
        err.contains("Bad ciphertext base64")
            || err.contains("ciphertext")
            || err.contains("base64"),
        "expected decrypt failure, got: {err}"
    );

    assert_eq!(
        access_count(&server, "CORRUPT_API_KEY"),
        0,
        "failed decrypt must not bump access_count"
    );
}
