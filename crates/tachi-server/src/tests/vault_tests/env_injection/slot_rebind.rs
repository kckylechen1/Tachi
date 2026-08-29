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

async fn slot_value(server: &crate::tests::TestServer, name: &str) -> String {
    let get = server
        .vault_get(Parameters(VaultGetParams {
            name: name.to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("vault_get");
    let json: serde_json::Value = serde_json::from_str(&get).expect("json");
    json["value"].as_str().expect("value").to_string()
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
    assert!(err.contains("fp1:"), "{err}");
    assert!(err.contains("rebind"), "{err}");
    assert!(!err.contains("siliconflow-original"), "{err}");
    assert!(!err.contains("glm-copied-in"), "{err}");
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
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "same-slot-bytes"
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
        slot_value(&server, "EXTRACT_API_KEY").await,
        "original-family"
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
async fn lane_slot_refuses_copying_existing_account_ciphertext() {
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
    let err = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "shared-siliconflow-bytes",
            false,
        )))
        .await
        .expect_err("copy into slot");
    assert!(err.contains("SILICONFLOW_API_KEY"), "{err}");
    assert!(err.contains("vault:SILICONFLOW_API_KEY"), "{err}");
    assert!(!err.contains("shared-siliconflow-bytes"), "{err}");
}

/// Copy protection remains in force when the lane slot already exists and the
/// caller explicitly requests a rebind. Rebind changes an account family; it
/// must not create a second ciphertext for the same account credential.
#[tokio::test]
async fn existing_lane_slot_refuses_account_copy_even_with_rebind() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-existing-copy".to_string(),
        }))
        .await
        .expect("init");
    server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "old-slot-family",
            false,
        )))
        .await
        .expect("initial slot write");
    server
        .vault_set(Parameters(slot_params(
            "SILICONFLOW_API_KEY",
            "shared-account-bytes",
            false,
        )))
        .await
        .expect("account write");

    let err = server
        .vault_set(Parameters(slot_params(
            "EXTRACT_API_KEY",
            "shared-account-bytes",
            true,
        )))
        .await
        .expect_err("existing slot must reject account copy");
    assert!(err.contains("vault:SILICONFLOW_API_KEY"), "{err}");
    assert!(!err.contains("shared-account-bytes"), "{err}");
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "old-slot-family"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_first_writers_cannot_bypass_lane_slot_rebind() {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "slot-rebind-race".to_string(),
        }))
        .await
        .expect("init");

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut writers = Vec::new();
    for index in 0..16 {
        let server = std::ops::Deref::deref(&server).clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            let value = format!("concurrent-family-{index}");
            barrier.wait().await;
            let result = server
                .vault_set(Parameters(slot_params("EXTRACT_API_KEY", &value, false)))
                .await;
            (value, result)
        }));
    }

    let mut winner = None;
    let mut refusals = 0;
    for writer in writers {
        let (value, result) = writer.await.expect("writer task");
        match result {
            Ok(_) => {
                assert!(
                    winner.replace(value).is_none(),
                    "only one first writer may win"
                );
            }
            Err(error) => {
                assert!(error.contains("rebind"), "{error}");
                refusals += 1;
            }
        }
    }
    let winner = winner.expect("one first writer succeeds");
    assert_eq!(refusals, 15);
    assert_eq!(slot_value(&server, "EXTRACT_API_KEY").await, winner);
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
    server
        .vault_set(Parameters(slot_params(
            "SILICONFLOW_API_KEY",
            "external-account-family",
            false,
        )))
        .await
        .expect("seed account");

    let db_path = server.global_db_path_buf();
    let mut competing_store = memcore::MemoryStore::open(
        db_path
            .to_str()
            .expect("global fixture database path is UTF-8"),
    )
    .expect("open independent store connection");
    let transaction = competing_store
        .begin_vault_transaction()
        .expect("begin competing immediate transaction");
    let mut copied_account = transaction
        .vault_get_entry("SILICONFLOW_API_KEY")
        .expect("read seeded account")
        .expect("seeded account exists");
    copied_account.name = "EXTRACT_API_KEY".to_string();
    transaction
        .vault_upsert_entry(&copied_account)
        .expect("stage competing slot write");

    let independent_server = std::ops::Deref::deref(&server).clone();
    let writer = tokio::spawn(async move {
        independent_server
            .vault_set(Parameters(slot_params(
                "EXTRACT_API_KEY",
                "mcp-writer-family",
                false,
            )))
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !writer.is_finished(),
        "MCP writer must wait for the independent SQLite writer"
    );
    transaction.commit().expect("commit competing slot write");

    let error = writer
        .await
        .expect("MCP writer task")
        .expect_err("MCP writer must re-evaluate after the competing commit");
    assert!(error.contains("rebind"), "{error}");
    assert!(!error.contains("external-account-family"), "{error}");
    assert!(!error.contains("mcp-writer-family"), "{error}");
    assert_eq!(
        slot_value(&server, "EXTRACT_API_KEY").await,
        "external-account-family"
    );
}
