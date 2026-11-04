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
    assert_eq!(
        denied,
        update(&restarted, &id, &Default::default()).unwrap_err()
    );
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

#[cfg(unix)]
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
    update(&server, &id, &Default::default()).unwrap();
    update(&server, &id, &Default::default()).unwrap();
    assert_eq!(std::fs::read(&sidecar).unwrap(), original);
    let mut binding: RunBinding = serde_json::from_slice(&original).unwrap();
    binding.result.as_mut().unwrap().created_at = "2020-01-01T00:00:00Z".into();
    std::fs::write(&sidecar, serde_json::to_vec(&binding).unwrap()).unwrap();
    assert!(get(&server, &id).is_err());
    assert!(update(&server, &id, &Default::default()).is_err());
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), status);
    assert_ne!(
        start(&server),
        id,
        "expiry may renew the protocol request, never the worker"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn missing_binding_project_change_and_narrow_profile_fail_closed() {
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
    assert_eq!(get(&clone, &id).unwrap().task.status(), TaskStatus::Working);
    let before = std::fs::read(run.join("status.json")).unwrap();
    assert_eq!(cancel(&clone, &id).await.unwrap_err(), unavailable());
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
    std::fs::remove_file(run.join("mcp-tasks.json")).unwrap();
    assert_eq!(
        begin_result(&server, &request()).unwrap_err(),
        unavailable()
    );
    assert_eq!(get(&server, &id).unwrap_err(), unavailable());
}

#[cfg(unix)]
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
#[cfg(unix)]
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

#[cfg(unix)]
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

fn project_claim(server: &MemoryServer, dispatch: Option<&str>, issue: &str) {
    crate::claims_ops::handle_task_claim(
        server,
        &serde_json::from_value(json!({
            "action":"claim", "dispatch_id":dispatch, "issue_ref":issue,
            "claim_mode":"read_only", "claim_role":"observer", "claim_scope":["**"],
            "expected_head":"1234567", "lease_expires_at":"2035-01-01T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
}

fn managed_status(run: &Path, epoch: &str, state: &str, revision: u64) {
    use crate::managed_run_epoch::{build_managed_run_identity, ManagedRunIdentityInput};
    let identity = build_managed_run_identity(
        DISPATCH,
        &ManagedRunIdentityInput {
            controller_epoch_id: epoch.into(),
            assignment_ref: "assignment:test".into(),
            assignment_identity_digest: None,
            execution_grant_ref: "grant:test".into(),
            exec_env_ref: None,
            launch_spec_digest: None,
            backend_name: "fixture".into(),
            backend_metadata_digest: None,
        },
        1,
    );
    std::fs::write(
        run.join("status.json"),
        serde_json::to_vec(&json!({
            "dispatch_id":DISPATCH,"state":state,"status_revision":revision,
            "executed_at":"2026-10-30T10:00:00Z", "updated_at":"2026-10-30T10:01:00Z",
            "managed_run_identity":identity,"private_prompt":"PRIVATE_RUN_SENTINEL"
        }))
        .unwrap(),
    )
    .unwrap();
}

async fn project_view(
    server: &MemoryServer,
    action: &str,
    issue: Option<&str>,
    limit: usize,
) -> Value {
    let raw = server
        .tachi_task(Parameters(
            serde_json::from_value(json!({
                "action":action,"issue_ref":issue,"limit":limit,"format":"json"
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert!(!raw.contains("PRIVATE_RUN_SENTINEL"));
    value
        .get("work_read_model")
        .or_else(|| value.pointer("/cycle/work_read_model"))
        .unwrap()
        .clone()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_receipts_share_revisions_and_preserve_controller_loss_without_writes() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    project_claim(&server, Some("20261030T100001Z-missing"), "org/repo#1694");
    for (epoch, state, expected, revision) in [
        (
            server.controller_epoch.as_str(),
            "TASK_STATE_WORKING",
            "unknown",
            1,
        ),
        ("ctrl-previous", "TASK_STATE_WORKING", "orphaned", 2),
        ("ctrl-previous", "TASK_STATE_FAILED", "TASK_STATE_FAILED", 3),
        (
            "ctrl-previous",
            "TASK_STATE_CANCELED",
            "TASK_STATE_CANCELED",
            4,
        ),
        (
            "ctrl-previous",
            "TASK_STATE_COMPLETED",
            "TASK_STATE_COMPLETED",
            5,
        ),
    ] {
        managed_status(&run, epoch, state, revision);
        let before = std::fs::read(run.join("status.json")).unwrap();
        let binding = std::fs::read(run.join("mcp-tasks.json")).unwrap();
        let mut items = Vec::new();
        for action in ["board", "status", "brief"] {
            let section = project_view(&server, action, Some("org/repo#1693"), 100).await;
            assert_eq!(
                section["sources"]["run_receipts"]["state"], "available",
                "{section}"
            );
            let mut item = section["items"][0].clone();
            item.as_object_mut().unwrap().remove("read_at");
            assert_eq!(item["run_receipts"][0]["status_revision"], revision);
            assert_eq!(item["run_receipts"][0]["state"], state);
            assert_eq!(item["run_receipts"][0]["execution_state"], expected);
            assert_eq!(item["status"]["success_shaped"], false);
            if expected == "unknown" || expected == "orphaned" {
                assert!(!item["status"]["run"].as_str().unwrap().contains("running"));
            }
            items.push(item);
        }
        assert_eq!(items[0], items[1]);
        assert_eq!(items[1], items[2]);
        let all = project_view(&server, "board", None, 100).await;
        assert_eq!(all["sources"]["run_receipts"]["incomplete"], true);
        let mut full_item = all["items"][0].clone();
        full_item.as_object_mut().unwrap().remove("read_at");
        assert_eq!(
            items[0], full_item,
            "neighbor's missing run cannot change this work revision"
        );
        let limited = project_view(&server, "board", None, 1).await;
        assert_eq!(limited["items"][0]["revision"], items[0]["revision"]);
        assert_eq!(limited["items"].as_array().unwrap().len(), 1);
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
        assert_eq!(std::fs::read(run.join("mcp-tasks.json")).unwrap(), binding);
    }
    project_claim(&server, Some("20261030T100002Z-missing"), "org/repo#1693");
    let partial = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    assert_eq!(partial["sources"]["run_receipts"]["incomplete"], true);
    managed_status(&run, &server.controller_epoch, "TASK_STATE_COMPLETED", 6);
    let updated = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    assert_ne!(
        partial["items"][0]["revision"],
        updated["items"][0]["revision"]
    );
    assert_eq!(
        partial["items"][0]["projection_revision"],
        updated["items"][0]["projection_revision"]
    );
    let zero = project_view(&server, "board", None, 0).await;
    assert_eq!(zero["items"], json!([]));
    assert_eq!(zero["sources"]["run_receipts"]["state"], "unavailable");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_claim_never_grants_run_access_and_unbound_receipts_stay_unavailable() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    managed_status(&run, &server.controller_epoch, "TASK_STATE_WORKING", 1);
    project_claim(&server, None, "org/repo#1693");
    let no_link = project_view(&server, "board", None, 100).await;
    assert_eq!(no_link["sources"]["run_receipts"]["state"], "unavailable");
    assert_eq!(no_link["items"][0]["run_receipts"], json!([]));
    bind_identity(&server, "agent.intruder");
    project_claim(&server, Some(DISPATCH), "org/repo#1694");
    project_claim(&server, Some("20261030T100001Z-missing"), "org/repo#1695");
    let foreign = project_view(&server, "board", Some("org/repo#1694"), 100).await;
    let missing = project_view(&server, "board", Some("org/repo#1695"), 100).await;
    for section in [&foreign, &missing] {
        assert_eq!(section["sources"]["run_receipts"]["state"], "unavailable");
        let row = &section["items"][0]["run_receipts"][0];
        assert_eq!(row.as_object().unwrap().len(), 2);
        assert_eq!(row["availability"], "unavailable");
    }
    bind_identity(&server, "agent.owner");
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    let ambiguous = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    assert_eq!(ambiguous["sources"]["run_receipts"]["state"], "unavailable");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_receipts_refuse_foreign_source_malformed_missing_and_revision_regression() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_WORKING", 2);
    let task = start(&server);
    let original = std::fs::read(run.join("mcp-tasks.json")).unwrap();
    for field in [
        "agent_identity_id",
        "canonical_project",
        "source_fingerprint",
    ] {
        let mut binding: Value = serde_json::from_slice(&original).unwrap();
        binding["owner"][field] = json!("foreign");
        std::fs::write(
            run.join("mcp-tasks.json"),
            serde_json::to_vec(&binding).unwrap(),
        )
        .unwrap();
        let section = project_view(&server, "board", None, 100).await;
        assert_eq!(
            section["sources"]["run_receipts"]["state"], "unavailable",
            "{field}"
        );
    }
    std::fs::write(run.join("mcp-tasks.json"), &original).unwrap();
    managed_status(&run, &server.controller_epoch, "TASK_STATE_WORKING", 1);
    assert!(get(&server, &task).is_err());
    let stale = project_view(&server, "board", None, 100).await;
    assert_eq!(stale["sources"]["run_receipts"]["state"], "unavailable");
    for bytes in [
        b"{".as_slice(),
        b"{}",
        b"{\"dispatch_id\":\"foreign\",\"status_revision\":3}",
    ] {
        std::fs::write(run.join("status.json"), bytes).unwrap();
        let section = project_view(&server, "board", None, 100).await;
        assert_eq!(section["sources"]["run_receipts"]["state"], "unavailable");
    }
    std::fs::remove_file(run.join("status.json")).unwrap();
    let missing = project_view(&server, "board", None, 100).await;
    assert_eq!(missing["sources"]["run_receipts"]["state"], "unavailable");
    assert_eq!(std::fs::read(run.join("mcp-tasks.json")).unwrap(), original);
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_receipts_refuse_alias_and_replacement_after_read() {
    use crate::managed_run_control::test_hooks::{install_status_io_hook, StatusIoHookStage};
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_WORKING", 1);
    let status = run.join("status.json");
    let parked = run.join("parked.json");
    std::fs::rename(&status, &parked).unwrap();
    std::os::unix::fs::symlink(&parked, &status).unwrap();
    let aliased = project_view(&server, "board", None, 100).await;
    assert_eq!(aliased["sources"]["run_receipts"]["state"], "unavailable");
    std::fs::remove_file(&status).unwrap();
    std::fs::rename(&parked, &status).unwrap();
    install_status_io_hook(
        StatusIoHookStage::AfterStatusRead,
        run.clone(),
        move |path| {
            std::fs::rename(path, path.with_file_name("parked-run")).unwrap();
            std::fs::create_dir(path).unwrap();
        },
    );
    let replaced = project_view(&server, "board", None, 100).await;
    assert_eq!(replaced["sources"]["run_receipts"]["state"], "unavailable");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_run_limit_is_applied_before_receipt_reads() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    for number in 0..9 {
        project_claim(
            &server,
            Some(&format!("20261030T100000Z-run-{number}")),
            "org/repo#1693",
        );
    }
    let before = std::fs::read(run.join("mcp-tasks.json")).unwrap();
    let section = project_view(&server, "board", None, 100).await;
    assert_eq!(section["available"], false);
    assert_eq!(section["health"]["bounded"], true);
    assert_eq!(
        section["sources"]["work_claims"]["reason"],
        "project_receipt_limit"
    );
    assert_eq!(std::fs::read(run.join("mcp-tasks.json")).unwrap(), before);
}

#[test]
fn project_reads_do_not_wait_for_the_tasks_status_mutex() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_WORKING", 1);
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let guard = lock.lock().unwrap();
    let result = std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::channel();
        let server = &*server;
        let worker = scope.spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            send.send(runtime.block_on(project_view(server, "board", None, 100)))
                .unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(5));
        // Release even on a regression so the worker can exit before failing.
        drop(guard);
        worker.join().unwrap();
        result
    });
    let section = result.expect("project admission must not wait for a Tasks-held status mutex");
    assert_eq!(section["sources"]["run_receipts"]["state"], "available");
}

fn mint_project_delivery(server: &MemoryServer, host: Option<&str>) -> memcore::DeliveryIntent {
    server
        .with_global_store(|store| {
            let claim = memcore::find_claim_requester_for_dispatch(store.connection(), DISPATCH)
                .map_err(|e| e.to_string())?
                .unwrap();
            memcore::mint_delivery_intent(
                store.connection(),
                &memcore::NewDeliveryIntent {
                    idempotency_key: crate::delivery_ops::managed_delivery_key(DISPATCH),
                    execution_source: memcore::DeliveryExecutionSource::ManagedDispatch,
                    execution_ref: DISPATCH.into(),
                    terminal_receipt_revision: 1,
                    work_claim_id: Some(claim.claim_id),
                    result_ref: "private-result-ref-sentinel".into(),
                    result_revision: 1,
                    payload_digest: "result-digest".into(),
                    visibility_class: memcore::DeliveryVisibilityClass::Private,
                    delivery_policy: memcore::DeliveryPolicy::ReturnToCurrentCall,
                    protocol_capability: "result-ref-v1".into(),
                    requester: memcore::DeliveryRequesterBinding {
                        agent_identity_id: Some("agent.owner".into()),
                        host_identity: host.map(str::to_owned),
                        session_ref: None,
                    },
                    expires_at: None,
                    correction: false,
                },
            )
            .map_err(|e| e.to_string())
        })
        .unwrap()
}

fn delivery_snapshot(server: &MemoryServer, id: &str) -> (Value, i64) {
    server
        .with_global_store(|store| {
            let row =
                memcore::get_delivery_intent(store.connection(), id).map_err(|e| e.to_string())?;
            let count = store
                .connection()
                .query_row("SELECT count(*) FROM delivery_events", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            Ok((serde_json::to_value(row).unwrap(), count))
        })
        .unwrap()
}

async fn assert_project_delivery(server: &MemoryServer, id: &str, state: &str) -> Value {
    let before = delivery_snapshot(server, id);
    let mut items = Vec::new();
    for action in ["board", "status", "brief"] {
        let section = project_view(server, action, Some("org/repo#1693"), 100).await;
        assert_eq!(
            section["sources"]["delivery"]["state"], "available",
            "{section}"
        );
        assert!(!section.to_string().contains("private-result-ref-sentinel"));
        let mut item = section["items"][0].clone();
        item.as_object_mut().unwrap().remove("read_at");
        assert_eq!(item["delivery_intents"][0]["delivery_state"], state);
        assert_eq!(item["delivery_intents"][0]["delivery_id"], id);
        assert_eq!(
            item["delivery_intents"][0]["revision"],
            before.0["revision"]
        );
        assert_eq!(item["status"]["delivery"], "observed");
        assert_eq!(item["status"]["success_shaped"], false);
        assert_eq!(item["status"]["adjudication"], "unavailable");
        items.push(item);
    }
    assert_eq!(items[0], items[1]);
    assert_eq!(items[1], items[2]);
    assert_eq!(
        delivery_snapshot(server, id),
        before,
        "reads cannot claim/ack/retry delivery"
    );
    items.remove(0)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_delivery_tracks_real_transitions_without_changing_execution_or_acceptance() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_COMPLETED", 1);
    let intent = mint_project_delivery(&server, None);
    let id = &intent.delivery_id;
    let original_receipt = std::fs::read(run.join("status.json")).unwrap();
    let ready = assert_project_delivery(&server, id, "ready").await;
    let caller = memcore::DeliveryCaller {
        agent_identity_id: "agent.owner".into(),
        host_identity: "agent.owner".into(),
    };
    let claim = |key: &str| {
        server
            .with_global_store(|store| {
                memcore::claim_ready_delivery(
                    store.connection(),
                    &memcore::DeliveryClaimRequest {
                        caller: caller.clone(),
                        claim_key: key.into(),
                        lease_seconds: 60,
                        only_delivery_id: Some(id.clone()),
                    },
                )
                .map_err(|e| e.to_string())
            })
            .unwrap();
    };
    claim("first");
    let queued = assert_project_delivery(&server, id, "requester_queued").await;
    assert_ne!(ready["revision"], queued["revision"]);
    server
        .with_global_store(|store| {
            memcore::reject_or_block(
                store.connection(),
                id,
                &caller,
                "ambiguous_send_outcome",
                None,
                None,
                None,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    let blocked = assert_project_delivery(&server, id, "blocked").await;
    assert_ne!(queued["revision"], blocked["revision"]);
    server
        .with_global_store(|store| {
            memcore::resume_requester_operation(store.connection(), &caller, true)
                .map_err(|e| e.to_string())
        })
        .unwrap();
    claim("second");
    server
        .with_global_store(|store| {
            memcore::ack_delivered(store.connection(), id, &caller, "ack", None)
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let delivered = assert_project_delivery(&server, id, "delivered").await;
    assert_ne!(blocked["revision"], delivered["revision"]);
    assert_eq!(ready["status"]["run"], delivered["status"]["run"]);
    // Correction re-arms a delivered result; old delivery cannot label the new revision delivered.
    server
        .with_global_store(|store| {
            let conn = store.connection();
            let new = memcore::NewDeliveryIntent {
                idempotency_key: intent.idempotency_key.clone(),
                execution_source: memcore::DeliveryExecutionSource::ManagedDispatch,
                execution_ref: DISPATCH.into(),
                terminal_receipt_revision: 2,
                work_claim_id: intent.work_claim_id.clone(),
                result_ref: "corrected-result-ref".into(),
                result_revision: 2,
                payload_digest: "corrected-digest".into(),
                visibility_class: memcore::DeliveryVisibilityClass::Private,
                delivery_policy: memcore::DeliveryPolicy::ReturnToCurrentCall,
                protocol_capability: "result-ref-v1".into(),
                requester: memcore::DeliveryRequesterBinding {
                    agent_identity_id: Some("agent.owner".into()),
                    host_identity: None,
                    session_ref: None,
                },
                expires_at: None,
                correction: true,
            };
            memcore::mint_delivery_intent(conn, &new).map_err(|e| e.to_string())
        })
        .unwrap();
    let corrected = assert_project_delivery(&server, id, "ready").await;
    assert_eq!(corrected["delivery_intents"][0]["result_revision"], 2);
    assert_ne!(corrected["revision"], delivered["revision"]);
    claim("third");
    server
        .with_global_store(|store| {
            memcore::reject_or_block(
                store.connection(),
                id,
                &caller,
                "transport_failure",
                None,
                Some(60),
                None,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_project_delivery(&server, id, "retrying").await;
    server
        .with_global_store(|store| {
            memcore::dismiss_delivery(store.connection(), id, &caller, None)
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_project_delivery(&server, id, "dismissed").await;
    assert_eq!(
        std::fs::read(run.join("status.json")).unwrap(),
        original_receipt
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_delivery_masks_foreign_bindings_and_optional_source_failure() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_COMPLETED", 1);
    let missing = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    let intent = mint_project_delivery(&server, Some("different-host"));
    let denied = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    assert_eq!(
        denied["sources"]["delivery"],
        missing["sources"]["delivery"]
    );
    assert_eq!(
        denied["items"][0]["revision"],
        missing["items"][0]["revision"]
    );
    assert!(!denied.to_string().contains(&intent.delivery_id));
    for (field, value) in [
        ("requester_agent_identity_id", "other-agent"),
        ("work_claim_id", "foreign-claim"),
        ("execution_ref", "foreign-dispatch"),
    ] {
        server.with_global_store(|store| {
            store.connection().execute("UPDATE delivery_intents SET requester_host_identity=NULL, requester_agent_identity_id='agent.owner', work_claim_id=?1, execution_ref=?2, delivery_state='ready' WHERE delivery_id=?3", rusqlite::params![intent.work_claim_id, DISPATCH, intent.delivery_id]).map_err(|e| e.to_string())?;
            store.connection().execute(&format!("UPDATE delivery_intents SET {field}=?1 WHERE delivery_id=?2"), rusqlite::params![value, intent.delivery_id]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        let denied = project_view(&server, "board", Some("org/repo#1693"), 100).await;
        assert_eq!(
            denied["sources"]["delivery"],
            missing["sources"]["delivery"]
        );
        assert!(!denied.to_string().contains(&intent.delivery_id));
    }
    let zero = project_view(&server, "board", None, 0).await;
    assert_eq!(zero["items"], json!([]));
    assert_eq!(zero["sources"]["delivery"]["state"], "unavailable");
    // Corrupt only this disposable fixture from an independent connection;
    // the production store correctly denies DDL through its authorizer.
    let fixture_db = rusqlite::Connection::open(server.global_db_path_buf()).unwrap();
    fixture_db
        .execute_batch("ALTER TABLE delivery_intents RENAME TO offline_delivery_intents")
        .unwrap();
    drop(fixture_db);
    let offline = project_view(&server, "brief", Some("org/repo#1693"), 100).await;
    assert_eq!(offline["sources"]["run_receipts"]["state"], "available");
    assert_eq!(
        offline["sources"]["delivery"],
        missing["sources"]["delivery"]
    );
    bind_identity(&server, "unqualified-agent");
    let revoked = project_view(&server, "board", None, 100).await;
    assert_eq!(revoked["available"], false);
    assert_eq!(revoked["items"], json!([]));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_delivery_requires_current_terminal_owned_run() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    let intent = mint_project_delivery(&server, Some("agent.owner"));
    for state in ["TASK_STATE_WORKING", "TASK_STATE_ORPHANED"] {
        managed_status(&run, &server.controller_epoch, state, 1);
        let view = project_view(&server, "board", Some("org/repo#1693"), 100).await;
        assert_eq!(view["sources"]["delivery"]["state"], "unavailable");
        assert!(!view.to_string().contains(&intent.delivery_id));
    }
    managed_status(&run, &server.controller_epoch, "TASK_STATE_COMPLETED", 2);
    assert_project_delivery(&server, &intent.delivery_id, "ready").await;
    // NotReady has no public producer today, but remains a valid persisted state.
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE delivery_intents SET delivery_state='not_ready' WHERE delivery_id=?1",
                    [&intent.delivery_id],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap();
    assert_project_delivery(&server, &intent.delivery_id, "not_ready").await;
    // An admitted peer may see the project claim, but cannot inherit this run owner.
    bind_identity(&server, "agent.peer");
    project_claim(&server, None, "org/repo#peer");
    let peer = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    assert_eq!(peer["available"], true);
    assert_eq!(peer["sources"]["delivery"]["state"], "unavailable");
    assert!(!peer.to_string().contains(&intent.delivery_id));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_delivery_partial_rows_update_only_their_work_revision() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_COMPLETED", 1);
    let intent = mint_project_delivery(&server, None);
    project_claim(&server, Some("20261030T100002Z-missing"), "org/repo#1693");
    project_claim(&server, Some("20261030T100003Z-neighbor"), "org/repo#1694");
    let before = project_view(&server, "board", Some("org/repo#1693"), 100).await;
    assert_eq!(before["sources"]["delivery"]["incomplete"], true);
    assert_eq!(before["items"][0]["status"]["delivery"], "not_integrated");
    assert_eq!(
        before["items"][0]["delivery_intents"][0]["delivery_state"],
        "ready"
    );
    assert_eq!(
        before["items"][0]["delivery_intents"][1]["availability"],
        "unavailable"
    );
    let neighbor = project_view(&server, "board", Some("org/repo#1694"), 100).await;
    let snapshot = delivery_snapshot(&server, &intent.delivery_id);
    server
        .with_global_store(|store| {
            memcore::claim_ready_delivery(
                store.connection(),
                &memcore::DeliveryClaimRequest {
                    caller: memcore::DeliveryCaller {
                        agent_identity_id: "agent.owner".into(),
                        host_identity: "agent.owner".into(),
                    },
                    claim_key: "partial-delivery".into(),
                    lease_seconds: 60,
                    only_delivery_id: Some(intent.delivery_id.clone()),
                },
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_ne!(snapshot, delivery_snapshot(&server, &intent.delivery_id));
    let after = project_view(&server, "brief", Some("org/repo#1693"), 100).await;
    assert_eq!(after["sources"]["delivery"]["state"], "unavailable");
    assert_eq!(
        after["items"][0]["delivery_intents"][0]["delivery_state"],
        "requester_queued"
    );
    assert_ne!(
        before["items"][0]["revision"],
        after["items"][0]["revision"]
    );
    assert_eq!(
        before["items"][0]["projection_revision"],
        after["items"][0]["projection_revision"]
    );
    let neighbor_after = project_view(&server, "status", Some("org/repo#1694"), 100).await;
    assert_eq!(
        neighbor["items"][0]["revision"],
        neighbor_after["items"][0]["revision"]
    );
    assert_eq!(
        neighbor["sources"]["delivery"],
        neighbor_after["sources"]["delivery"]
    );
    let full = project_view(&server, "board", None, 100).await;
    assert_eq!(full["items"][0]["revision"], after["items"][0]["revision"]);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_delivery_observes_production_terminal_failure_mint() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), "org/repo#1693");
    managed_status(&run, &server.controller_epoch, "TASK_STATE_FAILED", 1);
    crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
        &server,
        DISPATCH,
        "watchdog",
        Some("codex"),
        Some(PROJECT),
    );
    let intent = server
        .with_global_store(|store| {
            memcore::find_delivery_intent_by_idempotency_key(
                store.connection(),
                &crate::delivery_ops::managed_delivery_key(DISPATCH),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap()
        .expect("the production terminal failure producer mints in the global store");
    assert_eq!(intent.visibility_class, "private");
    assert_eq!(
        intent.requester_agent_identity_id.as_deref(),
        Some("agent.owner")
    );
    let item = assert_project_delivery(&server, &intent.delivery_id, "ready").await;
    assert_eq!(
        item["delivery_intents"][0]["result_revision"],
        intent.result_revision
    );
    assert_eq!(item["run_receipts"][0]["state"], "TASK_STATE_FAILED");
}
