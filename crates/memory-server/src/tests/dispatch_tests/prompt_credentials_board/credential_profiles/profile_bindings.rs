use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_materialize_without_explicit_params() {
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
                "opencode_shared": {
                    "provider": "opencode",
                    "entries": {
                        "api_key": "OPENCODE_SHARED_TEST_SECRET"
                    },
                    "allowed_consumers": {
                        "profiles": ["opencode_builder"]
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
    let secret_value = "opencode-shared-profile-secret";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch profile credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_SHARED_TEST_SECRET".to_string(),
            value: secret_value.to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "dispatch profile credential binding test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "smoke profile-declared credential dispatch");
    params.profile = Some("opencode_builder".to_string());
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import json, os; cfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}')); print('profile-config-present' if cfg.get('apiKey') == 'opencode-shared-profile-secret' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with profile-declared credential");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["agent"], serde_json::json!("opencode"));
    assert_eq!(response["host_adapter"], serde_json::json!("opencode"));
    assert_eq!(
        response["profile"]["host_adapter"],
        serde_json::json!("opencode")
    );
    assert_eq!(
        response["profile"]["selected_profile"],
        serde_json::json!("opencode_builder")
    );
    assert_eq!(
        response["profile"]["credential_profiles"][0],
        serde_json::json!("opencode_shared")
    );
    assert_eq!(
        response["credentials"][0]["profile"],
        serde_json::json!("opencode_shared")
    );
    assert_eq!(
        response["credentials"][0]["consumer"],
        serde_json::json!("opencode_builder")
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("profile-config-present"),
        "subprocess should receive profile-declared credential config; result={result}"
    );
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("read dispatch status"),
    )
    .expect("status JSON");
    assert_eq!(status["host_adapter"], serde_json::json!("opencode"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_respect_profile_allowlist() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        r#"{
          "credential_profiles": {
            "opencode_shared": {
              "entries": { "api_key": "OPENCODE_SHARED_DENIED_SECRET" },
              "allowed_consumers": { "profiles": ["opencode_builder"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "OPENCODE_DENIED_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch profile denied password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_SHARED_DENIED_SECRET".to_string(),
            value: "denied-profile-secret-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "dispatch profile allowlist denial test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "should fail before spawn");
    params.profile = Some("glm_51_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["opencode_shared".to_string()];
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("non-allowed selected profile should be denied before spawn");
    assert!(err.contains("denied_consumer"), "unexpected error: {err}");
    assert!(err.contains("glm_51_impl"), "unexpected error: {err}");
    assert!(
        !err.contains("denied-profile-secret-value"),
        "error must not leak secret value: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_credentials_can_allow_backend_agent_name() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "backend_agent_profile": {
              "entries": { "api_key": "BACKEND_AGENT_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "BACKEND_AGENT_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch backend password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "BACKEND_AGENT_SECRET".to_string(),
            value: "backend-agent-secret-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "backend agent allowlist test".to_string(),
            allowed_agents: Some(vec!["custom".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "profile selected but agent allowlist is backend");
    params.profile = Some("glm_51_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["backend_agent_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('BACKEND_AGENT_ENV') == 'backend-agent-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("backend agent allowlist should work even with selected dispatch profile");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["consumer"],
        serde_json::json!("custom"),
        "credential consumer should be backend agent when allowed_consumers.agents matches"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(result.contains("present"), "result={result}");
}
