use super::*;

#[tokio::test]
async fn vault_get_auto_rotate_does_not_advance_rotation_on_decrypt_failure() {
    let server = make_server();

    server
        .seed_vault("rotation-failure-password")
        .expect("seed unlocked vault");

    for (name, value) in [
        ("BROKEN_API_KEY_1", "broken-key-1"),
        ("BROKEN_API_KEY_2", "broken-key-2"),
    ] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated key".to_string(),
                allowed_agents: None,
            })
            .expect("seed rotation member");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "BROKEN_API_KEY".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("BROKEN_API_KEY_1")
                .map_err(|e| e.to_string())?
                .expect("rotation member should exist");
            entry.encrypted_value = "not valid base64".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("corrupt rotation member");

    let err = server
        .read_vault_secret("BROKEN_API_KEY", None, true)
        .expect_err("corrupted selected key should fail before rotation advances");
    assert!(err.contains("Bad ciphertext base64"), "{err}");

    let current_index = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("BROKEN_API_KEY")
                .map_err(|e| e.to_string())
                .map(|rotation| rotation.expect("rotation should exist").current_index)
        })
        .expect("read rotation");
    assert_eq!(
        current_index, 1,
        "failed decrypt must not skip the selected rotation member"
    );
}
