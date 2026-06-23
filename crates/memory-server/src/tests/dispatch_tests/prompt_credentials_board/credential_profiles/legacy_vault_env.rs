use super::*;

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
