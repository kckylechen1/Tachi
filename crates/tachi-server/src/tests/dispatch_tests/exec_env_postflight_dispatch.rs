//! Integration tests for ExecEnv postflight write-contract verification and terminal receipt (#894 S2e, #1322).

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::super::make_server;
use super::{dispatch_params, wait_for_dispatch_status};
use crate::test_support::EnvRestore;

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

    let mut params = dispatch_params(Some("custom"), "declared scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.cwd = Some(lease_path.to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
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

    let mut params = dispatch_params(Some("custom"), "mutating out of scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.cwd = Some(lease_path.to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
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

    let mut params = dispatch_params(Some("custom"), "unauthorized creation dispatch");
    params.profile = Some("glm_impl".to_string());
    params.cwd = Some(lease_path.to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
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
}
