use super::*;

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
        .seed_vault("child-env-default-password")
        .expect("seed unlocked vault");
    for (name, value) in [
        ("TACHI_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_EXISTING_API_KEY", "vault-overrides-env"),
        ("LONGPORT_APP_SECRET", "longport-secret"),
    ] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env default injection test".to_string(),
                allowed_agents: None,
            })
            .expect("seed secret");
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
async fn default_and_fill_missing_preserve_inherited_openai_api_key() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
    std::env::set_var("OPENAI_API_KEY", "caller-openai-value");
    let server = make_server();
    server
        .seed_vault("child-env-openai-fill-password")
        .expect("seed unlocked vault");
    server
        .seed_vault_secret(VaultSecretFixture {
            name: "OPENAI_API_KEY".to_string(),
            value: "vault-openai-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "provider-shaped child env precedence fixture".to_string(),
            allowed_agents: None,
        })
        .expect("seed secret");

    for mode in [None, Some("fill_missing")] {
        match mode {
            Some(mode) => std::env::set_var("TACHI_VAULT_CHILD_ENV", mode),
            None => std::env::remove_var("TACHI_VAULT_CHILD_ENV"),
        }
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg("printf '%s' \"$OPENAI_API_KEY\"");
        let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
        assert_eq!(injected, 0, "mode {mode:?} must preserve inherited name");
        let output = cmd.output().await.expect("env probe command should run");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).expect("env probe output should be utf8"),
            "caller-openai-value",
            "mode {mode:?} must keep the caller's inherited provider value"
        );
    }

    std::env::remove_var("OPENAI_API_KEY");
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
        .seed_vault("child-env-password")
        .expect("seed unlocked vault");
    for (name, value) in [
        ("TACHI_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_EXISTING_API_KEY", "vault-overrides-env"),
        ("LONGPORT_APP_SECRET", "longport-secret"),
        ("NOT-A-SHELL-NAME", "must-not-inject"),
    ] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env injection test".to_string(),
                allowed_agents: None,
            })
            .expect("seed secret");
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
        .seed_vault("child-env-fill-password")
        .expect("seed unlocked vault");
    for (name, value) in [
        ("TACHI_FILL_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_FILL_EXISTING_API_KEY", "vault-preserved"),
        ("TACHI_FILL_LONGPORT_APP_SECRET", "longport-secret"),
    ] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env fill-missing test".to_string(),
                allowed_agents: None,
            })
            .expect("seed secret");
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
