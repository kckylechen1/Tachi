use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn provider_key_health_blocking_persist_honors_test_disable_env() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

    let health = client.record_provider_key_result_blocking(
        "TACHI_TEST_ONLY_API_KEY_DISABLED_PERSIST",
        "TACHI_TEST_ONLY_API_KEY_DISABLED_PERSIST_1",
        Some(429),
        None,
        Some(30),
        Some("provider throttled"),
    );

    assert_eq!(health.status, HEALTH_RATE_LIMITED);
    assert!(
        !db_path.exists(),
        "blocking persist should not create a DB when test persistence is disabled"
    );
    let status = client.provider_health_status();
    assert!(status.persist_last_attempt_at.is_none());
    assert!(status.persist_last_error.is_none());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_key_health_persists_off_async_runtime_thread() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

    let health = client.record_provider_key_result(
        "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
        "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
        Some(429),
        None,
        Some(30),
        Some("provider throttled"),
    );
    assert_eq!(health.status, HEALTH_RATE_LIMITED);

    client
        .await_provider_health_persistence()
        .await
        .expect("background key-health persist should finish");
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen vault db");
    let persisted = store
        .vault_get_key_health(
            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
        )
        .expect("read persisted health")
        .expect("background key-health persist should be visible after the boundary");
    assert_eq!(persisted.status, HEALTH_RATE_LIMITED);
    assert_eq!(
        persisted.last_error.as_deref(),
        Some("rate limited; retry after 30s")
    );

    let status = client.provider_health_status();
    assert!(status.persist_last_attempt_at.is_some());
    assert!(status.persist_last_success_at.is_some());
    assert!(status.persist_last_error.is_none());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_key_health_phase_boundary_joins_contested_writer() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("initialize vault db");
    drop(store);

    let writer_gate = rusqlite::Connection::open(&db_path).expect("open writer gate");
    writer_gate
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold the vault writer boundary");

    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
    let health = client.record_provider_key_result(
        "TACHI_TEST_ONLY_API_KEY_PHASE_BOUNDARY",
        "TACHI_TEST_ONLY_API_KEY_PHASE_BOUNDARY_1",
        Some(429),
        None,
        Some(30),
        Some("provider throttled"),
    );
    assert_eq!(health.status, HEALTH_RATE_LIMITED);

    let terminal = client.await_provider_health_persistence();
    tokio::pin!(terminal);
    tokio::select! {
        biased;
        result = &mut terminal => {
            panic!("phase boundary returned before the contested writer terminated: {result:?}");
        }
        _ = tokio::task::yield_now() => {}
    }

    writer_gate
        .execute_batch("COMMIT")
        .expect("release the vault writer boundary");
    terminal
        .await
        .expect("phase boundary should finish after the writer releases");

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen vault db");
    let persisted = store
        .vault_get_key_health(
            "TACHI_TEST_ONLY_API_KEY_PHASE_BOUNDARY",
            "TACHI_TEST_ONLY_API_KEY_PHASE_BOUNDARY_1",
        )
        .expect("read persisted health")
        .expect("joined writer must be visible at the next phase");
    assert_eq!(persisted.status, HEALTH_RATE_LIMITED);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_key_health_persist_errors_are_visible_in_status() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("missing-parent").join("vault.db");
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

    let health = client.record_provider_key_result(
        "TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR",
        "TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR_1",
        Some(429),
        None,
        Some(30),
        Some("provider throttled"),
    );
    assert_eq!(health.status, HEALTH_RATE_LIMITED);

    let terminal_error = client
        .await_provider_health_persistence()
        .await
        .expect_err("phase boundary must surface the persistence failure");
    let status = client.provider_health_status();

    let error = status
        .persist_last_error
        .as_deref()
        .expect("persist error should be visible");
    assert!(status.persist_last_attempt_at.is_some());
    assert!(status.persist_last_success_at.is_none());
    assert!(
        error.contains("persist vault key health for TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR"),
        "unexpected persist error: {error}"
    );
    assert_eq!(terminal_error, error);
}

#[tokio::test]
async fn provider_key_health_reloads_external_db_cooldowns_before_selection() {
    const KEY: &str = "TACHI_TEST_ONLY_API_KEY_RELOAD_COOLDOWN";
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
    drop(store);

    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
    client.set_provider_secret_pool(
        KEY,
        vec![
            ProviderSecret {
                key_id: format!("{KEY}_1"),
                value: "secret-one".to_string(),
            },
            ProviderSecret {
                key_id: format!("{KEY}_2"),
                value: "secret-two".to_string(),
            },
        ],
    );

    let now = Utc::now();
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
    store
        .vault_upsert_key_health(&VaultKeyHealth {
            logical_name: KEY.to_string(),
            key_id: format!("{KEY}_1"),
            status: HEALTH_RATE_LIMITED.to_string(),
            cooldown_until: Some((now + chrono::Duration::seconds(60)).to_rfc3339()),
            last_attempt: Some(now.to_rfc3339()),
            last_error: Some("manual CLI cooldown".to_string()),
            updated_at: now.to_rfc3339(),
            ..VaultKeyHealth::default()
        })
        .expect("write external key health");
    drop(store);

    client.force_provider_health_reload_due_for_tests();
    let selected = client
        .required_selected_secret_or_wait(&[KEY], 1, "test reload")
        .await
        .expect("selection should not fail")
        .expect("second key should be selected");

    assert_eq!(selected.key_id, format!("{KEY}_2"));
    let status = client.provider_health_status();
    assert_eq!(status.source_of_truth, "vault_db");
    assert!(status.last_success_at.is_some());
    assert!(status.last_error.is_none());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn provider_key_health_reload_clears_local_cooldown_on_external_success() {
    const KEY: &str = "TACHI_TEST_ONLY_API_KEY_RELOAD_SUCCESS";
    let _guard = crate::test_support::global_test_lock().lock();
    // The local rate-limit helper persists asynchronously in production. This
    // fixture is specifically testing an *external* DB success superseding
    // local cooldown state, so suppress that local write and avoid a stale
    // background rate-limit write racing the external success below.
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
    client.set_provider_secret_pool(
        KEY,
        vec![
            ProviderSecret {
                key_id: format!("{KEY}_1"),
                value: "secret-one".to_string(),
            },
            ProviderSecret {
                key_id: format!("{KEY}_2"),
                value: "secret-two".to_string(),
            },
        ],
    );
    client.mark_provider_key_rate_limited_for_tests(KEY, &format!("{KEY}_1"), Some(300));

    let now = Utc::now() + chrono::Duration::seconds(1);
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
    store
        .vault_upsert_key_health(&VaultKeyHealth {
            logical_name: KEY.to_string(),
            key_id: format!("{KEY}_1"),
            status: HEALTH_OK.to_string(),
            last_success: Some(now.to_rfc3339()),
            updated_at: now.to_rfc3339(),
            ..VaultKeyHealth::default()
        })
        .expect("write external success health");
    drop(store);

    client.force_provider_health_reload_due_for_tests();
    let selected = client
        .required_selected_secret_or_wait(&[KEY], 1, "test reload")
        .await
        .expect("selection should not fail")
        .expect("first key should be reinstated");

    assert_eq!(selected.key_id, format!("{KEY}_1"));
    assert!(client
        .provider_pool_statuses()
        .into_iter()
        .find(|status| status.logical_name == KEY)
        .is_some_and(|status| status.available_keys == 2));
}
