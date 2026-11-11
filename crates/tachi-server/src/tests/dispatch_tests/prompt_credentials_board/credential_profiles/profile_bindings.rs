use super::*;

#[tokio::test]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn retained_kernel_declared_credentials_materialize_without_explicit_params() {
    use crate::staffing_ops::tests::{
        find_worktree, isolate_staff_repository_environment, CertifiedWorktreeCleanupGuard,
        CurrentDirGuard, StaffRepositoryFixture,
    };
    use crate::test_support::EnvRestore;
    use crate::tests::dispatch_tests::{dispatch_status_is_terminal, wait_for_dispatch_status};
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("isolated run root");
    let temp_worktrees = tempfile::tempdir().expect("isolated worktree root");
    let temp_bin = tempfile::tempdir().expect("isolated worker PATH");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _home = EnvRestore::set_path("HOME", temp_home.path());
    let _runs = EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let _worktrees = EnvRestore::set_path("TACHI_WORKTREES_ROOT", temp_worktrees.path());
    let _git = isolate_staff_repository_environment();
    let repository = StaffRepositoryFixture::new(temp_home.path());
    let credentials_dir = repository.repo.join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    let _cwd = CurrentDirGuard::set(&repository.repo);
    let _cleanup = CertifiedWorktreeCleanupGuard::arm(temp_worktrees.path(), find_worktree);
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _plan_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let _embedding = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    let worker = temp_bin.path().join("opencode");
    std::fs::write(
        &worker,
        "#!/usr/bin/env python3\nimport json, os, sys\nsys.stdin.read()\ncfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}'))\nprint('profile-config-present' if cfg.get('apiKey') == 'opencode-shared-profile-secret' else 'missing')\n",
    )
    .expect("write credential witness worker");
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755))
        .expect("make credential witness executable");
    let path = std::env::join_paths(std::iter::once(temp_bin.path().to_path_buf()).chain(
        std::env::split_paths(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
    ))
    .expect("isolated worker PATH");
    let _path = EnvRestore::set_os("PATH", &path);
    let server = crate::staffing_ops::tests::test_server();
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
                        "profiles": ["kernel_opencode_credentials"]
                    },
                    "materializers": [
                        {
                            "type": "config_content_env",
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
        .seed_vault("dispatch profile credential password")
        .expect("seed unlocked vault");
    server
        .seed_vault_secret(crate::vault_test_api::VaultSecretFixture {
            name: "OPENCODE_SHARED_TEST_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch profile credential binding test".to_string(),
            allowed_agents: None,
        })
        .expect("seed secret");

    // This fixed test-only fixture supplies the credential declaration and
    // verbose routing receipt. It enters the real managed launch kernel; it
    // does not restore the retired public profile or claim provider acceptance.
    let mut request = tachi_params::StaffAssignmentRequest::new(
        tachi_params::TachiDispatchReason::DurableCrossSession,
        "smoke profile-declared credential dispatch",
    );
    request.profile = Some("kernel_opencode_credentials".to_string());
    request.worker = Some("custom".to_string());
    request.project = Some("tachi".to_string());
    request.issue_ref = Some("kckylechen1/tachi#2252".to_string());
    request.declared_file_scope = Some(Vec::new());
    let raw = crate::dispatch_ops::launch_staff_kernel_fixture(&server, request)
        .await
        .expect("retained kernel should start with profile-declared credential")
        .raw;
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["agent"], serde_json::json!("custom"));
    assert_eq!(response["host_adapter"], serde_json::json!("opencode"));
    assert_eq!(
        response["profile"]["host_adapter"],
        serde_json::json!("opencode")
    );
    assert_eq!(
        response["profile"]["selected_profile"],
        serde_json::json!("kernel_opencode_credentials")
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
        serde_json::json!("kernel_opencode_credentials")
    );
    repository.assert_dispatch_source(
        &server,
        response["dispatch_id"].as_str().expect("dispatch id"),
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("profile-config-present"),
        "subprocess should receive profile-declared credential config; result={result}"
    );
    let status = wait_for_dispatch_status(&run_dir).await;
    assert!(dispatch_status_is_terminal(&status), "{status}");
    assert_eq!(status["host_adapter"], serde_json::json!("opencode"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_respect_profile_allowlist() {
    let _lock = crate::utils::global_test_lock()
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
              "allowed_consumers": { "profiles": ["kernel_opencode_credentials"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "OPENCODE_DENIED_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .seed_vault("dispatch profile denied password")
        .expect("seed unlocked vault");
    server
        .seed_vault_secret(crate::vault_test_api::VaultSecretFixture {
            name: "OPENCODE_SHARED_DENIED_SECRET".to_string(),
            value: "denied-profile-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch profile allowlist denial test".to_string(),
            allowed_agents: None,
        })
        .expect("seed secret");

    let mut params = dispatch_params(None, "should fail before spawn");
    params.profile = Some("glm_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.credential_profiles = vec!["opencode_shared".to_string()];
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("non-allowed selected profile should be denied before spawn");
    assert!(err.contains("denied_consumer"), "unexpected error: {err}");
    assert!(err.contains("glm_impl"), "unexpected error: {err}");
    assert!(
        !err.contains("denied-profile-secret-value"),
        "error must not leak secret value: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_credentials_can_allow_backend_agent_name() {
    let _lock = crate::utils::global_test_lock()
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
        .seed_vault("dispatch backend password")
        .expect("seed unlocked vault");
    server
        .seed_vault_secret(crate::vault_test_api::VaultSecretFixture {
            name: "BACKEND_AGENT_SECRET".to_string(),
            value: "backend-agent-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "backend agent allowlist test".to_string(),
            allowed_agents: Some(vec!["custom".to_string()]),
        })
        .expect("seed secret");

    let mut params = dispatch_params(None, "profile selected but agent allowlist is backend");
    params.profile = Some("glm_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
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
