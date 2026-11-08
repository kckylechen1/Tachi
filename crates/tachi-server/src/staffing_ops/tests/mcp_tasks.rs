use super::*;
use rmcp::model::{CallToolRequestParams, CallToolResponse, TaskPayload, TaskStatus};

/// Bind through the production launch path, then read the same worker's result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn real_staff_launch_persists_result_owner_before_returning_acceptance() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let worktrees = tempfile::tempdir().unwrap();
    write_fake_worker(bin.path(), 0);
    std::fs::rename(bin.path().join("codex"), bin.path().join("opencode")).unwrap();
    let worker = bin.path().join("opencode");
    let script = std::fs::read_to_string(&worker).unwrap();
    let marker = "printf 'staff fake worker\\n'";
    assert!(script.contains(marker));
    std::fs::write(
        &worker,
        script.replace(marker, &format!("{marker}\npwd -P\nprintf 'accepted implementation\\n' > staff-result.txt\ngit add staff-result.txt && git commit -m 'staff fixture result' >/dev/null && git push origin HEAD >/dev/null || exit 97")),
    )
    .unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.path().to_path_buf())
            .chain(std::env::split_paths("/usr/bin:/bin:/usr/sbin:/sbin")),
    )
    .unwrap();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs =
        crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &home.path().join("runs"));
    let _path = crate::test_support::EnvRestore::set_os("PATH", &path);
    let _git = isolate_staff_repository_environment();
    let _registry = crate::test_support::EnvRestore::set_path("HOME", home.path());
    let _worktrees =
        crate::test_support::EnvRestore::set_path("TACHI_WORKTREES_ROOT", worktrees.path());
    let _v2 = crate::test_support::EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = crate::test_support::EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let _transport = crate::test_support::EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "cli");
    let repository = StaffRepositoryFixture::new(home.path());
    let _cwd = CurrentDirGuard::set(&repository.repo);
    let _fixture_cleanup = CertifiedWorktreeCleanupGuard::arm(worktrees.path(), find_worktree);
    let server = test_server();
    // Resolve the repository's stable name before the first open stamps its role.
    let (project, _) = server
        .resolve_server_named_project_binding("tachi")
        .unwrap();
    server
        .with_named_project_store(&project, |_| Ok(()))
        .unwrap();
    server
        .with_named_project_store_read_identity_checked(&project, |_| Ok(()))
        .expect("the task owner's canonical project must be readable before launch");
    server.set_session_identity(
        Some("mcp-task-lifecycle-test".into()),
        Some(project.clone()),
        Some(tachi_hub::ToolProfile::coordinate()),
    );
    crate::claims_ops::admit_agent_connection(&server, Some("agent.task-owner".into()), true)
        .unwrap();
    let mut request = staff_request(&project);
    request.project = None;
    request.profile = Some("glm_impl".into());
    request.worker = Some("custom".into());
    request.declared_file_scope = Some(vec!["workspace".into(), "gitdir".into()]);
    let flow_id = request.flow_id.clone().expect("fixture has a flow");
    let raw = staff_start(&server, request).await.unwrap();
    let mut cleanup = StaffCleanupGuard::arm(&raw);
    let release = StaffWorkerReleaseGuard(bin.path().join("opencode.release"));
    let accepted: Value = serde_json::from_str(&raw).unwrap();
    let dispatch_id = accepted["dispatch_id"].as_str().unwrap();
    repository.assert_dispatch_source(&server, dispatch_id);
    let lease = server
        .with_global_store_read(|store| {
            memcore::list_exec_envs(store.connection(), None).map_err(|error| error.to_string())
        })
        .unwrap()
        .into_iter()
        .find(|lease| lease.dispatch_id.as_deref() == Some(dispatch_id))
        .expect("bound start provisions a project-owned worktree");
    let worker_cwd = std::fs::canonicalize(&lease.path).unwrap();
    assert_ne!(worker_cwd, std::fs::canonicalize(&repository.repo).unwrap());
    let run = dispatch_runs_root().join(dispatch_id);
    let owner: Value =
        serde_json::from_slice(&std::fs::read(run.join("mcp-tasks.json")).unwrap()).unwrap();
    assert_eq!(owner["owner"]["agent_identity_id"], "agent.task-owner");
    assert_eq!(owner["owner"]["canonical_project"], project);
    assert!(
        owner["result"].is_null(),
        "start is acceptance, not a deferred result call"
    );
    let request = CallToolRequestParams::new("tachi_staff").with_arguments(
        serde_json::json!({"action":"result","dispatch_id":dispatch_id,"format":"json"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let CallToolResponse::Task(task) = crate::mcp_tasks::begin_result(&server, &request).unwrap()
    else {
        panic!("running worker must have a deferred result");
    };
    assert_eq!(task.task.status, TaskStatus::Working);
    let CallToolResponse::Task(repeated) =
        crate::mcp_tasks::begin_result(&server, &request).unwrap()
    else {
        panic!("repeated result remains deferred");
    };
    assert_eq!(task.task.task_id, repeated.task.task_id);
    let entries: std::collections::BTreeSet<_> = std::fs::read_dir(dispatch_runs_root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        [flow_id, dispatch_id.to_owned()].into_iter().collect()
    );
    // An explicit claim references this admitted run; it does not grant run
    // ownership or change the legacy flow's producer contract.
    let claim_params = serde_json::from_value(serde_json::json!({
        "action":"claim", "dispatch_id":dispatch_id, "issue_ref":"org/repo#1693",
        "claim_mode":"read_only", "claim_role":"observer", "claim_scope":["**"],
        "expected_head":"1234567", "lease_expires_at":"2035-01-01T00:00:00Z"
    }))
    .unwrap();
    crate::claims_ops::handle_task_claim(&server, &claim_params).unwrap();
    let mut spawned = false;
    for _ in 0..100 {
        let roster =
            crate::staffing_ops::presence::roster(&server, Some(&project), None, 100).unwrap();
        spawned = roster["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["dispatch_id"] == dispatch_id && row["state"] == "busy");
        if spawned {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        spawned,
        "compare a launched worker while its release barrier is held"
    );
    let before = assert_shared_project_run(&server, dispatch_id).await;
    assert_eq!(before["execution_state"], "running");
    drop(release);
    let (status, report) = wait_for_staff_terminal(&run).await;
    wait_for_staff_cleanup(dispatch_id).await;
    cleanup.disarm();
    assert_eq!(terminal_staff_state(&status), "TASK_STATE_COMPLETED");
    let task = crate::mcp_tasks::get(&server, &task.task.task_id).unwrap();
    assert_eq!(task.task.status(), TaskStatus::Completed);
    let TaskPayload::Completed { result } = task.task.payload else {
        panic!("completed task must contain the original tool result");
    };
    let response: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(response["result"]["body"], report);
    assert_eq!(response["dispatch_id"], dispatch_id);
    let after = assert_shared_project_run(&server, dispatch_id).await;
    assert_eq!(after["state"], response["state"]);
    assert_eq!(after["status_revision"], response["status_revision"]);
    assert_eq!(
        after["execution_state"],
        response["read_projection"]["execution_state"]
    );
    assert_ne!(before["receipt_digest"], after["receipt_digest"]);
    assert_eq!(result["resultType"], "complete");
    assert!(report.contains("staff fake worker"));
    assert!(
        report
            .lines()
            .any(|line| line == worker_cwd.to_str().unwrap()),
        "the actual worker runs in its bound project's managed worktree: {report}"
    );
    assert_eq!(status["env_id"], lease.env_id);
    assert_eq!(status["exec_env_postflight"]["verdict"], "clean");
    assert_eq!(status["exec_env_postflight"]["artifacts"], "released");
    assert!(
        worker_cwd.is_dir(),
        "write-authority worktree remains available for acceptance"
    );
    assert_eq!(
        std::fs::read_to_string(worker_cwd.join("staff-result.txt")).unwrap(),
        "accepted implementation\n"
    );
    assert_eq!(
        repository.git(&["show", &format!("{}:staff-result.txt", lease.branch)]),
        "accepted implementation\n"
    );
    assert!(
        !repository
            .git(&[
                "ls-remote",
                "origin",
                &format!("refs/heads/{}", lease.branch)
            ])
            .trim()
            .is_empty(),
        "automatic completion must preserve the submitted implementation branch"
    );
    assert_eq!(
        route_decisions_count(&server),
        1,
        "one acceptance launches one work lifecycle"
    );
}

async fn assert_shared_project_run(server: &MemoryServer, dispatch_id: &str) -> Value {
    use rmcp::handler::server::wrapper::Parameters;
    let mut views = Vec::new();
    for action in ["board", "status", "brief"] {
        let raw = server
            .tachi_task(Parameters(
                serde_json::from_value(serde_json::json!({
                    "action":action,"issue_ref":"org/repo#1693","format":"json"
                }))
                .unwrap(),
            ))
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let section = value
            .get("work_read_model")
            .or_else(|| value.pointer("/cycle/work_read_model"))
            .unwrap();
        let mut item = section["items"][0].clone();
        item.as_object_mut().unwrap().remove("read_at");
        assert_eq!(item["status"]["success_shaped"], false);
        assert_eq!(
            section["sources"]["run_receipts"]["state"], "available",
            "{section}"
        );
        // This worker exits without tachi_complete. The watchdog records
        // execution completion, but does not synthesize a result delivery.
        assert_eq!(section["sources"]["delivery"]["state"], "unavailable");
        assert_eq!(item["delivery_intents"][0]["availability"], "unavailable");
        views.push(item);
    }
    assert_eq!(views[0], views[1]);
    assert_eq!(views[1], views[2]);
    let row = views[0]["run_receipts"][0].clone();
    let raw = server
        .tachi_staff(Parameters(
            serde_json::from_value(serde_json::json!({
                "action":"status","dispatch_id":dispatch_id,"format":"json"
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let staff: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(row["state"], staff["state"]);
    assert_eq!(row["status_revision"], staff["status_revision"]);
    assert_eq!(
        row["execution_state"],
        staff["read_projection"]["execution_state"]
    );
    row
}
