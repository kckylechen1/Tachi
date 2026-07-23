use super::*;

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
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE vault_entries SET access_count = 0 WHERE name = ?1",
                    ["VOYAGE_API_KEY"],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("reset access_count should succeed");

    let before = server
        .with_global_store_read(|store| {
            let entry = store
                .vault_get_entry("VOYAGE_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("VOYAGE_API_KEY should exist");
            Ok(entry.access_count)
        })
        .expect("read access_count before materialize");
    assert_eq!(
        before, 0,
        "seeded vault api_key should start at access_count=0"
    );

    crate::provider_config::materialize_for_server(&server)
        .expect("materialize_for_server should read unlocked vault pools");

    let after = server
        .with_global_store_read(|store| {
            let entry = store
                .vault_get_entry("VOYAGE_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("VOYAGE_API_KEY should exist");
            Ok(entry.access_count)
        })
        .expect("read access_count after materialize");
    assert!(
        after >= 1,
        "materialize vault_pools path must bump access_count; got {after}"
    );
}
