use super::*;
use rmcp::handler::server::wrapper::Parameters;
use std::path::PathBuf;

const PROJECT: &str = "mcp-task-owner-project";
const DISPATCH: &str = "20261030T100000Z-test-owner";

fn bind_identity(server: &MemoryServer, identity: &str) {
    server.set_session_identity(
        Some("task-tests".into()),
        Some(PROJECT.into()),
        Some(tachi_hub::ToolProfile::coordinate()),
    );
    crate::claims_ops::admit_agent_connection(server, Some(identity.into()), true).unwrap();
}

fn fixture() -> (
    tempfile::TempDir,
    crate::test_support::EnvRestore,
    crate::tests::TestServer,
    PathBuf,
) {
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let env = crate::test_support::EnvRestore::set_path("TACHI_HOME", runs.path());
    bind_identity(&server, "agent.owner");
    let run = runs.path().join("runs").join(DISPATCH);
    std::fs::create_dir_all(&run).unwrap();
    prepare_run_binding(&server, &run, DISPATCH, Some(PROJECT)).unwrap();
    write_status(&run, "TASK_STATE_WORKING", 1);
    (runs, env, server, run)
}

fn write_status(run: &Path, state: &str, revision: u64) {
    let anchor = AnchoredRunStatus::open(run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    anchor
        .write_atomic(
            &serde_json::to_vec(&json!({
                "dispatch_id":DISPATCH,"state":state,"status_revision":revision,
                "updated_at":Utc::now().to_rfc3339(), "project":PROJECT,
            }))
            .unwrap(),
            &fence,
        )
        .unwrap();
}

fn request() -> CallToolRequestParams {
    CallToolRequestParams::new("tachi_staff").with_arguments(
        json!({"action":"result","dispatch_id":DISPATCH,"format":"json"})
            .as_object()
            .unwrap()
            .clone(),
    )
}

fn start(server: &MemoryServer) -> String {
    let CallToolResponse::Task(task) = begin_result(server, &request()).unwrap() else {
        panic!("expected deferred result");
    };
    assert_eq!(task.task.status, TaskStatus::Working);
    assert!(
        DateTime::parse_from_rfc3339(&task.task.last_updated_at).unwrap()
            >= DateTime::parse_from_rfc3339(&task.task.created_at).unwrap()
    );
    task.task.task_id
}

#[test]
fn binding_is_durable_idempotent_and_owner_bound_before_publication() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let before = std::fs::read(run.join("status.json")).unwrap();
    let id = start(&server);
    assert_eq!(
        start(&server),
        id,
        "repeated result requests must not create work or more handles"
    );
    assert_eq!(
        get(&server, &id).unwrap().task.status(),
        TaskStatus::Working
    );
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
    let global = server.global_db_path_buf();
    let home = global.parent().unwrap().parent().unwrap().join("home");
    let project = server
        .resolve_server_named_project_db_path(PROJECT)
        .unwrap();
    let restarted = MemoryServer::new_with_home_for_test(global, Some(project), home).unwrap();
    assert_ne!(server.controller_epoch, restarted.controller_epoch);
    bind_identity(&restarted, "agent.owner");
    assert_eq!(get(&restarted, &id).unwrap().task.task.task_id, id);
    assert_eq!(start(&restarted), id);
    assert_eq!(std::fs::read_dir(run.parent().unwrap()).unwrap().count(), 1);
    bind_identity(&restarted, "agent.intruder");
    let denied = get(&restarted, &id).unwrap_err();
    assert_eq!(denied, get(&restarted, "not-a-task").unwrap_err());
    assert_eq!(denied, update(&restarted, &id).unwrap_err());
    assert_eq!(denied, begin_result(&restarted, &request()).unwrap_err());
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn terminal_result_matches_legacy_and_never_regresses() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    std::fs::write(run.join("result.md"), "Result 中文\n").unwrap();
    write_status(&run, "TASK_STATE_FAILED", 2);
    let result = get(&server, &id).unwrap();
    assert_eq!(
        result.task.status(),
        TaskStatus::Completed,
        "business failure is not JSON-RPC failure"
    );
    let TaskPayload::Completed { result: body } = &result.task.payload else {
        panic!("result");
    };
    let legacy = server
        .tachi_staff(Parameters(
            serde_json::from_value(Value::Object(request().arguments.unwrap())).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(body["content"][0]["text"], legacy);
    let content: Value =
        serde_json::from_str(body["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(content["state"], "TASK_STATE_FAILED");
    assert_eq!(content["result"]["body"], "Result 中文\n");
    write_status(&run, "TASK_STATE_WORKING", 1);
    std::fs::write(run.join("result.md"), "later bytes").unwrap();
    assert_eq!(
        get(&server, &id).unwrap(),
        result,
        "final request result is immutable across receipt drift"
    );
}

#[test]
fn tool_error_is_completed_and_report_alias_is_never_followed() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "PRIVATE_REPORT_SENTINEL").unwrap();
    std::os::unix::fs::symlink(outside.path(), run.join("result.md")).unwrap();
    write_status(&run, "TASK_STATE_COMPLETED", 2);
    let response = serde_json::to_value(get(&server, &id).unwrap()).unwrap();
    assert_eq!(response["status"], "completed");
    assert_eq!(response["result"]["isError"], true);
    assert!(!response.to_string().contains("PRIVATE_REPORT_SENTINEL"));
}

#[test]
fn unknown_input_and_expired_task_never_mutate_or_remove_work() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let sidecar = run.join("mcp-tasks.json");
    let original = std::fs::read(&sidecar).unwrap();
    let status = std::fs::read(run.join("status.json")).unwrap();
    update(&server, &id).unwrap();
    update(&server, &id).unwrap();
    assert_eq!(std::fs::read(&sidecar).unwrap(), original);
    let mut binding: RunBinding = serde_json::from_slice(&original).unwrap();
    binding.result.as_mut().unwrap().created_at = "2020-01-01T00:00:00Z".into();
    std::fs::write(&sidecar, serde_json::to_vec(&binding).unwrap()).unwrap();
    assert!(get(&server, &id).is_err());
    assert!(update(&server, &id).is_err());
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), status);
    assert_ne!(
        start(&server),
        id,
        "expiry may renew the protocol request, never the worker"
    );
}

#[test]
fn missing_binding_project_change_and_narrow_profile_fail_closed() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let clone = server.clone_for_mcp_session();
    crate::claims_ops::admit_agent_connection(&clone, Some("agent.owner".into()), true).unwrap();
    clone.set_session_identity(None, None, Some(tachi_hub::ToolProfile::coordinate()));
    assert_eq!(get(&clone, &id).unwrap_err(), unavailable());
    clone.set_session_identity(
        None,
        Some(PROJECT.into()),
        Some(tachi_hub::ToolProfile::remember()),
    );
    assert_eq!(get(&clone, &id).unwrap_err(), unavailable());
    std::fs::remove_file(run.join("mcp-tasks.json")).unwrap();
    assert_eq!(
        begin_result(&server, &request()).unwrap_err(),
        unavailable()
    );
    assert_eq!(get(&server, &id).unwrap_err(), unavailable());
}

#[test]
fn status_regression_and_private_binding_alias_refuse_without_rebinding() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    write_status(&run, "TASK_STATE_WORKING", 3);
    get(&server, &id).unwrap();
    write_status(&run, "TASK_STATE_WORKING", 2);
    assert_eq!(get(&server, &id).unwrap_err(), unavailable());
    assert!(prepare_run_binding(&server, &run, DISPATCH, Some(PROJECT)).is_err());
    let private = run.join("mcp-tasks.json");
    let alias = run.join("foreign-binding.json");
    std::fs::rename(&private, &alias).unwrap();
    std::os::unix::fs::symlink(&alias, &private).unwrap();
    assert_eq!(get(&server, &id).unwrap_err(), unavailable());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn orphan_cancel_never_fabricates_stopped_or_changes_work() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let before = std::fs::read(run.join("status.json")).unwrap();
    assert!(cancel(&server, &id)
        .await
        .unwrap_err()
        .message
        .contains("no current controller"));
    assert_eq!(
        get(&server, &id).unwrap().task.status(),
        TaskStatus::Working
    );
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
    bind_identity(&server, "agent.intruder");
    assert_eq!(cancel(&server, &id).await.unwrap_err(), unavailable());
}

#[test]
fn terminal_at_initial_call_remains_an_immediate_result() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    write_status(&run, "TASK_STATE_COMPLETED", 2);
    let before = std::fs::read(run.join("mcp-tasks.json")).unwrap();
    assert!(matches!(
        begin_result(&server, &request()).unwrap(),
        CallToolResponse::Complete(_)
    ));
    assert_eq!(std::fs::read(run.join("mcp-tasks.json")).unwrap(), before);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancellation_ack_is_only_intent_until_the_existing_owner_confirms() {
    use crate::managed_run_control::CancelCompletion;
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let mut status: Value =
        serde_json::from_slice(&std::fs::read(run.join("status.json")).unwrap()).unwrap();
    status["execution_classification"] = json!("managed_custom");
    status["lifecycle_owner"] = json!("memory_server_managed_custom");
    std::fs::write(
        run.join("status.json"),
        serde_json::to_vec(&status).unwrap(),
    )
    .unwrap();
    let (mut receiver, _controller) = server.managed_run_controls.register(DISPATCH).unwrap();
    server
        .managed_run_controls
        .retain_accepted_status_anchor(DISPATCH, AnchoredRunStatus::open(&run).unwrap())
        .unwrap();
    let pending = cancel(&server, &id);
    tokio::pin!(pending);
    let command = tokio::select! {
        result = &mut pending => panic!("cancellation returned before its controller: {result:?}"),
        command = receiver.recv() => command.unwrap(),
    };
    assert_eq!(command.expected_status_revision, 1);
    assert_eq!(
        get(&server, &id).unwrap().task.status(),
        TaskStatus::Working
    );
    let intent = std::fs::read(run.join("status.json")).unwrap();
    cancel(&server, &id).await.unwrap();
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), intent);
    assert!(
        receiver.try_recv().is_err(),
        "duplicate cancel must not send another command"
    );
    command
        .response
        .send(CancelCompletion::Unconfirmed)
        .ok()
        .unwrap();
    pending.await.unwrap();
    assert_eq!(
        get(&server, &id).unwrap().task.status(),
        TaskStatus::Working
    );
    crate::managed_run_control::confirm_managed_custom_cancellation(
        &run,
        1,
        "test_owner_confirmation",
    )
    .unwrap();
    let final_result = get(&server, &id).unwrap();
    assert_eq!(final_result.task.status(), TaskStatus::Cancelled);
    let before = std::fs::read(run.join("status.json")).unwrap();
    cancel(&server, &id).await.unwrap();
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
    assert_eq!(get(&server, &id).unwrap(), final_result);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn completion_winner_and_unrelated_cancellation_are_completed_results() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    write_status(&run, "TASK_STATE_COMPLETED", 2);
    let before = std::fs::read(run.join("status.json")).unwrap();
    cancel(&server, &id).await.unwrap();
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
    assert_eq!(
        get(&server, &id).unwrap().task.status(),
        TaskStatus::Completed
    );

    let sidecar = run.join("mcp-tasks.json");
    let mut binding: RunBinding =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    binding.result.as_mut().unwrap().terminal = None;
    std::fs::write(&sidecar, serde_json::to_vec(&binding).unwrap()).unwrap();
    write_status(&run, "TASK_STATE_CANCELED", 3);
    assert_eq!(
        get(&server, &id).unwrap().task.status(),
        TaskStatus::Completed,
        "work cancelled outside this request is still a successfully retrieved result"
    );
}

#[test]
fn special_status_file_is_refused_without_a_blocking_open() {
    use std::os::unix::ffi::OsStrExt;
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let status = run.join("status.json");
    std::fs::remove_file(&status).unwrap();
    let name = std::ffi::CString::new(status.as_os_str().as_bytes()).unwrap();
    // SAFETY: the private fixture path is NUL terminated and has no live file.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(get(&server, &id).is_err());
}

#[test]
fn oversized_binding_write_preserves_the_published_handle() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let id = start(&server);
    let original = std::fs::read(run.join("mcp-tasks.json")).unwrap();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    {
        let lock = anchor.lock();
        let _guard = lock.lock().unwrap();
        let fence = anchor.acquire_fence().unwrap();
        assert!(anchor
            .write_mcp_binding(&vec![b'x'; 1024 * 1024 + 1], &fence)
            .is_err());
    }
    assert_eq!(std::fs::read(run.join("mcp-tasks.json")).unwrap(), original);
    assert_eq!(get(&server, &id).unwrap().task.task.task_id, id);
}
