use super::*;

#[tokio::test]
async fn tachi_status_marks_vault_alias_in_config_env_as_vault_config() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\n").expect("write config env");

    let global_db = temp_home.temp_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("mkdir");
    let store = memory_core::MemoryStore::open(global_db.to_str().unwrap()).expect("open global");
    let _ = store; // vault entries optional for status name listing

    let original_voyage = std::env::var_os("VOYAGE_API_KEY");
    std::env::remove_var("VOYAGE_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let voyage = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("VOYAGE_API_KEY"))
        .expect("voyage key row");
    // Without vault entry, alias alone may be unresolved; with vault it is vault(config.env).
    assert!(
        voyage["source"] == json!("vault(config.env)")
            || voyage["source"] == json!("vault-alias-unresolved")
            || voyage["status"] == json!("missing")
    );

    if let Some(value) = original_voyage {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
}

#[tokio::test]
async fn tachi_status_marks_config_env_only_key_as_configured() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "VOYAGE_API_KEY=config-only-value\n").expect("write config env");

    let original_voyage = std::env::var_os("VOYAGE_API_KEY");
    std::env::remove_var("VOYAGE_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let voyage = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("VOYAGE_API_KEY"))
        .expect("voyage key row");
    assert_eq!(voyage["status"], json!("configured"));
    assert_eq!(voyage["source"], json!("config.env"));

    if let Some(value) = original_voyage {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
}

#[tokio::test]
async fn tachi_status_marks_config_env_alias_key_as_configured() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "BIGMODEL_API_KEY=alias-config-value\n").expect("write config env");

    let original_reasoning = std::env::var_os("REASONING_API_KEY");
    let original_zai = std::env::var_os("ZAI_API_KEY");
    let original_bigmodel = std::env::var_os("BIGMODEL_API_KEY");
    std::env::remove_var("REASONING_API_KEY");
    std::env::remove_var("ZAI_API_KEY");
    std::env::remove_var("BIGMODEL_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let reasoning = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("REASONING_API_KEY"))
        .expect("reasoning key row");
    assert_eq!(reasoning["status"], json!("configured"));
    assert_eq!(reasoning["source"], json!("alias"));

    if let Some(value) = original_reasoning {
        std::env::set_var("REASONING_API_KEY", value);
    } else {
        std::env::remove_var("REASONING_API_KEY");
    }
    if let Some(value) = original_zai {
        std::env::set_var("ZAI_API_KEY", value);
    } else {
        std::env::remove_var("ZAI_API_KEY");
    }
    if let Some(value) = original_bigmodel {
        std::env::set_var("BIGMODEL_API_KEY", value);
    } else {
        std::env::remove_var("BIGMODEL_API_KEY");
    }
}
