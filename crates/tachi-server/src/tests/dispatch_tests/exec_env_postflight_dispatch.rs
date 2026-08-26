//! Integration tests for ExecEnv postflight write-contract verification and terminal receipt (#894 S2e, #1322).

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::super::make_server;
use super::{dispatch_params, wait_for_dispatch_status};
use crate::test_support::EnvRestore;

fn seed_managed_lease(
    server: &crate::server_state::MemoryServer,
    lease_path: &std::path::Path,
    suffix: &str,
) -> (String, String) {
    let env_id = format!("env-postflight-{suffix}");
    let resource_id = format!("res-postflight-{suffix}");
    server
        .with_global_store(|store| {
            memcore::insert_exec_env(
                store.connection_mut(),
                &memcore::NewExecEnvLease {
                    env_id: env_id.clone(),
                    kind: "worktree".to_string(),
                    path: lease_path.to_string_lossy().to_string(),
                    repo_root: lease_path.to_string_lossy().to_string(),
                    branch: format!("test/{suffix}"),
                    base_sha: "test-base".to_string(),
                    dispatch_id: None,
                    env_class: memcore::EnvClass::EditOnly,
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::insert_resource(
                store.connection_mut(),
                &memcore::NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: memcore::ResourceKind::Worktree,
                    path: lease_path.to_string_lossy().to_string(),
                    bytes: None,
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::bind_resource(store.connection_mut(), &env_id, &resource_id)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("seed managed postflight lease");
    (env_id, resource_id)
}

fn resource_state(
    server: &crate::server_state::MemoryServer,
    resource_id: &str,
) -> memcore::ResourceState {
    server
        .with_global_store_read(|store| {
            memcore::get_resource(store.connection(), resource_id)
                .map_err(|error| error.to_string())
        })
        .expect("read managed resource")
        .expect("managed resource exists")
        .state
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn required_postflight_rejects_unmanaged_and_default_bindings_before_spawn() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let server = make_server();
    let unmanaged = TempDir::new().expect("unmanaged cwd");

    for (label, cwd, unmanaged_cwd) in [
        (
            "unmanaged",
            Some(unmanaged.path().to_string_lossy().to_string()),
            Some(true),
        ),
        ("default", None, None),
    ] {
        let marker = temp_home.path().join(format!("{label}-spawned"));
        let mut params = dispatch_params(Some("custom"), "required postflight pre-spawn fence");
        params.profile = Some("glm_impl".to_string());
        params.cwd = cwd;
        params.unmanaged_cwd = unmanaged_cwd;
        params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
        params.command = vec![
            "python3".to_string(),
            "-c".to_string(),
            format!("open({marker:?}, 'w').write('spawned')"),
        ];

        let error = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
            .await
            .expect_err("required postflight must reject a non-managed binding");
        assert!(error.contains("requires a managed env_id"), "{error}");
        assert!(!marker.exists(), "{label} worker must not spawn");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn postflight_dispatch_with_declared_scope_accepts_in_scope_write() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    fs::write(lease_path.join("forbidden.txt"), b"initial forbidden\n").expect("write forbidden");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "clean");

    let mut params = dispatch_params(Some("custom"), "declared scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id);
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('allowed.txt', 'w').write('updated allowed')".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));

    let terminal_status = wait_for_dispatch_status(&run_dir).await;
    assert!(matches!(
        terminal_status["state"].as_str(),
        Some("TASK_STATE_COMPLETED" | "TASK_STATE_CLOSED")
    ));
    assert_eq!(terminal_status["result_written"], true);

    let postflight = &terminal_status["exec_env_postflight"];
    assert_eq!(postflight["gate"], "exec_env_postflight");
    assert_eq!(postflight["verdict"], "clean");
    assert_eq!(postflight["artifacts"], "released");
    assert_eq!(postflight["lease_action"], "none");
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Active
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn postflight_dispatch_rejects_and_withholds_when_worker_mutates_out_of_scope() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    fs::write(lease_path.join("forbidden.txt"), b"initial forbidden\n").expect("write forbidden");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "mutated");

    let mut params = dispatch_params(Some("custom"), "mutating out of scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id);
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('forbidden.txt', 'w').write('bad mutation')".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));

    let terminal_status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(terminal_status["state"], "TASK_STATE_FAILED");
    assert_eq!(terminal_status["result_written"], false);
    assert!(terminal_status["result_persist_error"].is_string());

    let postflight = &terminal_status["exec_env_postflight"];
    assert_eq!(postflight["gate"], "exec_env_postflight");
    assert_eq!(postflight["verdict"], "rejected");
    assert_eq!(postflight["artifacts"], "withheld");
    assert_eq!(postflight["lease_action"], "quarantined");
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Quarantined
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn postflight_dispatch_rejects_and_withholds_when_untracked_file_created_outside_scope() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "created");

    let mut params = dispatch_params(Some("custom"), "unauthorized creation dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id);
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('untracked_secret.txt', 'w').write('unauthorized creation')".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));

    let terminal_status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(terminal_status["state"], "TASK_STATE_FAILED");
    assert_eq!(terminal_status["result_written"], false);
    assert!(terminal_status["result_persist_error"].is_string());

    let postflight = &terminal_status["exec_env_postflight"];
    assert_eq!(postflight["gate"], "exec_env_postflight");
    assert_eq!(postflight["verdict"], "rejected");
    assert_eq!(postflight["artifacts"], "withheld");
    assert_eq!(postflight["lease_action"], "quarantined");
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Quarantined
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_declared_scope_is_visible_on_the_automatic_presence_claim() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    fs::write(lease_dir.path().join("allowed.txt"), b"initial\n").expect("write allowed");
    let (env_id, _) = seed_managed_lease(&server, lease_dir.path(), "claim-scope");

    let mut params = dispatch_params(Some("custom"), "observable declared scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id);
    params.issue_ref = Some("#1322-claim-scope".to_string());
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import time; time.sleep(0.5)".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    let observed_scope: String = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT declared_file_scope FROM session_claims WHERE dispatch_id = ?1 AND state = 'active'",
                    rusqlite::params![dispatch_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read automatic presence claim");
    assert_eq!(observed_scope, r#"["allowed.txt"]"#);

    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let _ = wait_for_dispatch_status(&run_dir).await;
}
