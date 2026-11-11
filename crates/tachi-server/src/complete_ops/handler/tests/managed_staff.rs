//! Completion/terminal race regressions use the retained canonical kernel.
//! The public native Staff facade is validated independently.

use super::*;
use crate::staffing_ops::tests::{
    find_worktree, isolate_staff_repository_environment, CertifiedWorktreeCleanupGuard,
    CurrentDirGuard, StaffCleanupGuard, StaffRepositoryFixture, StaffWorkerReleaseGuard,
};

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn issue_1825_background_terminal_supersedes_failed_completion_admission() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temporary Tachi home");
    let temp_runs = tempfile::tempdir().expect("temporary run root");
    let temp_bin = tempfile::tempdir().expect("temporary worker bin");
    let exit_trigger = temp_bin.path().join("exit-trigger");
    let worker = temp_bin.path().join("opencode");
    std::fs::write(
        &worker,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nwhile test ! -e '{}'; do sleep 0.01; done\nprintf 'background terminal after admitted completion\\n'\nexit 2\n",
            exit_trigger.display()
        ),
    )
    .expect("write controlled managed worker");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&worker)
        .expect("controlled worker metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&worker, permissions).expect("make controlled worker executable");
    let old_path = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
    let joined_path = std::env::join_paths(
        std::iter::once(temp_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .expect("join controlled worker PATH");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let _path = crate::test_support::EnvRestore::set_os("PATH", &joined_path);
    let _git_environment = isolate_staff_repository_environment();
    let _registry_home = crate::test_support::EnvRestore::set_path("HOME", temp_home.path());
    let repository = StaffRepositoryFixture::new(temp_home.path());
    let _cwd = CurrentDirGuard::set(&repository.repo);
    let temp_worktrees = tempfile::tempdir().expect("isolated Staff worktrees");
    let _worktrees =
        crate::test_support::EnvRestore::set_path("TACHI_WORKTREES_ROOT", temp_worktrees.path());
    let _fixture_cleanup = CertifiedWorktreeCleanupGuard::arm(temp_worktrees.path(), find_worktree);
    let _transport = crate::test_support::EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "cli");
    let _v2 = crate::test_support::EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = crate::test_support::EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let _capture_gate = CaptureGateEnforceGuard::new();
    let server = crate::staffing_ops::tests::test_server();
    let (project, _) = server
        .resolve_server_named_project_binding("tachi")
        .expect("resolve the registered fixture project before initializing its store");
    server
        .with_named_project_store(&project, |_| Ok(()))
        .expect("initialize registered fixture project store");
    let raw = crate::staffing_ops::tests::kernel_start(
        &server,
        crate::staffing_ops::StaffStartRequest {
            submission_id: None,
            task: "interleave real managed terminal with completion rollback".to_string(),
            staffing_reason: tachi_params::TachiDispatchReason::DurableCrossSession,
            profile: Some("kernel_opencode_write".to_string()),
            worker: Some("custom".to_string()),
            project: Some(project.clone()),
            stage: None,
            execution_level: None,
            issue_ref: Some("kckylechen1/tachi#1825".to_string()),
            pr_ref: None,
            flow_id: Some("flow_1825_admission_terminal_interleave".to_string()),
            completion_predicate: None,
            recommendation_ref: None,
            declared_file_scope: Some(Vec::new()),
        },
    )
    .await
    .expect("real Staff background launch");
    let _cleanup_guard = StaffCleanupGuard::arm(&raw);
    let _release_worker = StaffWorkerReleaseGuard(exit_trigger.clone());
    let response: Value = serde_json::from_str(&raw).expect("Staff response JSON");
    let dispatch_id = response["dispatch_id"]
        .as_str()
        .expect("managed dispatch id")
        .to_string();
    repository.assert_dispatch_source(&server, &dispatch_id);
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(&dispatch_id);
    let status_path = run_dir.join("status.json");
    let (_barrier_guard, entered, release) =
        install_managed_completion_admission_barrier(&dispatch_id);
    let handler_server = server.clone();
    let handler_dispatch_id = dispatch_id.clone();
    let completion = tokio::spawn(async move {
        handle_tachi_complete(
            &handler_server,
            managed_completion_params(&handler_dispatch_id),
            false,
        )
        .await
    });
    tokio::task::spawn_blocking(move || entered.recv().expect("handler admission barrier"))
        .await
        .expect("admission barrier join");
    std::fs::write(&exit_trigger, b"release background terminal")
        .expect("release controlled worker");
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if std::fs::read_to_string(run_dir.join("result.md")).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "real background runner must persist its result while waiting for admission: {}",
            std::fs::read_to_string(&status_path)
                .unwrap_or_else(|error| format!("<status unavailable: {error}>"))
        )
    });
    let fenced: Value = serde_json::from_slice(
        &std::fs::read(&status_path).expect("read admitted status before handler release"),
    )
    .expect("parse admitted status before handler release");
    assert_eq!(fenced["state"], "TASK_STATE_WORKING");
    assert_eq!(
        fenced["completion_recovery"]["status"], "completion_admitted",
        "the background finalizer must retain ownership while the handler admission lease is live"
    );
    assert!(server.managed_run_controls.contains(&dispatch_id));
    release.send(()).expect("release failed completion handler");
    assert_eq!(
        completion
            .await
            .expect("completion handler join")
            .expect_err("capture gate rejects completion after terminalization"),
        "completion eval was not durably recorded"
    );
    let terminal = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(raw_status) = std::fs::read_to_string(&status_path) {
                if let Ok(status) = serde_json::from_str::<Value>(&raw_status) {
                    if status["state"] == "TASK_STATE_FAILED" {
                        return status;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background finalizer must terminalize after admission rollback");
    for _ in 0..360 {
        if !server.managed_run_controls.contains(&dispatch_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let after: Value = serde_json::from_slice(
        &std::fs::read(&status_path).expect("read terminal status after rollback"),
    )
    .expect("parse terminal status after rollback");
    assert_eq!(terminal["state"], "TASK_STATE_FAILED");
    assert_eq!(after["state"], "TASK_STATE_FAILED");
    assert!(after.get("completion_recovery").is_none());
    assert!(std::fs::read_to_string(run_dir.join("result.md"))
        .expect("terminal result")
        .contains("background terminal after admitted completion"));
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, &dispatch_id).await,
        Some("TASK_STATE_FAILED".to_string())
    );
    let outcomes: i64 = server
        .with_named_project_store_read(&project, |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [&dispatch_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count terminal failure outcomes");
    assert_eq!(outcomes, 1);
    assert!(!server.managed_run_controls.contains(&dispatch_id));
    let flow_slot =
        crate::task_lifecycle::run_dir_for_flow_id("flow_1825_admission_terminal_interleave")
            .expect("flow run directory")
            .join(".dispatch-dedupe");
    assert!(
        !flow_slot.exists()
            || std::fs::read_dir(&flow_slot)
                .expect("flow slot directory")
                .next()
                .is_none(),
        "terminal background owner must release the flow slot"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn issue_1825_committed_completion_admission_makes_cancel_unavailable_without_signaling_and_can_finalize(
) {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temporary Tachi home");
    let temp_runs = tempfile::tempdir().expect("temporary run root");
    let temp_bin = tempfile::tempdir().expect("temporary worker bin");
    let exit_trigger = temp_bin.path().join("exit-trigger");
    let worker_started = temp_bin.path().join("worker-started");
    let worker = temp_bin.path().join("opencode");
    std::fs::write(
        &worker,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\ntouch '{}'\nwhile test ! -e '{}'; do sleep 0.01; done\nprintf 'background terminal held for successful completion\\n'\nexit 2\n",
            worker_started.display(),
            exit_trigger.display(),
        ),
    )
    .expect("write controlled managed worker");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&worker)
        .expect("controlled worker metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&worker, permissions).expect("make controlled worker executable");
    let old_path = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
    let joined_path = std::env::join_paths(
        std::iter::once(temp_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .expect("join controlled worker PATH");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let _path = crate::test_support::EnvRestore::set_os("PATH", &joined_path);
    let _git_environment = isolate_staff_repository_environment();
    let _registry_home = crate::test_support::EnvRestore::set_path("HOME", temp_home.path());
    let repository = StaffRepositoryFixture::new(temp_home.path());
    let _cwd = CurrentDirGuard::set(&repository.repo);
    let temp_worktrees = tempfile::tempdir().expect("isolated Staff worktrees");
    let _worktrees =
        crate::test_support::EnvRestore::set_path("TACHI_WORKTREES_ROOT", temp_worktrees.path());
    let _fixture_cleanup = CertifiedWorktreeCleanupGuard::arm(temp_worktrees.path(), find_worktree);
    let _transport = crate::test_support::EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "cli");
    let _v2 = crate::test_support::EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = crate::test_support::EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let _embedding =
        crate::test_support::EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    std::fs::create_dir_all(repository.repo.join(".tachi/credentials"))
        .expect("create isolated credential profile directory");
    std::fs::write(
        repository
            .repo
            .join(".tachi/credentials/opencode-shared.json"),
        serde_json::json!({
            "credential_profiles": {
                "opencode_shared": {
                    "entries": {"auth_json": "OPENCODE_SHARED_AUTH_JSON"},
                    "allowed_consumers": {
                        "agents": ["opencode"],
                        "profiles": ["kernel_opencode_credentials"]
                    },
                    "materializers": [{
                        "type": "env",
                        "source": "auth_json",
                        "target": "OPENCODE_SHARED_AUTH_JSON"
                    }, {
                        "type": "config_overlay",
                        "source": "auth_json",
                        "target": "{credentials_dir}/opencode.json",
                        "template": {
                            "provider": {
                                "fixture": {"apiKey": "{env:OPENCODE_SHARED_AUTH_JSON}"}
                            }
                        }
                    }]
                }
            }
        })
        .to_string(),
    )
    .expect("write isolated OpenCode credential profile");
    let server = crate::staffing_ops::tests::test_server();
    let (project, _) = server
        .resolve_server_named_project_binding("tachi")
        .expect("resolve the registered fixture project before initializing its store");
    server
        .with_named_project_store(&project, |_| Ok(()))
        .expect("initialize registered fixture project store");
    crate::vault_test_api::seed_vault(&server, "issue-1825-opencode-fixture")
        .expect("initialize isolated credential vault");
    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "OPENCODE_SHARED_AUTH_JSON".to_string(),
            value: r#"{"token":"issue-1825-fixture"}"#.to_string(),
            secret_type: "api_key".to_string(),
            description: "issue-1825 successful completion credential fixture".to_string(),
            allowed_agents: None,
        },
    )
    .expect("seed isolated OpenCode credential secret");
    let raw = crate::staffing_ops::tests::kernel_start(
        &server,
        crate::staffing_ops::StaffStartRequest {
            submission_id: None,
            task: "interleave real managed terminal with successful completion".to_string(),
            staffing_reason: tachi_params::TachiDispatchReason::DurableCrossSession,
            profile: Some("kernel_opencode_credentials".to_string()),
            worker: Some("custom".to_string()),
            project: Some(project.clone()),
            stage: None,
            execution_level: None,
            issue_ref: Some("kckylechen1/tachi#1825".to_string()),
            pr_ref: None,
            flow_id: Some("flow_1825_admission_success_interleave".to_string()),
            completion_predicate: None,
            recommendation_ref: None,
            declared_file_scope: Some(Vec::new()),
        },
    )
    .await
    .expect("real Staff background launch");
    let _cleanup_guard = StaffCleanupGuard::arm(&raw);
    let _release_worker = StaffWorkerReleaseGuard(exit_trigger.clone());
    let response: Value = serde_json::from_str(&raw).expect("Staff response JSON");
    let dispatch_id = response["dispatch_id"]
        .as_str()
        .expect("managed dispatch id")
        .to_string();
    repository.assert_dispatch_source(&server, &dispatch_id);
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(&dispatch_id);
    let status_path = run_dir.join("status.json");
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if worker_started.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("real background root must acknowledge startup before cancellation");
    assert!(
        run_dir.join("credentials/opencode.json").exists(),
        "the managed completion fixture must use a real ephemeral OpenCode overlay"
    );
    let (_cancel_dequeue_guard, cancel_dequeued, continue_cancel, cancel_observation) =
        crate::dispatch_ops::install_managed_cancel_dequeue_barrier(&run_dir);
    let (_barrier_guard, entered, release) =
        install_managed_completion_admission_barrier(&dispatch_id);
    let handler_server = server.clone();
    let handler_dispatch_id = dispatch_id.clone();
    let completion = tokio::spawn(async move {
        handle_tachi_complete(
            &handler_server,
            managed_completion_params(&handler_dispatch_id),
            false,
        )
        .await
    });
    tokio::task::spawn_blocking(move || entered.recv().expect("handler admission barrier"))
        .await
        .expect("admission barrier join");
    let admitted: Value = serde_json::from_slice(
        &std::fs::read(&status_path).expect("read admitted status before cancellation"),
    )
    .expect("parse admitted status before cancellation");
    let expected_status_revision = admitted["status_revision"]
        .as_u64()
        .expect("committed admission status revision");
    let evals_before_completion = eval_memory_count(&server, &dispatch_id);
    let outcomes_before_completion = dispatch_outcome_count(&server, &dispatch_id);
    let adjudications_before_completion = dispatch_adjudication_count(&server, &dispatch_id);
    let cancel_server = server.clone();
    let cancel_dispatch_id = dispatch_id.clone();
    let cancellation_task = tokio::task::spawn(async move {
        crate::managed_run_control::request_managed_custom_cancel(
            &cancel_server,
            &cancel_dispatch_id,
            expected_status_revision,
        )
        .await
    });
    match tokio::task::spawn_blocking(move || {
        cancel_dequeued.recv_timeout(Duration::from_millis(250))
    })
    .await
    .expect("join cancellation dequeue observation")
    {
        Ok(()) => {
            continue_cancel
                .send(())
                .expect("release unexpectedly dequeued cancellation");
            let response = cancellation_task
                .await
                .expect("unexpected cancellation task join");
            panic!("completion admission must reject before the background owner dequeues cancellation: {response:?}");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("cancellation dequeue observer disconnected before the response")
        }
    }
    let cancellation: Value = serde_json::from_str(
        &cancellation_task
            .await
            .expect("cancellation task join")
            .expect("cancel after real completion admission"),
    )
    .expect("cancellation JSON");
    assert_eq!(cancellation["receipt"], "cancellation_unavailable");
    assert_eq!(cancellation["reason"], "terminal_or_recovery_state");
    assert!(
        matches!(
            cancel_observation.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "an unavailable cancellation must not reach the real background terminal owner"
    );
    assert!(
        admitted.get("cancellation").is_none(),
        "a cancellation rejected by the committed completion admission must not signal the child"
    );
    assert!(
        worker_started.exists() && !run_dir.join("result.md").exists(),
        "the acknowledged root must remain live until the explicit completion trigger"
    );
    let concurrent = handle_tachi_complete(&server, managed_completion_params(&dispatch_id), false)
        .await
        .expect_err("a second handler must not inherit the admitted owner");
    assert_eq!(concurrent, "managed completion already admitted");
    std::fs::write(&exit_trigger, b"release background terminal")
        .expect("release controlled worker");
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if std::fs::read_to_string(run_dir.join("result.md")).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background runner must finish before completion is released");
    let fenced: Value =
        serde_json::from_slice(&std::fs::read(&status_path).expect("read admitted status"))
            .expect("parse admitted status");
    assert_eq!(fenced["state"], "TASK_STATE_WORKING");
    assert_eq!(
        fenced["completion_recovery"]["status"],
        "completion_admitted"
    );
    release
        .send(())
        .expect("release successful completion handler");
    completion
        .await
        .expect("completion handler join")
        .expect("completion must win its owned admission");
    let terminal = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(raw_status) = std::fs::read_to_string(&status_path) {
                if let Ok(status) = serde_json::from_str::<Value>(&raw_status) {
                    if status["state"] == "TASK_STATE_COMPLETED"
                        && status.get("resolved_completion").is_some()
                    {
                        return status;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("completion receipt must be the sole terminal truth");
    assert!(terminal.get("completion_recovery").is_none());
    assert_eq!(
        eval_memory_count(&server, &dispatch_id),
        evals_before_completion + 1,
        "only the admitted handler may persist the canonical eval"
    );
    assert_eq!(
        dispatch_adjudication_count(&server, &dispatch_id),
        adjudications_before_completion,
        "successful completion and the rejected second handler must not fabricate an adjudication"
    );
    assert!(
        !run_dir.join("credentials/opencode.json").exists(),
        "a completion-owned nonzero managed terminal must clean its real overlay before release"
    );
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, &dispatch_id).await,
        Some("TASK_STATE_COMPLETED".to_string())
    );
    let outcomes: i64 = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [&dispatch_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count canonical completion outcomes");
    assert_eq!(
        outcomes,
        outcomes_before_completion + 1,
        "only the admitted handler may project the canonical outcome"
    );
    for _ in 0..360 {
        if !server.managed_run_controls.contains(&dispatch_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!server.managed_run_controls.contains(&dispatch_id));
    let flow_slot =
        crate::task_lifecycle::run_dir_for_flow_id("flow_1825_admission_success_interleave")
            .expect("flow run directory")
            .join(".dispatch-dedupe");
    assert!(
        !flow_slot.exists()
            || std::fs::read_dir(&flow_slot)
                .expect("flow slot directory")
                .next()
                .is_none(),
        "the terminal background owner must release the flow slot after completion wins"
    );
}
