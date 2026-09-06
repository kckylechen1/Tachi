use super::*;

fn slot_params(name: &str, value: &str, rebind: bool) -> VaultSetParams {
    VaultSetParams {
        name: name.to_string(),
        value: value.to_string(),
        agent_id: None,
        secret_type: "api_key".to_string(),
        description: "slot-rebind discriminator".to_string(),
        allowed_agents: None,
        enable_rotation: false,
        rotation_strategy: None,
        rebind,
    }
}

async fn seed_account(server: &crate::tests::TestServer, name: &str, value: &str) {
    server
        .vault_set(Parameters(slot_params(name, value, false)))
        .await
        .unwrap_or_else(|error| panic!("seed account {name}: {error}"));
}

fn corrupt_account_entry(server: &crate::tests::TestServer, name: &str, invalid_utf8: bool) {
    let key = {
        let vault = server.vault_read();
        *vault.key.as_ref().expect("unlocked vault key").bytes()
    };
    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry(name)
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| panic!("missing account {name}"));
            if invalid_utf8 {
                let (encrypted_value, nonce) = crate::vault_crypto::encrypt(&key, &[0xff, 0xfe])
                    .map_err(|error| error.to_string())?;
                entry.encrypted_value = encrypted_value;
                entry.nonce = nonce;
            } else {
                entry.encrypted_value = "corrupt-ciphertext".into();
                entry.nonce = "corrupt-nonce".into();
            }
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .expect("corrupt account entry");
}

fn stored_slot_value(server: &crate::tests::TestServer, name: &str) -> String {
    let key = {
        let vault = server.vault_read();
        *vault.key.as_ref().expect("unlocked vault key").bytes()
    };
    server
        .with_global_store_read(|store| {
            let entry = store
                .vault_get_entry(name)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("missing lane slot {name}"))?;
            let decrypted =
                crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)
                    .map_err(|error| error.to_string())?;
            crate::vault_crypto::decode_utf8_zeroizing(
                decrypted,
                format!("lane slot {name} is not valid UTF-8"),
            )
        })
        .expect("read stored lane slot value")
}

async fn slot_value(server: &crate::tests::TestServer, name: &str) -> String {
    let get = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: name.to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("materialize account through public Vault lease");
    let json: serde_json::Value = serde_json::from_str(&get).expect("json");
    assert_eq!(json["leased"], true);
    if crate::vault_ops::is_lane_slot_secret_name(name) {
        let pointer = stored_slot_value(server, name);
        assert_eq!(json["key_id"], pointer.strip_prefix("vault:").unwrap());
    }
    json["env"][name]
        .as_str()
        .expect("leased value")
        .to_string()
}

/// tachi#1855: copying a different family into EXTRACT_API_KEY without
/// `--rebind` must refuse and leave the old ciphertext.
#[tokio::test]
async fn lane_slot_overwrite_without_rebind_is_refused() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-refuse".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "siliconflow-original").await;
    seed_account(&server, "DEEPSEEK_API_KEY", "glm-copied-in").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "siliconflow-original",
            false,
        )))
        .await
        .expect("first write");

    let err = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "glm-copied-in",
            false,
        )))
        .await
        .expect_err("overwrite without rebind");
    assert!(err.contains("EXTRACT_API_KEY"), "{err}");
    assert!(err.matches("fp1:").count() >= 2, "{err}");
    assert!(err.contains("old_fingerprint="), "{err}");
    assert!(err.contains("new_fingerprint="), "{err}");
    assert!(err.contains("rebind"), "{err}");
    assert!(!err.contains("siliconflow-original"), "{err}");
    assert!(!err.contains("glm-copied-in"), "{err}");
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY").to_string(),
        "vault:SILICONFLOW_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "siliconflow-original"
    );
}

#[tokio::test]
async fn lane_slot_same_bytes_are_noop_not_refusal() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-same".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "same-slot-bytes").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "same-slot-bytes",
            false,
        )))
        .await
        .expect("first write");
    let body = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "same-slot-bytes",
            false,
        )))
        .await
        .expect("identical overwrite");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["stored"], json!(true));
    assert_eq!(json["rebind"], json!(false));
    assert_eq!(json["noop"], json!(true));
    assert_eq!(json["old_fingerprint"], json["new_fingerprint"]);
    assert_eq!(json["bound_account"], json!("SILICONFLOW_API_KEY"));
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "same-slot-bytes"
    );
}

#[tokio::test]
async fn explicit_lane_slot_bind_ignores_unrelated_corrupt_accounts() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-unrelated-corruption".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "DEEPSEEK_API_KEY", "selected-family").await;
    seed_account(&server, "SILICONFLOW_API_KEY", "bad-bytes").await;
    seed_account(&server, "OPENAI_API_KEY", "corrupt-ciphertext-family").await;
    corrupt_account_entry(&server, "SILICONFLOW_API_KEY", true);
    corrupt_account_entry(&server, "OPENAI_API_KEY", false);

    let body = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            false,
        )))
        .await
        .expect("healthy explicit target must bind");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["bound_account"], "DEEPSEEK_API_KEY");
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "selected-family"
    );
    server
        .vault_set(Parameters(slot_params(
            "SUMMARY_API_KEY",
            "selected-family",
            false,
        )))
        .await
        .expect("raw matching must skip unrelated corrupt accounts");
    assert_eq!(
        stored_slot_value(&server, "SUMMARY_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );
}

#[tokio::test]
async fn explicit_lane_slot_bind_rejects_corrupt_selected_account_unchanged() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-selected-corruption".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "DEEPSEEK_API_KEY", "original-family").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            false,
        )))
        .await
        .expect("initial bind");
    corrupt_account_entry(&server, "DEEPSEEK_API_KEY", true);

    let error = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            false,
        )))
        .await
        .expect_err("corrupt selected account must fail");
    assert!(error.contains("not valid UTF-8"), "{error}");
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );
}

#[tokio::test]
async fn lane_slot_rebind_updates_and_returns_fingerprints() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-ok".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "siliconflow-original").await;
    seed_account(&server, "DEEPSEEK_API_KEY", "glm-rebound").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "siliconflow-original",
            false,
        )))
        .await
        .expect("first write");
    let body = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "glm-rebound",
            true,
        )))
        .await
        .expect("rebind");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["rebind"], json!(true));
    let old_fp = json["old_fingerprint"].as_str().expect("old fp");
    let new_fp = json["new_fingerprint"].as_str().expect("new fp");
    assert!(old_fp.starts_with("fp1:"), "{old_fp}");
    assert!(new_fp.starts_with("fp1:"), "{new_fp}");
    assert_ne!(old_fp, new_fp);
    assert!(!body.contains("siliconflow-original"));
    assert!(!body.contains("glm-rebound"));
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );
    assert_eq!(slot_value(&server, "EXTRACT_API_KEY").await, "glm-rebound");
}

#[tokio::test]
async fn lane_slot_rebind_rejects_empty_and_whitespace_values() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-empty".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "original-family").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "original-family",
            false,
        )))
        .await
        .expect("seed slot");

    for invalid in ["", " \t\n"] {
        let error = server
            .vault_set(Parameters(slot_params("EXTRACT_API_KEY", invalid, true)))
            .await
            .expect_err("empty-family rebind must be refused");
        assert!(error.contains("cannot be empty"), "{error}");
    }
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "original-family"
    );
}

#[tokio::test]
async fn lane_slot_rebind_rejects_secret_type_override_bypass() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-type-override".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "original-family").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "original-family",
            false,
        )))
        .await
        .expect("seed slot");

    let mut bypass = slot_params("EXTRACT_API_KEY", "replacement-family", false);
    bypass.secret_type = "other".to_string();
    let error = server
        .vault_set(Parameters(bypass))
        .await
        .expect_err("type override must not bypass rebind");
    assert!(error.contains("must use secret_type 'api_key'"), "{error}");
    assert!(!error.contains("replacement-family"), "{error}");
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "original-family"
    );
}

#[tokio::test]
async fn legacy_non_api_key_lane_slot_fails_closed_before_api_overwrite() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-legacy-type".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "legacy-family").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "legacy-family",
            false,
        )))
        .await
        .expect("seed slot");
    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("EXTRACT_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("seeded lane slot");
            entry.secret_type = "other".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("install legacy lane-slot type");

    let error = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "replacement-family",
            false,
        )))
        .await
        .expect_err("legacy lane slot must fail closed");
    assert!(error.contains("legacy secret_type 'other'"), "{error}");
    assert!(error.contains("Remove or migrate"), "{error}");
    assert!(!error.contains("replacement-family"), "{error}");
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );
}

#[tokio::test]
async fn account_name_rotation_does_not_require_rebind() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-account".to_string(),
        }))
        .await
        .expect("init");
    server
        .vault_set(Parameters(slot_params(
            "DEEPSEEK_API_KEY",
            "deepseek-old",
            false,
        )))
        .await
        .expect("first write");
    server
        .vault_set(Parameters(slot_params(
            "DEEPSEEK_API_KEY",
            "deepseek-rotated",
            false,
        )))
        .await
        .expect("account rotation");
    assert_eq!(
        slot_value(&server, "DEEPSEEK_API_KEY").await,
        "deepseek-rotated"
    );
}

#[tokio::test]
async fn lane_slot_binds_to_existing_account_without_copying_ciphertext() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-copy".to_string(),
        }))
        .await
        .expect("init");
    server
        .vault_set(Parameters(slot_params(
            "SILICONFLOW_API_KEY",
            "shared-siliconflow-bytes",
            false,
        )))
        .await
        .expect("account write");
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "shared-siliconflow-bytes",
            false,
        )))
        .await
        .expect("bind existing account");
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "shared-siliconflow-bytes"
    );
}

#[tokio::test]
async fn existing_lane_slot_rebinds_to_registered_account_pointer() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-existing-copy".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "DEEPSEEK_API_KEY", "old-slot-family").await;
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "old-slot-family",
            false,
        )))
        .await
        .expect("initial slot write");
    seed_account(&server, "SILICONFLOW_API_KEY", "shared-account-bytes").await;
    let body = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "shared-account-bytes",
            true,
        )))
        .await
        .expect("explicit rebind to registered account");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["rebind"], json!(true));
    assert!(json["old_fingerprint"].as_str().is_some());
    assert!(json["new_fingerprint"].as_str().is_some());
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "shared-account-bytes"
    );
}

#[tokio::test]
async fn lane_slot_refuses_copy_from_unregistered_mcp_account() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-unregistered-account".to_string(),
        }))
        .await
        .expect("init");
    server
        .vault_set(Parameters(slot_params(
            "MCP_CONTEXT7_API_KEY",
            "shared-mcp-account-bytes",
            false,
        )))
        .await
        .expect("seed unregistered MCP account");

    let error = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "shared-mcp-account-bytes",
            true,
        )))
        .await
        .expect_err("lane slot must reject a copied unregistered account");
    assert!(error.contains("MCP_CONTEXT7_API_KEY"), "{error}");
    assert!(!error.contains("shared-mcp-account-bytes"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_first_writers_cannot_bypass_lane_slot_rebind() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-race".to_string(),
        }))
        .await
        .expect("init");
    seed_account(
        &server,
        "SILICONFLOW_API_KEY",
        "concurrent-family-siliconflow",
    )
    .await;
    seed_account(&server, "DEEPSEEK_API_KEY", "concurrent-family-deepseek").await;

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut writers = Vec::new();
    for (name, value) in [
        ("SILICONFLOW_API_KEY", "concurrent-family-siliconflow"),
        ("DEEPSEEK_API_KEY", "concurrent-family-deepseek"),
    ] {
        let server = std::ops::Deref::deref(&server).clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            barrier.wait().await;
            let result = server
                .vault_set(Parameters(slot_params("EXTRACT_API_KEY", value, false)))
                .await;
            (name, value, result)
        }));
    }

    let mut winner = None;
    let mut refusals = 0;
    for writer in writers {
        let (name, value, result) = writer.await.expect("writer task");
        match result {
            Ok(_) => {
                assert!(
                    winner.replace((name, value)).is_none(),
                    "only one first writer may win"
                );
            }
            Err(error) => {
                assert!(error.contains("rebind"), "{error}");
                refusals += 1;
            }
        }
    }
    let (winner_name, winner_value) = winner.expect("one first writer succeeds");
    assert_eq!(refusals, 1);
    assert_eq!(
        stored_slot_value(&server, "EXTRACT_API_KEY"),
        format!("vault:{winner_name}")
    );
    assert_eq!(slot_value(&server, "EXTRACT_API_KEY").await, winner_value);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_store_connection_cannot_bypass_lane_slot_rebind() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-cross-connection".to_string(),
        }))
        .await
        .expect("init");
    seed_account(&server, "SILICONFLOW_API_KEY", "external-account-family").await;
    seed_account(&server, "DEEPSEEK_API_KEY", "mcp-writer-family").await;

    let db_path = server.global_db_path_buf();
    let mut store_a = memcore::MemoryStore::open(
        db_path
            .to_str()
            .expect("global fixture database path is UTF-8"),
    )
    .expect("open independent store A");
    let mut store_b = memcore::MemoryStore::open(
        db_path
            .to_str()
            .expect("global fixture database path is UTF-8"),
    )
    .expect("open independent store B");
    let key = {
        let vault = server.vault_read();
        *vault.key.as_ref().expect("unlocked key").bytes()
    };
    let key_a = key;
    let key_b = key;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let barrier_a = std::sync::Arc::clone(&barrier);
    let handle_a = std::thread::spawn(move || {
        barrier_a.wait();
        crate::vault_ops::account_bind::write_lane_slot_binding(
            &mut store_a,
            &key_a,
            "EXTRACT_API_KEY",
            "external-account-family",
            false,
            "",
            None,
            None,
        )
    });
    let handle_b = std::thread::spawn(move || {
        barrier.wait();
        crate::vault_ops::account_bind::write_lane_slot_binding(
            &mut store_b,
            &key_b,
            "EXTRACT_API_KEY",
            "mcp-writer-family",
            false,
            "",
            None,
            None,
        )
    });
    let result_a = handle_a.join().expect("store A writer");
    let result_b = handle_b.join().expect("store B writer");
    assert_eq!(
        result_a.is_ok() as u8 + result_b.is_ok() as u8,
        1,
        "one independent store writer must win"
    );
    let a_ok = result_a.is_ok();
    let error = if result_a.is_err() {
        result_a.expect_err("store A refusal")
    } else {
        result_b.expect_err("store B refusal")
    };
    assert!(error.contains("old_fingerprint="), "{error}");
    assert!(error.contains("new_fingerprint="), "{error}");
    assert!(!error.contains("external-account-family"), "{error}");
    assert!(!error.contains("mcp-writer-family"), "{error}");
    let winner_value = if a_ok {
        "external-account-family"
    } else {
        "mcp-writer-family"
    };
    assert_eq!(slot_value(&server, "EXTRACT_API_KEY").await, winner_value);
}
