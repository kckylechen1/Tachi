use super::*;

#[tokio::test]
async fn dispatch_env_injection_uses_logical_rotation_key_not_member_names() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "hunter2-hunter2".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("TAVILY_API_KEY_1", "tavily-key-1"),
        ("TAVILY_API_KEY_2", "tavily-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated tavily key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "TAVILY_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s' \"$TAVILY_API_KEY\" \"${TAVILY_API_KEY_1-unset}\" \"${TAVILY_API_KEY_2-unset}\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert!(injected >= 1);
    let output = cmd.output().await.expect("run env inspection command");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");

    assert!(
        stdout == "tavily-key-1|unset|unset" || stdout == "tavily-key-2|unset|unset",
        "expected logical TAVILY_API_KEY only, got {stdout}"
    );
}

#[tokio::test]
async fn vault_lock_preserves_env_provider_fallback() {
    std::env::set_var("TACHI_ENV_FALLBACK_API_KEY", "env-secret");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "env-fallback-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "TACHI_ENV_FALLBACK_API_KEY".to_string(),
            value: "vault-secret".to_string(),
            secret_type: "api_key".to_string(),
            description: "env fallback test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["TACHI_ENV_FALLBACK_API_KEY"])
            .as_deref(),
        Some("vault-secret")
    );

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["TACHI_ENV_FALLBACK_API_KEY"])
            .as_deref(),
        Some("env-secret"),
        "locking vault must clear only vault overrides and preserve env fallback"
    );
    std::env::remove_var("TACHI_ENV_FALLBACK_API_KEY");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup + subprocess spawn
async fn dispatch_vault_env_injection_does_not_export_all_secrets_by_default() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
    std::env::set_var("TACHI_EXISTING_API_KEY", "env-value");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "child-env-default-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("TACHI_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_EXISTING_API_KEY", "vault-overrides-env"),
        ("LONGPORT_APP_SECRET", "longport-secret"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env default injection test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|' \"$TACHI_CHILD_ONLY_API_KEY\" \"$TACHI_EXISTING_API_KEY\" \"$LONGPORT_APP_SECRET\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert_eq!(injected, 0);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(stdout, "|env-value||");

    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("TACHI_EXISTING_API_KEY");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup + subprocess spawn
async fn dispatch_vault_env_injection_overrides_existing_env_when_all_configured() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("NOT-A-SHELL-NAME");
    std::env::set_var("TACHI_VAULT_CHILD_ENV", "all");
    std::env::set_var("TACHI_EXISTING_API_KEY", "env-value");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "child-env-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("TACHI_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_EXISTING_API_KEY", "vault-overrides-env"),
        ("LONGPORT_APP_SECRET", "longport-secret"),
        ("NOT-A-SHELL-NAME", "must-not-inject"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env injection test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg("printf '%s|%s|%s|' \"$TACHI_CHILD_ONLY_API_KEY\" \"$TACHI_EXISTING_API_KEY\" \"$LONGPORT_APP_SECRET\"; env | grep -q '^NOT-A-SHELL-NAME=' && printf bad || true");
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert_eq!(injected, 3);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(
        stdout,
        "child-vault-value|vault-overrides-env|longport-secret|"
    );

    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("NOT-A-SHELL-NAME");
    std::env::remove_var("TACHI_EXISTING_API_KEY");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");

    std::env::remove_var("TACHI_FILL_CHILD_ONLY_API_KEY");
    std::env::remove_var("TACHI_FILL_LONGPORT_APP_SECRET");
    std::env::set_var("TACHI_FILL_EXISTING_API_KEY", "env-value");
    std::env::set_var("TACHI_VAULT_CHILD_ENV", "fill_missing");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "child-env-fill-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("TACHI_FILL_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_FILL_EXISTING_API_KEY", "vault-preserved"),
        ("TACHI_FILL_LONGPORT_APP_SECRET", "longport-secret"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env fill-missing test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|' \"$TACHI_FILL_CHILD_ONLY_API_KEY\" \"$TACHI_FILL_EXISTING_API_KEY\" \"$TACHI_FILL_LONGPORT_APP_SECRET\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert_eq!(injected, 2);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(stdout, "child-vault-value|env-value|longport-secret|");

    std::env::remove_var("TACHI_FILL_CHILD_ONLY_API_KEY");
    std::env::remove_var("TACHI_FILL_LONGPORT_APP_SECRET");
    std::env::remove_var("TACHI_FILL_EXISTING_API_KEY");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
}

#[tokio::test]
async fn dispatch_vault_env_injection_resolves_project_bindings_from_cwd() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "project-env-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("longbridge.1.secret", "longbridge-secret"),
        ("project.override.secret", "project-override"),
        ("PROJECT_SHARED_API_KEY", "global-shared"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "project env binding test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let temp = tempfile::tempdir().expect("create temp project");
    let project = temp.path().join("project");
    let nested = project.join("src/nested");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "\
# Project-local aliases, not plaintext secrets.
PROJECT_LONGPORT_SECRET=vault:longbridge.1.secret
PROJECT_SHARED_API_KEY=vault:project.override.secret
BAD-NAME=vault:project.override.secret
PROJECT_LITERAL=not-a-vault-alias
PROJECT_MISSING=vault:missing.secret
",
    )
    .expect("write project vault env");

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|' \"$PROJECT_LONGPORT_SECRET\" \"$PROJECT_SHARED_API_KEY\" \"$PROJECT_LITERAL\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, Some(&nested));
    assert_eq!(injected, 2);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(stdout, "longbridge-secret|project-override||");
}
