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
        script.replace(marker, &format!("{marker}\npwd -P")),
    )
    .unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.path().to_path_buf())
            .chain(std::env::split_paths("/usr/bin:/bin:/usr/sbin:/sbin")),
    )
    .unwrap();
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
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
    let server = test_server();
    server
        .with_named_project_store("tachi", |_| Ok(()))
        .unwrap();
    server.set_session_identity(
        Some("mcp-task-lifecycle-test".into()),
        Some("tachi".into()),
        Some(tachi_hub::ToolProfile::coordinate()),
    );
    crate::claims_ops::admit_agent_connection(&server, Some("agent.task-owner".into()), true)
        .unwrap();
    let mut request = staff_request("tachi");
    request.profile = Some("glm_impl".into());
    request.worker = Some("custom".into());
    let raw = staff_start(&server, request).await.unwrap();
    let mut cleanup = StaffCleanupGuard::arm(&raw);
    let release = StaffWorkerReleaseGuard(bin.path().join("opencode.release"));
    let accepted: Value = serde_json::from_str(&raw).unwrap();
    let dispatch_id = accepted["dispatch_id"].as_str().unwrap();
    let run = dispatch_runs_root().join(dispatch_id);
    let owner: Value =
        serde_json::from_slice(&std::fs::read(run.join("mcp-tasks.json")).unwrap()).unwrap();
    assert_eq!(owner["owner"]["agent_identity_id"], "agent.task-owner");
    assert_eq!(owner["owner"]["canonical_project"], "tachi");
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
    assert_eq!(std::fs::read_dir(dispatch_runs_root()).unwrap().count(), 1);
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
    assert_eq!(result["resultType"], "complete");
    assert!(report.contains("staff fake worker"));
    assert!(
        report.contains(
            std::fs::canonicalize(&repository.repo)
                .unwrap()
                .to_str()
                .unwrap()
        ),
        "the actual worker runs in the isolated fixture repository: {report}"
    );
    assert_eq!(
        route_decisions_count(&server),
        1,
        "one acceptance launches one work lifecycle"
    );
}
