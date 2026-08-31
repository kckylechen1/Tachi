use super::*;

/// A provider result that is persisted after the Vault pool snapshot must
/// invalidate that snapshot before publication. The health row participates in
/// the same revision fence as entries and rotations, so an external durable
/// writer cannot be hidden by a refresh that started earlier.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn persisted_health_change_invalidates_provider_publication_snapshot() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "health-publication-persisted".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "health-publication-secret".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "health publication persisted revision discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set");

    let llm = std::sync::Arc::clone(&server.llm);
    let error = crate::provider_config::materialize_for_server_with_post_vault_load_hook_for_tests(
        &server,
        move || {
            let _ = llm.record_provider_key_result_blocking(
                "SILICONFLOW_API_KEY",
                "SILICONFLOW_API_KEY",
                Some(401),
                None,
                None,
                Some("health publication persisted discriminator"),
            );
        },
    )
    .expect_err("a persisted health change must invalidate the stale snapshot");
    assert!(error.contains("revision changed"), "{error}");
    assert!(error.contains("health"), "{error}");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["SILICONFLOW_API_KEY"])
            .is_none(),
        "a rejected publication must not install the stale provider pool"
    );
}

/// A memory-only result cannot be detected by a second SQLite revision read,
/// but it is still newer than the materialization baseline. Publication keeps
/// that exact logical/member health state under the provider-state write lock,
/// so the just-recorded unusable key cannot become selectable again.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn memory_only_health_change_survives_provider_publication() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _disable_persistence =
        crate::test_support::EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "health-publication-memory".to_string(),
        }))
        .await
        .expect("vault_init");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "SILICONFLOW_API_KEY".to_string(),
            value: "health-publication-secret".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "health publication memory discriminator".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set");

    let llm = std::sync::Arc::clone(&server.llm);
    crate::provider_config::materialize_for_server_with_hook_for_tests(&server, move || {
        let _ = llm.record_provider_key_result_blocking(
            "SILICONFLOW_API_KEY",
            "SILICONFLOW_API_KEY",
            Some(401),
            None,
            None,
            Some("health publication memory discriminator"),
        );
    })
    .expect("memory-only health change must not abort the refresh");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["SILICONFLOW_API_KEY"])
            .is_none(),
        "a newer memory-only auth failure must remain effective after publication"
    );
}
