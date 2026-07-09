use super::*;

#[test]
fn dispatch_run_cleanup_only_for_completed_success() {
    assert!(crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_COMPLETED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_FAILED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_INPUT_REQUIRED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(1),
        Some("TASK_STATE_COMPLETED")
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_run_dir_is_created_with_0o700() {
    use std::os::unix::fs::PermissionsExt;

    let (server, _temp_home) = make_server_with_temp_home();
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");

    let mut params = dispatch_params(Some("custom"), "smoke dispatch dir mode");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    let tachi_home = std::env::var("TACHI_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").expect("HOME set by TempHomeGuard"))
                .join(".tachi")
        });
    let run_dir = tachi_home.join("runs").join(dispatch_id);
    assert!(run_dir.exists(), "run dir should exist: {run_dir:?}");
    let mode = std::fs::metadata(&run_dir)
        .expect("run dir metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "dispatch run dir should be restricted to owner"
    );
}
