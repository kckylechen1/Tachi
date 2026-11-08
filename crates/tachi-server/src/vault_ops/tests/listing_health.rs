use super::*;

// Call only while holding global_test_lock. Listing reconciles every config
// location and lane slot, even when the server has an explicit database path.
fn isolate_vault_list_environment() -> (
    crate::test_support::CwdRestore,
    Vec<EnvRestore>,
    tempfile::TempDir,
) {
    let temp = tempfile::tempdir().expect("vault list environment");
    let mut env = ["HOME", "TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"]
        .into_iter()
        .map(|name| EnvRestore::set_path(name, temp.path()))
        .collect::<Vec<_>>();
    env.extend(
        tachi_llm::LANE_SLOT_SECRET_NAMES
            .iter()
            .map(|name| EnvRestore::remove(name)),
    );
    // Restore cwd and environment before removing their temporary directory.
    (crate::test_support::CwdRestore::set(temp.path()), env, temp)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_list_is_a_secret_negative_account_health_board() {
    use memcore::vault::health::{EvidenceKind, TypedOutcome};

    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = isolate_vault_list_environment();
    let _binding = EnvRestore::set("EXTRACT_API_KEY", "  vault:DEEPSEEK_API_KEY  ");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-health-board-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "health-board-password").expect("vault init");

    let secret_sentinel = "tachi-fixture-secret-never-list";
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        &(secret_sentinel.to_string()),
        Some("api_key"),
        "official DeepSeek",
        false,
    )
    .await
    .expect("set DeepSeek fixture");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "ANTHROPIC_API_KEY",
        "anthropic-secret-never-list",
        Some("api_key"),
        "typed unknown fixture",
        false,
    )
    .await
    .expect("set typed unknown fixture");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        &(secret_sentinel.to_string()),
        false,
        "extract lane slot",
        None,
        None,
    )
    .expect("bind extract slot to DeepSeek account");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "LEGACY_DESCRIPTION_ONLY",
        "another-secret-never-list",
        Some("json_blob"),
        "description-only leftover",
        false,
    )
    .await
    .expect("set legacy fixture");

    let (probed_at, credential_generation) = server
        .with_global_store_read(|store| {
            let entry = store
                .vault_get_entry("DEEPSEEK_API_KEY")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "DeepSeek fixture missing".to_string())?;
            let probed_at = chrono::DateTime::parse_from_rfc3339(&entry.updated_at)
                .map(|at| at.with_timezone(&chrono::Utc) + chrono::Duration::seconds(1))
                .map_err(|error| error.to_string())?;
            let entries = store
                .vault_list_entries()
                .map_err(|error| error.to_string())?;
            let rotations = store
                .vault_list_rotations()
                .map_err(|error| error.to_string())?;
            let generation = crate::vault_ops::vault_materialization_acl_revision_from_rows(
                &entries,
                &rotations,
                &[],
            )
            .contents;
            Ok((probed_at, generation))
        })
        .expect("derive probe timestamp and credential generation");
    let exhausted = memcore::vault::health::record_key_outcome_for_generation(
        None,
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_API_KEY",
        TypedOutcome::Exhausted,
        EvidenceKind::Probed,
        None,
        probed_at,
        Some(credential_generation),
    )
    .health;
    let unrelated_newer = memcore::vault::health::record_key_outcome_for_generation(
        None,
        "UNRELATED_POOL",
        "DEEPSEEK_API_KEY",
        TypedOutcome::Success,
        EvidenceKind::Probed,
        None,
        probed_at + chrono::Duration::minutes(1),
        Some(credential_generation),
    )
    .health;
    let raw_error_sentinel = "RAW_PROVIDER_ERROR_BODY_MUST_NOT_LIST";
    let prior_error = memcore::vault::health::record_key_outcome_for_generation(
        None,
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_API_KEY",
        TypedOutcome::Error,
        EvidenceKind::Probed,
        Some(raw_error_sentinel),
        probed_at + chrono::Duration::minutes(2),
        Some(credential_generation),
    )
    .health;
    let unknown = memcore::vault::health::record_key_outcome_for_generation(
        Some(&prior_error),
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_API_KEY",
        TypedOutcome::Unknown,
        EvidenceKind::Probed,
        None,
        probed_at + chrono::Duration::minutes(3),
        Some(credential_generation),
    )
    .health;
    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&exhausted)
                .map_err(|error| error.to_string())?;
            store
                .vault_upsert_key_health(&unrelated_newer)
                .map_err(|error| error.to_string())?;
            store
                .vault_upsert_key_health(&unknown)
                .map_err(|error| error.to_string())
        })
        .expect("seed exact-identity and unknown health");
    crate::provider_config::materialize_for_server(&server)
        .expect("publish one coherent credential and health generation");
    server.llm.record_provider_key_result(
        "EXTRACT_API_KEY",
        "DEEPSEEK_API_KEY",
        Some(200),
        None,
        None,
        None,
    );

    let listed = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list health board");
    let body: serde_json::Value = serde_json::from_str(&listed).expect("list JSON");
    let deepseek = body["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "DEEPSEEK_API_KEY")
        .expect("DeepSeek row");
    assert_eq!(deepseek["secret_type"], "api_key");
    assert_eq!(
        deepseek["bound_slots"],
        serde_json::json!(["EXTRACT_API_KEY"])
    );
    assert_eq!(deepseek["last_probe_class"], "402");
    assert_eq!(deepseek["last_probe_at"], probed_at.to_rfc3339());
    assert_eq!(deepseek["alias_integrity"], "unusable");
    assert_eq!(deepseek["provider_kind"], "deepseek");
    assert!(deepseek["account_id"].as_str().is_some());

    let anthropic = body["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "ANTHROPIC_API_KEY")
        .expect("typed unknown row");
    assert_eq!(anthropic["last_probe_class"], "unknown");
    assert_eq!(
        anthropic["last_probe_at"],
        (probed_at + chrono::Duration::minutes(3)).to_rfc3339()
    );

    let (account_fingerprint, auth_ref) = server
        .with_global_store_read(|store| {
            let account = memcore::db::list_provider_accounts(store.connection())
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|account| account.provider_kind == "deepseek")
                .ok_or_else(|| "DeepSeek account metadata missing".to_string())?;
            Ok((account.account_fingerprint, account.auth_ref))
        })
        .expect("read private account identifiers");

    let legacy = body["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "LEGACY_DESCRIPTION_ONLY")
        .expect("legacy description-only row");
    assert_eq!(legacy["description"], "description-only leftover");
    assert_eq!(legacy["last_probe_class"], "unknown");
    assert!(legacy["last_probe_at"].is_null());

    assert!(
        !listed.contains(secret_sentinel),
        "secret value leaked: {listed}"
    );
    assert!(
        !listed.contains("another-secret-never-list"),
        "secret value leaked: {listed}"
    );
    assert!(
        !listed.contains("anthropic-secret-never-list"),
        "secret value leaked: {listed}"
    );
    assert!(
        !listed.contains(raw_error_sentinel),
        "raw provider error leaked: {listed}"
    );
    assert!(
        !listed.contains(&account_fingerprint),
        "full fingerprint leaked: {listed}"
    );
    if let Some(auth_ref) = auth_ref {
        assert!(
            !listed.contains(&auth_ref),
            "custody reference leaked: {listed}"
        );
    }
    for forbidden_field in [
        "encrypted_value",
        "nonce",
        "secret_length",
        "metadata",
        "last_error",
    ] {
        assert!(
            !listed.contains(forbidden_field),
            "forbidden field leaked: {listed}"
        );
    }

    // Model a same-name replacement whose provider refresh did not publish:
    // the runtime still contains the old member identity and old successful
    // cache generation, while the durable row has a newer generation. Listing
    // must fail closed without comparing plaintext or issuing another probe.
    let replacement_sentinel = "replacement-secret-must-not-list";
    plant_vault_secret(&server, "DEEPSEEK_API_KEY", replacement_sentinel, "api_key");
    server
        .with_global_store(|store| {
            let now = chrono::Utc::now().to_rfc3339();
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "DEEPSEEK_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 2,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|error| error.to_string())
        })
        .expect("seed invalid replacement generation");
    let refresh_error = crate::provider_config::materialize_for_server(&server)
        .expect_err("invalid replacement generation must fail provider refresh");
    assert!(
        refresh_error.contains("DEEPSEEK_API_KEY"),
        "{refresh_error}"
    );
    // Complete an old request after both the same-name replacement and the
    // failed refresh. Its timestamp is newest, but its health came from the
    // still-published old credential generation and must not attach to K2.
    server.llm.record_provider_key_result_for_generation_tests(
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_API_KEY",
        Some(200),
        credential_generation,
    );
    let replaced = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list after unpublished replacement");
    let replaced_body: serde_json::Value =
        serde_json::from_str(&replaced).expect("replacement list JSON");
    let replaced_deepseek = replaced_body["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "DEEPSEEK_API_KEY")
        .expect("replaced DeepSeek row");
    assert_eq!(replaced_deepseek["last_probe_class"], "unknown");
    assert!(replaced_deepseek["last_probe_at"].is_null());
    assert_eq!(replaced_deepseek["alias_integrity"], "unknown");
    assert!(!replaced.contains(replacement_sentinel), "{replaced}");

    handle_vault_lock(&server).await.expect("lock Vault");
    let locked = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list locked health board");
    let locked: serde_json::Value = serde_json::from_str(&locked).expect("locked list JSON");
    let locked_deepseek = locked["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "DEEPSEEK_API_KEY")
        .expect("locked DeepSeek row");
    assert_eq!(
        locked_deepseek["alias_integrity"], "unknown",
        "a locked cache cannot prove alias integrity"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_list_empty_drop_is_current_generation_only_and_locked_unknown() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _environment = isolate_vault_list_environment();
    let _alias = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let temp = tempfile::tempdir().unwrap();
    let server = MemoryServer::new(temp.path().join("memory.db"), None).unwrap();
    crate::vault_test_api::seed_vault(&server, "synthetic-empty-password").unwrap();
    plant_vault_secret(&server, "DEEPSEEK_API_KEY", "", "api_key");
    crate::provider_config::materialize_for_server(&server).expect("materialize empty drop");
    let list = |body: String| -> String {
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        value["credentials"][0]["alias_integrity"]
            .as_str()
            .unwrap()
            .into()
    };
    assert_eq!(
        list(
            handle_vault_list(&server, VaultListParams { secret_type: None })
                .await
                .unwrap()
        ),
        "empty"
    );
    plant_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "synthetic-replacement-not-empty",
        "api_key",
    );
    assert_eq!(
        list(
            handle_vault_list(&server, VaultListParams { secret_type: None })
                .await
                .unwrap()
        ),
        "unknown",
        "stale empty evidence"
    );
    crate::provider_config::materialize_for_server(&server).expect("materialize nonempty");
    assert_eq!(
        list(
            handle_vault_list(&server, VaultListParams { secret_type: None })
                .await
                .unwrap()
        ),
        "resolved",
        "replacement positive control"
    );
    plant_vault_secret(&server, "DEEPSEEK_API_KEY", "", "api_key");
    crate::provider_config::materialize_for_server(&server).unwrap();
    handle_vault_lock(&server).await.unwrap();
    assert_eq!(
        list(
            handle_vault_list(&server, VaultListParams { secret_type: None })
                .await
                .unwrap()
        ),
        "unknown",
        "locked never certifies empty"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_list_real_probe_401_is_distinct_from_403_and_self_report() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _environment = isolate_vault_list_environment();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let _alias = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let mut observations = Vec::new();
    for status in [401, 403] {
        let temp = tempfile::tempdir().unwrap();
        let server = MemoryServer::new(temp.path().join("memory.db"), None).unwrap();
        crate::vault_test_api::seed_vault(&server, "synthetic-probe-password").unwrap();
        plant_vault_secret(
            &server,
            "DEEPSEEK_API_KEY",
            "synthetic-probe-only-secret",
            "api_key",
        );
        crate::provider_config::materialize_for_server(&server).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_handler = count.clone();
        let router = axum::Router::new().route(
            "/models",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let count = count_handler.clone();
                async move {
                    assert_eq!(
                        headers.get("authorization").unwrap(),
                        "Bearer synthetic-probe-only-secret"
                    );
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        "RAW_PROBE_BODY_MUST_NOT_LEAK",
                    )
                }
            }),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let shutdown = cancel.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let (probe, health) = server
            .llm
            .probe_member_auth_and_record_with_endpoint_for_tests(
                tachi_llm::auth_probe_descriptor_for_provider_kind("deepseek").unwrap(),
                "DEEPSEEK_API_KEY",
                "DEEPSEEK_API_KEY",
                &format!("http://{address}/models"),
            )
            .await;
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            probe.auth_class,
            tachi_llm::ProviderAuthProbeClass::AuthFailed
        );
        assert!(health.unwrap().auth_failed);
        server
            .llm
            .await_provider_health_persistence()
            .await
            .expect("real probe persistence terminal");
        let persisted = server
            .with_global_store_read(|store| {
                store
                    .vault_get_key_health("DEEPSEEK_API_KEY", "DEEPSEEK_API_KEY")
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        assert!(persisted.auth_failed);
        assert_eq!(persisted.status, "auth_failed");
        let body = handle_vault_list(&server, VaultListParams { secret_type: None })
            .await
            .unwrap();
        assert!(!body.contains("RAW_PROBE_BODY_MUST_NOT_LEAK"));
        assert!(!body.contains("synthetic-probe-only-secret"));
        let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listed["credentials"][0]["alias_integrity"], "unusable");
        observations.push(
            listed["credentials"][0]["last_probe_class"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        let generation = server.llm.provider_health_board_snapshot().2.unwrap();
        server.llm.record_provider_key_result_for_generation_tests(
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY",
            Some(status),
            generation,
        );
        let body = handle_vault_list(&server, VaultListParams { secret_type: None })
            .await
            .unwrap();
        let reported: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            reported["credentials"][0]["last_probe_class"], "auth_failed",
            "self report is not exact probe authority"
        );
    }
    assert_eq!(observations, ["401", "auth_failed"]);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_list_rejects_persisted_late_old_health_after_rematerialize_and_restart() {
    assert_late_old_health_after_rematerialize_and_restart(false).await;
}

#[tokio::test]
async fn vault_list_same_timestamp_replacement_rejects_late_old_health_after_restart() {
    assert_late_old_health_after_rematerialize_and_restart(true).await;
}

#[allow(clippy::await_holding_lock)]
async fn assert_late_old_health_after_rematerialize_and_restart(same_metadata: bool) {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = isolate_vault_list_environment();
    let _binding = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-health-generation-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "health-generation-password").expect("vault init");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "generation-k1",
        None,
        "",
        false,
    )
    .await
    .expect("set K1");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "generation-k1",
        false,
        "",
        None,
        None,
    )
    .expect("bind extract to K1");
    crate::provider_config::materialize_for_server(&server).expect("materialize K1");
    let k1_generation = server
        .llm
        .provider_health_board_snapshot()
        .2
        .expect("K1 generation");

    let previous_entry = server
        .with_global_store(|store| {
            store
                .vault_get_entry("DEEPSEEK_API_KEY")
                .map_err(|error| error.to_string())
        })
        .expect("read synthetic K1")
        .expect("K1 row");
    plant_vault_secret(&server, "DEEPSEEK_API_KEY", "generation-k2", "api_key");
    if same_metadata {
        server.with_global_store(|store| {
            store.connection().execute(
                "UPDATE vault_entries SET updated_at = ?1, description = ?2 WHERE name = ?3",
                rusqlite::params![previous_entry.updated_at, previous_entry.description, previous_entry.name],
            ).map_err(|error| error.to_string())?;
            let replacement = store.vault_get_entry("DEEPSEEK_API_KEY")
                .map_err(|error| error.to_string())?.expect("K2 row");
            let mut only_ciphertext_changed = previous_entry.clone();
            only_ciphertext_changed.encrypted_value = replacement.encrypted_value.clone();
            only_ciphertext_changed.nonce = replacement.nonce.clone();
            assert_eq!(serde_json::to_value(&replacement).unwrap(), serde_json::to_value(&only_ciphertext_changed).unwrap());
            assert_ne!(replacement.encrypted_value, previous_entry.encrypted_value);
            assert_ne!(replacement.nonce, previous_entry.nonce);
            Ok(())
        }).expect("freeze identical metadata to model same-millisecond replacement");
    }
    let late = server.llm.record_provider_key_result_for_generation_tests(
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_API_KEY",
        Some(200),
        k1_generation,
    );
    assert_eq!(
        memcore::vault::health::credential_generation_from_metadata(&late.metadata),
        Some(k1_generation)
    );

    crate::provider_config::materialize_for_server(&server).expect("materialize K2");
    let k2_generation = server
        .llm
        .provider_health_board_snapshot()
        .2
        .expect("K2 generation");
    if !same_metadata {
        assert_ne!(k1_generation, k2_generation);
    }
    let listed = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list K2 after successful materialization");
    let listed: serde_json::Value = serde_json::from_str(&listed).expect("list JSON");
    let row = listed["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "DEEPSEEK_API_KEY")
        .expect("DeepSeek row");
    assert_eq!(row["last_probe_class"], "unknown");
    assert!(row["last_probe_at"].is_null());
    assert_eq!(row["alias_integrity"], "unknown");

    drop(server);
    let restarted = MemoryServer::new(db_path, None).expect("restart server");
    handle_vault_unlock(
        &restarted,
        VaultUnlockParams {
            password: "health-generation-password".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        },
    )
    .await
    .expect("unlock restarted vault");
    crate::provider_config::materialize_for_server(&restarted)
        .expect("rematerialize after restart");
    let restarted_list = handle_vault_list(&restarted, VaultListParams { secret_type: None })
        .await
        .expect("list after restart");
    let restarted_list: serde_json::Value =
        serde_json::from_str(&restarted_list).expect("restart list JSON");
    let restarted_row = restarted_list["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .find(|row| row["name"] == "DEEPSEEK_API_KEY")
        .expect("restarted DeepSeek row");
    assert_eq!(restarted_row["last_probe_class"], "unknown");
    assert!(restarted_row["last_probe_at"].is_null());
    assert_eq!(restarted_row["alias_integrity"], "unknown");
    assert_ne!(k1_generation, k2_generation);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_list_persisted_account_veto_precedes_older_slot_success_after_restart() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = isolate_vault_list_environment();
    let _binding = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-account-veto-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "account-veto-password").expect("vault init");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "account-veto-fixture",
        None,
        "",
        false,
    )
    .await
    .expect("set DeepSeek");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "account-veto-fixture",
        false,
        "",
        None,
        None,
    )
    .expect("bind extract to DeepSeek");
    crate::provider_config::materialize_for_server(&server).expect("materialize fixture");
    let generation = server
        .llm
        .provider_health_board_snapshot()
        .2
        .expect("credential generation");

    server.llm.record_provider_key_result_for_generation_tests(
        "EXTRACT_API_KEY",
        "DEEPSEEK_API_KEY",
        Some(200),
        generation,
    );
    server.llm.record_provider_key_result_for_generation_tests(
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_API_KEY",
        Some(401),
        generation,
    );
    let listed = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list after account veto");
    let assert_veto = |payload: &str| {
        let payload: serde_json::Value = serde_json::from_str(payload).expect("list JSON");
        let row = payload["credentials"]
            .as_array()
            .expect("credentials")
            .iter()
            .find(|row| row["name"] == "DEEPSEEK_API_KEY")
            .expect("DeepSeek row");
        assert_eq!(row["last_probe_class"], "auth_failed");
        assert!(row["last_probe_at"].as_str().is_some());
        assert_eq!(row["alias_integrity"], "unusable");
    };
    assert_veto(&listed);

    drop(server);
    let restarted = MemoryServer::new(db_path, None).expect("restart server");
    handle_vault_unlock(
        &restarted,
        VaultUnlockParams {
            password: "account-veto-password".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        },
    )
    .await
    .expect("unlock restarted vault");
    crate::provider_config::materialize_for_server(&restarted)
        .expect("rematerialize after restart");
    let restarted_list = handle_vault_list(&restarted, VaultListParams { secret_type: None })
        .await
        .expect("list persisted account veto after restart");
    assert_veto(&restarted_list);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_list_never_projects_identityless_doctor_cache_across_account_changes_or_members() {
    use crate::status_ops::status_health::{ProviderProbeReport, ProviderProbeResult};

    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = isolate_vault_list_environment();
    let _binding = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-identityless-cache-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "identityless-cache-password").expect("vault init");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-cache-fixture",
        None,
        "",
        false,
    )
    .await
    .expect("set DeepSeek");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-cache-fixture",
        false,
        "",
        None,
        None,
    )
    .expect("bind extract to DeepSeek");
    crate::provider_config::materialize_for_server(&server).expect("materialize DeepSeek");

    let write_identityless_ok = || {
        crate::status_ops::status_health::write_provider_probe_cache_report(
            &server.tachi_home_dir(),
            &server.global_db_path_buf(),
            ProviderProbeReport {
                probes: vec![ProviderProbeResult {
                    name: "chat_extract".to_string(),
                    status: "ok".to_string(),
                    message: Some("IDENTITYLESS_CACHE_SENTINEL".to_string()),
                }],
                rotation_groups: Vec::new(),
            },
        )
        .expect("write identity-less cache");
    };
    write_identityless_ok();

    let list_class = |payload: &str, name: &str| {
        let payload: serde_json::Value = serde_json::from_str(payload).expect("list JSON");
        payload["credentials"]
            .as_array()
            .expect("credentials")
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("missing {name}"))["last_probe_class"]
            .as_str()
            .expect("probe class")
            .to_string()
    };
    let deepseek = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list DeepSeek");
    assert_eq!(list_class(&deepseek, "DEEPSEEK_API_KEY"), "unknown");
    assert!(!deepseek.contains("IDENTITYLESS_CACHE_SENTINEL"));

    plant_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-replacement-cache-fixture",
        "api_key",
    );
    crate::provider_config::materialize_for_server(&server).expect("materialize replacement");
    write_identityless_ok();
    let replaced = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list replacement-during-probe case");
    assert_eq!(list_class(&replaced, "DEEPSEEK_API_KEY"), "unknown");

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "OPENAI_API_KEY",
        "openai-cache-fixture",
        None,
        "",
        false,
    )
    .await
    .expect("set OpenAI");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "openai-cache-fixture",
        true,
        "",
        None,
        None,
    )
    .expect("rebind extract to OpenAI");
    std::env::set_var("EXTRACT_API_KEY", "vault:OPENAI_API_KEY");
    crate::provider_config::materialize_for_server(&server).expect("materialize account rebind");
    write_identityless_ok();
    let rebound = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list account rebind");
    assert_eq!(list_class(&rebound, "OPENAI_API_KEY"), "unknown");

    crate::vault_test_api::owner_set_vault_pool(
        &server,
        "VOYAGE_API_KEY",
        &(vec!["voyage-cache-1".to_string(), "voyage-cache-2".to_string()]),
        "round_robin",
    )
    .await
    .expect("set Voyage pool");
    std::env::set_var("EXTRACT_API_KEY", "vault:VOYAGE_API_KEY");
    crate::provider_config::materialize_for_server(&server).expect("materialize Voyage members");
    write_identityless_ok();
    let members = handle_vault_list(&server, VaultListParams { secret_type: None })
        .await
        .expect("list rotation members");
    assert_eq!(list_class(&members, "VOYAGE_API_KEY_1"), "unknown");
    assert_eq!(list_class(&members, "VOYAGE_API_KEY_2"), "unknown");
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
    crate::vault_test_api::seed_vault(&server, "leftover-api-key").expect("vault init");

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
