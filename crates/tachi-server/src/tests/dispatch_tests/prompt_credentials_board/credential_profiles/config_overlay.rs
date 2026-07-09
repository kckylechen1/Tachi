use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_injects_config_overlay_env_without_response_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "opencode_config_profile": {
                    "provider": "opencode",
                    "entries": {
                        "api_key": "OPENCODE_ROUTER_SECRET"
                    },
                    "allowed_consumers": {
                        "agents": ["custom"]
                    },
                    "materializers": [
                        {
                            "type": "config_overlay",
                            "source": "api_key",
                            "target": "OPENCODE_CONFIG_CONTENT",
                            "template": {
                                "provider": "openai",
                                "apiKey": "{{secret}}"
                            }
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "opencode-router-secret-value";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch config credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_ROUTER_SECRET".to_string(),
            value: secret_value.to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "dispatch config credential test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke credential config overlay dispatch");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.credential_profiles = vec!["opencode_config_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import json, os; cfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}')); print('config-present' if cfg.get('apiKey') == 'opencode-router-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with materialized config overlay");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["steps"][0]["output"],
        serde_json::json!("config_overlay:OPENCODE_CONFIG_CONTENT")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["status"],
        serde_json::json!("prepared_config_env")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["redacted"],
        serde_json::json!(true)
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("config-present"),
        "subprocess should receive rendered config without printing it; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"credentials_materialized\""),
        "trajectory should record redacted credential materialization: {trajectory}"
    );
    assert!(
        !trajectory.contains(secret_value),
        "trajectory must not leak secret value: {trajectory}"
    );
}
