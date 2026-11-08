use super::*;
use memcore::vault::accounts::{EVENT_KIND_ALIAS_RETIRED, EVENT_KIND_FINGERPRINT_OBSERVED};

async fn fixture() -> (MemoryServer, std::path::PathBuf) {
    let path = crate::utils::test_fixture_path(format!(
        "vault-slot-events-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(path.clone(), None).unwrap();
    crate::vault_test_api::seed_vault(&server, "slot-event-fixture").unwrap();
    for (name, value) in [
        ("DEEPSEEK_API_KEY", "original-account-material"),
        ("SILICONFLOW_API_KEY", "replacement-account-material"),
    ] {
        crate::vault_test_api::owner_set_vault_secret(&server, name, value, None, "", false)
            .await
            .unwrap();
    }
    (server, path)
}

fn seed_legacy_slot(server: &MemoryServer, value: &str) {
    with_vault_key(server, |key| {
        server.with_global_store(|store| {
            let mut slot = store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
            slot.name = "EXTRACT_API_KEY".into();
            (slot.encrypted_value, slot.nonce) =
                crate::vault_crypto::encrypt(key, value.as_bytes())?;
            store
                .vault_upsert_entry(&slot)
                .map_err(|error| error.to_string())
        })
    })
    .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_rebind_reports_truthful_legacy_raw_fingerprint() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    seed_legacy_slot(&server, "orphaned-legacy-slot-material");
    let expected_old = with_vault_key(&server, |key| {
        Ok(crate::vault_ops::slot_rebind::fingerprint_secret(
            key,
            "EXTRACT_API_KEY",
            "orphaned-legacy-slot-material",
        ))
    })
    .unwrap();
    let error = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "",
        None,
        None,
    )
    .expect_err("legacy bytes still require explicit rebind");
    assert!(error.contains(&expected_old), "{error}");
    assert!(!error.contains("orphaned-legacy-slot-material"), "{error}");
    let response = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        true,
        "",
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        response.old_fingerprint.as_deref(),
        Some(expected_old.as_str())
    );
    assert_ne!(
        response.old_fingerprint.as_deref(),
        Some(response.new_fingerprint.as_str())
    );
    server
        .with_global_store(|store| {
            let account = memcore::db::list_provider_accounts(store.connection())
                .unwrap()
                .remove(0);
            let events =
                memcore::db::list_provider_account_events(store.connection(), &account.account_id)
                    .unwrap();
            let event = events
                .iter()
                .find(|event| event.event_kind == "slot_rebind")
                .unwrap();
            let evidence: serde_json::Value = serde_json::from_str(&event.evidence).unwrap();
            assert_eq!(evidence["old_fingerprint"], expected_old);
            assert_eq!(evidence["new_fingerprint"], response.new_fingerprint);
            assert!(!event.evidence.contains("orphaned-legacy-slot-material"));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_remove_refuses_legacy_pointer_without_ledger() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    seed_legacy_slot(&server, "vault:DEEPSEEK_API_KEY");
    assert_eq!(
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
            .unwrap(),
        "original-account-material",
    );
    let error = crate::vault_test_api::owner_remove_vault_secret(&server, "DEEPSEEK_API_KEY")
        .await
        .expect_err("legacy slot still references the physical account row");
    assert!(error.contains("lane slot"), "{error}");
    assert_eq!(
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
            .unwrap(),
        "original-account-material",
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_reject_duplicate_custody_before_binding() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    for name in ["VOYAGE_API_KEY", "VOYAGE_RERANK_API_KEY"] {
        crate::vault_test_api::owner_set_vault_secret(
            &server,
            name,
            "shared-voyage-material",
            None,
            "",
            false,
        )
        .await
        .unwrap();
    }
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:VOYAGE_API_KEY",
        false,
        "",
        None,
        None,
    )
    .unwrap();
    let before = server
        .with_global_store(|store| {
            Ok(memcore::db::list_provider_accounts(store.connection()).unwrap()[0].clone())
        })
        .unwrap();
    let error = crate::vault_test_api::bind_vault_slot(
        &server,
        "SUMMARY_API_KEY",
        "vault:VOYAGE_RERANK_API_KEY",
        false,
        "",
        None,
        None,
    )
    .expect_err("same bytes do not authorize moving another account's custody");
    assert!(error.contains("custody"), "{error}");
    assert!(!error.contains("shared-voyage-material"), "{error}");
    server
        .with_global_store(|store| {
            assert!(store.vault_get_entry("SUMMARY_API_KEY").unwrap().is_none());
            let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0].account_id, before.account_id);
            assert_eq!(accounts[0].revision, before.revision);
            let custody = memcore::db::get_account_custody(store.connection(), &before.account_id)
                .unwrap()
                .unwrap();
            assert_eq!(custody.custody_target, "VOYAGE_API_KEY");
            Ok(())
        })
        .unwrap();
    // The rejected duplicate is still an ordinary unbound entry and may rotate.
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "VOYAGE_RERANK_API_KEY",
        "distinct-rerank-material",
        None,
        "",
        false,
    )
    .await
    .unwrap();
    crate::vault_test_api::bind_vault_slot(
        &server,
        "SUMMARY_API_KEY",
        "vault:VOYAGE_RERANK_API_KEY",
        false,
        "",
        None,
        None,
    )
    .unwrap();
    let rerank = server
        .with_global_store(|store| {
            let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
            assert_eq!(accounts.len(), 2);
            Ok(accounts
                .into_iter()
                .find(|account| account.account_id != before.account_id)
                .unwrap())
        })
        .unwrap();
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "VOYAGE_RERANK_API_KEY",
        "rotated-rerank-material",
        None,
        "",
        false,
    )
    .await
    .unwrap();
    server
        .with_global_store(|store| {
            let after = memcore::db::get_provider_account(store.connection(), &rerank.account_id)
                .unwrap()
                .unwrap();
            assert_eq!(after.revision, rerank.revision + 1);
            assert_ne!(after.account_fingerprint, rerank.account_fingerprint);
            let original =
                memcore::db::get_provider_account(store.connection(), &before.account_id)
                    .unwrap()
                    .unwrap();
            assert_eq!(original.account_fingerprint, before.account_fingerprint);
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_reject_removing_active_account_custody() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "",
        None,
        None,
    )
    .unwrap();
    let before = server
        .with_global_store(|store| Ok(store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap()))
        .unwrap();
    let error = crate::vault_test_api::owner_remove_vault_secret(&server, "DEEPSEEK_API_KEY")
        .await
        .expect_err("plain removal must not leave active account custody dangling");
    assert!(error.contains("custody"), "{error}");
    server
        .with_global_store(|store| {
            let after = store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
            assert_eq!(after.encrypted_value, before.encrypted_value);
            assert_eq!(after.nonce, before.nonce);
            assert!(store.vault_get_entry("EXTRACT_API_KEY").unwrap().is_some());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_remove_slot_retires_only_binding_alias() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "",
        None,
        None,
    )
    .unwrap();
    crate::vault_test_api::owner_remove_vault_secret(&server, "EXTRACT_API_KEY")
        .await
        .unwrap();
    server
        .with_global_store(|store| {
            assert!(store.vault_get_entry("EXTRACT_API_KEY").unwrap().is_none());
            assert!(store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().is_some());
            let account = memcore::db::list_provider_accounts(store.connection())
                .unwrap()
                .remove(0);
            assert_eq!(
                account.status,
                memcore::vault::accounts::ACCOUNT_STATUS_ACTIVE
            );
            let aliases =
                memcore::db::list_provider_account_aliases(store.connection(), &account.account_id)
                    .unwrap();
            assert!(aliases
                .iter()
                .any(|alias| alias.alias_name == "EXTRACT_API_KEY" && alias.retired));
            assert!(aliases
                .iter()
                .any(|alias| alias.alias_name == "DEEPSEEK_API_KEY" && !alias.retired));
            let events =
                memcore::db::list_provider_account_events(store.connection(), &account.account_id)
                    .unwrap();
            assert!(events.iter().any(|event| {
                event.event_kind == EVENT_KIND_ALIAS_RETIRED
                    && serde_json::from_str::<serde_json::Value>(&event.evidence).unwrap()
                        ["alias_name"]
                        == "EXTRACT_API_KEY"
            }));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_account_events_rebind_is_durable_and_rotation_keeps_identity() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, _) = fixture().await;
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "",
        None,
        None,
    )
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

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "rotated-account-material",
        None,
        "",
        false,
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

    let response = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:SILICONFLOW_API_KEY",
        true,
        "",
        None,
        None,
    )
    .unwrap();
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
            assert_eq!(
                evidence["old_fingerprint"].as_str(),
                response.old_fingerprint.as_deref()
            );
            assert_eq!(evidence["new_fingerprint"], response.new_fingerprint);
            assert_eq!(evidence["slot"], "EXTRACT_API_KEY");
            assert_ne!(evidence["old_fingerprint"], evidence["new_fingerprint"]);
            for secret in [
                "original-account-material",
                "rotated-account-material",
                "replacement-account-material",
            ] {
                assert!(!format!("{response:?}").contains(secret));
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
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "",
        None,
        None,
    )
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
    let error = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:SILICONFLOW_API_KEY",
        true,
        "",
        None,
        None,
    )
    .expect_err("no binding can commit without its account event");
    assert!(error.contains("account"), "{error}");
    crate::vault_test_api::owner_remove_vault_secret(&server, "EXTRACT_API_KEY")
        .await
        .expect_err("slot deletion must roll its alias retirement back if the event fails");
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
            let account = memcore::db::list_provider_accounts(store.connection())
                .unwrap()
                .remove(0);
            assert!(memcore::db::list_provider_account_aliases(
                store.connection(),
                &account.account_id
            )
            .unwrap()
            .iter()
            .any(|alias| alias.alias_name == "EXTRACT_API_KEY" && !alias.retired));
            Ok(())
        })
        .unwrap();
}
