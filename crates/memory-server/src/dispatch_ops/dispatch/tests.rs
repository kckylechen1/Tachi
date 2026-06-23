use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn generate_mcp_config_sets_owner_only_permissions() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let server = crate::tests::make_server();
    let path = generate_mcp_config(&server, "test-perms", true, false, None, &[])
        .await
        .expect("generate mcp config")
        .expect("config path");

    assert!(path.exists());
    let temp_leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("config parent"))
        .expect("read config parent")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("dispatch-test-perms-mcp.json.tmp."))
        .collect();
    assert!(
        temp_leftovers.is_empty(),
        "MCP config atomic write should not leave temp files: {temp_leftovers:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "MCP config mode should be 0o600, got {:#o}",
            mode
        );
    }

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn mcp_cleanup_removes_temp_config_on_drop() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let path = temp_home.path().join("dispatch-test-mcp.json");
    std::fs::write(&path, b"{}").expect("write temp config");
    assert!(path.exists());
    {
        let _cleanup = McpCleanup(Some(path.clone()));
    }
    assert!(!path.exists(), "MCP config should be removed on drop");
}

#[test]
fn dispatch_runs_root_uses_canonical_tachi_home_aliases() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let sigil_home = tempfile::tempdir().expect("sigil home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    let original_sigil_home = std::env::var_os("SIGIL_HOME");
    let original_app_home = std::env::var_os("TACHI_APP_HOME");

    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");
    std::env::set_var("SIGIL_HOME", sigil_home.path());
    std::env::remove_var("TACHI_APP_HOME");
    assert_eq!(dispatch_runs_root(), sigil_home.path().join("runs"));

    std::env::remove_var("SIGIL_HOME");
    std::env::set_var("TACHI_APP_HOME", "~/custom-tachi");
    assert_eq!(
        dispatch_runs_root(),
        temp_home.path().join("custom-tachi").join("runs")
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
    if let Some(value) = original_sigil_home {
        std::env::set_var("SIGIL_HOME", value);
    } else {
        std::env::remove_var("SIGIL_HOME");
    }
    if let Some(value) = original_app_home {
        std::env::set_var("TACHI_APP_HOME", value);
    } else {
        std::env::remove_var("TACHI_APP_HOME");
    }
}

#[test]
fn recover_orphaned_dispatch_runs_marks_working_runs_failed() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let run_dir = dispatch_runs_root().join("20260614T000000Z-claude-deadbeef");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": "20260614T000000Z-claude-deadbeef",
            "state": "TASK_STATE_WORKING",
            "agent": "claude",
        })
        .to_string(),
    )
    .expect("status");

    let recovered = recover_orphaned_dispatch_runs();
    assert_eq!(
        recovered,
        vec!["20260614T000000Z-claude-deadbeef".to_string()]
    );

    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["state"], "TASK_STATE_FAILED");
    assert_eq!(status["recovery_reason"], "daemon_restart_orphan_recovery");

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn global_dispatch_slot_blocks_duplicate_active_task_without_flow_id() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let first = reserve_global_dispatch_slot("same task without flow", "dispatch-one")
        .expect("first global reserve");
    let duplicate = reserve_global_dispatch_slot("same task without flow", "dispatch-two")
        .expect_err("duplicate active task should be blocked without flow_id");
    assert!(
        duplicate.contains("duplicate dispatch blocked"),
        "unexpected error: {duplicate}"
    );

    release_flow_dispatch_slot(Some(first));
    assert!(
        reserve_global_dispatch_slot("same task without flow", "dispatch-three").is_ok(),
        "slot should be reusable after release"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn flow_dispatch_slot_blocks_duplicate_active_task() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let flow_id = format!(
        "flow_20260610T000000Z_duplicate_slot_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = crate::shell_ops::run_dir_for_flow_id(&flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow dir");

    let first = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-one")
        .expect("first reserve")
        .expect("slot path");
    let duplicate = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-two")
        .expect_err("duplicate active task should be blocked");
    assert!(
        duplicate.contains("duplicate dispatch blocked"),
        "unexpected error: {duplicate}"
    );

    release_flow_dispatch_slot(Some(first));
    assert!(
        reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-three")
            .expect("reserve after release")
            .is_some(),
        "slot should be reusable after release"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn flow_dispatch_slot_reclaims_stale_lock_when_run_status_is_missing() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runs_root = tempfile::tempdir().expect("temp runs root");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", runs_root.path());

    let flow_id = format!(
        "flow_20260610T000001Z_stale_slot_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let task = "same task";
    let old_dispatch_id = "dispatch-stale-lock";
    let new_dispatch_id = "dispatch-new-lock";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(&flow_id).expect("flow run dir");
    let lock_dir = run_dir.join(".dispatch-dedupe");
    std::fs::create_dir_all(&lock_dir).expect("create lock dir");
    let task_hash = crate::utils::stable_hash(task);
    let stale_created_at =
        (Utc::now() - chrono::Duration::seconds(DISPATCH_DEDUPE_STALE_LOCK_SECS + 1)).to_rfc3339();
    crate::utils::write_owner_only_file_atomic(
        &lock_dir.join(format!("{task_hash}.json")),
        serde_json::to_vec_pretty(&json!({
            "scope": "flow",
            "task_hash": task_hash,
            "dispatch_id": old_dispatch_id,
            "task": task,
            "flow_id": flow_id,
            "created_at": stale_created_at,
        }))
        .expect("serialize stale lock")
        .as_slice(),
    )
    .expect("write stale lock");

    let reserved = reserve_flow_dispatch_slot(Some(&flow_id), task, new_dispatch_id)
        .expect("stale lock should be reclaimed")
        .expect("slot path");
    let lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reserved).expect("read lock"))
            .expect("parse lock");
    assert_eq!(lock["dispatch_id"], json!(new_dispatch_id));

    release_flow_dispatch_slot(Some(reserved));
    if let Some(value) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", value);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
