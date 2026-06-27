use super::*;

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
