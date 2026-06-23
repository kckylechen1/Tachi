use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_injects_env_without_response_secret() {
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
        credentials_dir.join("dispatch.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "dispatch_env_profile": {
                    "provider": "test",
                    "entries": {
                        "api_key": "DISPATCH_PROFILE_SECRET"
                    },
                    "allowed_consumers": {
                        "agents": ["custom"]
                    },
                    "materializers": [
                        {
                            "type": "env",
                            "source": "api_key",
                            "target": "PROFILE_ENV_SECRET"
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "dispatch-profile-secret-value";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DISPATCH_PROFILE_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke credential dispatch env");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.credential_profiles = vec!["dispatch_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('PROFILE_ENV_SECRET') == 'dispatch-profile-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with materialized credential");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["steps"][0]["output"],
        serde_json::json!("env:PROFILE_ENV_SECRET")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["status"],
        serde_json::json!("prepared_env")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["redacted"],
        serde_json::json!(true)
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("present"),
        "subprocess should receive credential env without printing it; result={result}"
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_requires_unlocked_vault_before_spawn() {
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
            "locked_env_profile": {
              "entries": { "api_key": "LOCKED_DISPATCH_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "LOCKED_ENV_SECRET" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch locked password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "LOCKED_DISPATCH_SECRET".to_string(),
            value: "locked-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential lock test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let mut params = dispatch_params(Some("custom"), "should fail before spawn");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["locked_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("locked vault should fail dispatch before spawn");
    assert!(err.contains("requires unlocked Vault secret"), "err: {err}");
    assert!(err.contains("Vault is locked"), "err: {err}");
    let runs_dir = temp_home.path().join("runs");
    let run_dir = std::fs::read_dir(&runs_dir)
        .expect("runs dir exists")
        .next()
        .expect("failed dispatch should leave run status")
        .expect("run dir entry")
        .path();
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status should exist"),
    )
    .expect("status JSON");
    assert_eq!(status["state"], serde_json::json!("TASK_STATE_FAILED"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_denies_consumer_before_decrypting_secret() {
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
            "denied_env_profile": {
              "entries": { "api_key": "DENIED_DISPATCH_SECRET" },
              "allowed_consumers": { "agents": ["other-agent"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "DENIED_ENV_SECRET" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch denied password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DENIED_DISPATCH_SECRET".to_string(),
            value: "denied-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential deny test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let mut params = dispatch_params(Some("custom"), "should fail before decrypt");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["denied_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("denied consumer should fail dispatch before decrypt");
    assert!(
        err.contains("not ready for consumer 'custom'"),
        "err: {err}"
    );
    assert!(err.contains("denied_consumer"), "err: {err}");
    assert!(
        !err.contains("Vault is locked"),
        "denied profile should fail before decrypting/unlocking secret: {err}"
    );
    assert!(
        !err.contains("denied-secret-value"),
        "redacted readiness error must not leak secret: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_legacy_vault_env_binding_still_injects_without_credential_profile() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    std::fs::create_dir_all(project.path().join(".tachi")).expect("create .tachi dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        project.path().join(".tachi/vault.env"),
        "LEGACY_DISPATCH_ENV=vault:LEGACY_DISPATCH_SECRET\n",
    )
    .expect("write vault.env");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch legacy password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "LEGACY_DISPATCH_SECRET".to_string(),
            value: "legacy-dispatch-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "legacy dispatch env test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke legacy vault env dispatch");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('LEGACY_DISPATCH_ENV') == 'legacy-dispatch-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("legacy env dispatch should start");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert!(
        response["credentials"]
            .as_array()
            .is_some_and(|v| v.is_empty()),
        "legacy env path should not synthesize credential reports: {response:#}"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("present"),
        "legacy vault env should still reach subprocess; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"legacy_vault_env_injected\""),
        "legacy env injection should be auditable without values: {trajectory}"
    );
}
