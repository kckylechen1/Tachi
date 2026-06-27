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
