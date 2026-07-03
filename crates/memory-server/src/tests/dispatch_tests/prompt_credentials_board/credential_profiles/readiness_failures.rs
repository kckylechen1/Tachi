use super::*;

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
            agent_id: None,
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
            agent_id: None,
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
