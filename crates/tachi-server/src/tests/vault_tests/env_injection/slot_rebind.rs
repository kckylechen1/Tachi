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
