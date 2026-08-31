use super::*;
use memcore::vault::accounts::{EVENT_KIND_ALIAS_RETIRED, EVENT_KIND_FINGERPRINT_OBSERVED};

async fn fixture() -> (MemoryServer, std::path::PathBuf) {
    let path = crate::utils::test_fixture_path(format!(
        "vault-slot-events-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(path.clone(), None).unwrap();
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-event-fixture".into(),
        },
    )
    .await
    .unwrap();
    for (name, value) in [
        ("DEEPSEEK_API_KEY", "original-account-material"),
        ("SILICONFLOW_API_KEY", "replacement-account-material"),
    ] {
        handle_vault_set(&server, vault_set_params(name, value, false))
            .await
            .unwrap();
    }
    (server, path)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_rebind_is_durable_and_rotation_keeps_identity() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY", false),
    )
    .await
    .unwrap();
    let original = server
        .with_global_store(|store| {
            let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
            assert_eq!(
                accounts.len(),
                1,
                "first binding must have a durable provider account"
            );
            Ok(accounts[0].clone())
        })
        .unwrap();

    handle_vault_set(
        &server,
        vault_set_params("DEEPSEEK_API_KEY", "rotated-account-material", false),
    )
    .await
    .unwrap();
    server
        .with_global_store(|store| {
            let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0].account_id, original.account_id);
            assert_ne!(
                accounts[0].account_fingerprint,
                original.account_fingerprint
            );
            let events =
                memcore::db::list_provider_account_events(store.connection(), &original.account_id)
                    .unwrap();
            assert!(events
                .iter()
                .any(|event| event.event_kind == EVENT_KIND_FINGERPRINT_OBSERVED));
            Ok(())
        })
        .unwrap();

    let response = handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "vault:SILICONFLOW_API_KEY", true),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&response).unwrap();
    server
        .with_global_store(|store| {
            let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
            assert_eq!(accounts.len(), 2);
            let replacement = accounts
                .iter()
                .find(|account| account.account_id != original.account_id)
                .unwrap();
            let old_aliases = memcore::db::list_provider_account_aliases(
                store.connection(),
                &original.account_id,
            )
            .unwrap();
            assert!(old_aliases
                .iter()
                .any(|alias| alias.alias_name == "EXTRACT_API_KEY" && alias.retired));
            let old_events =
                memcore::db::list_provider_account_events(store.connection(), &original.account_id)
                    .unwrap();
            assert!(old_events
                .iter()
                .any(|event| event.event_kind == EVENT_KIND_ALIAS_RETIRED));
            let events = memcore::db::list_provider_account_events(
                store.connection(),
                &replacement.account_id,
            )
            .unwrap();
            let event = events
                .iter()
                .find(|event| event.event_kind == "slot_rebind")
                .expect("durable rebind event");
            let evidence: serde_json::Value = serde_json::from_str(&event.evidence).unwrap();
            assert_eq!(evidence["old_fingerprint"], body["old_fingerprint"]);
            assert_eq!(evidence["new_fingerprint"], body["new_fingerprint"]);
            assert_eq!(evidence["slot"], "EXTRACT_API_KEY");
            assert_ne!(evidence["old_fingerprint"], evidence["new_fingerprint"]);
            for secret in [
                "original-account-material",
                "rotated-account-material",
                "replacement-account-material",
            ] {
                assert!(!response.contains(secret));
                assert!(events.iter().all(|event| !event.evidence.contains(secret)));
            }
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_failure_rolls_back_binding_and_new_identity() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, path) = fixture().await;
    handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY", false),
    )
    .await
    .unwrap();
    let before = server
        .with_global_store(|store| Ok(store.vault_get_entry("EXTRACT_API_KEY").unwrap().unwrap()))
        .unwrap();
    // Only the disposable fixture is changed. The ordinary store authorizer
    // remains installed; a missing event table fails after the slot upsert.
    crate::test_support::with_unrestricted_fixture_connection(&path, |connection| {
        connection
            .execute_batch("ALTER TABLE provider_account_events RENAME TO fixture_account_events")
    })
    .unwrap();
    let error = handle_vault_set(
        &server,
        vault_set_params("EXTRACT_API_KEY", "vault:SILICONFLOW_API_KEY", true),
    )
    .await
    .expect_err("no binding can commit without its account event");
    assert!(error.contains("account"), "{error}");
    server
        .with_global_store(|store| {
            let after = store.vault_get_entry("EXTRACT_API_KEY").unwrap().unwrap();
            assert_eq!(after.encrypted_value, before.encrypted_value);
            assert_eq!(after.nonce, before.nonce);
            assert_eq!(
                memcore::db::list_provider_accounts(store.connection())
                    .unwrap()
                    .len(),
                1
            );
            Ok(())
        })
        .unwrap();
}
