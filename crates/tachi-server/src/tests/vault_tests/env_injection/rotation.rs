use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup + subprocess spawn
async fn dispatch_env_injection_uses_logical_rotation_key_not_member_names() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = make_server();

    // Child-env injection is caller-wins by default (#1393, see
    // docs/engineering/architecture/vault-auth-broker.md): Vault supplies only
    // names the caller does not already have. This test is about WHICH NAME the
    // rotation is injected under — logical prefix, not member names — so the
    // ambient value has to be out of the way, or the assertion below measures
    // the developer's own exported TAVILY_API_KEY instead of the vault's.
    let _ambient = crate::test_support::EnvRestore::remove("TAVILY_API_KEY");
    let _ambient_1 = crate::test_support::EnvRestore::remove("TAVILY_API_KEY_1");
    let _ambient_2 = crate::test_support::EnvRestore::remove("TAVILY_API_KEY_2");

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
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "rotated tavily key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "TAVILY_API_KEY".to_string(),
            agent_id: None,
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
