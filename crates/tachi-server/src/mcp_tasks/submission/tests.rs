use super::*;
use crate::test_support::EnvRestore;
use std::sync::Arc;

const PROJECT: &str = "submission-project";

fn bind(server: &MemoryServer, identity: &str) {
    bind_project(server, identity, PROJECT);
}

fn bind_project(server: &MemoryServer, identity: &str, project: &str) {
    server.set_session_identity(
        Some(uuid::Uuid::new_v4().to_string()),
        Some(project.into()),
        Some(tachi_hub::ToolProfile::coordinate()),
    );
    crate::claims_ops::admit_agent_connection(server, Some(identity.into()), true).unwrap();
}

fn request(key: &str) -> StaffAssignmentRequest {
    let mut request = StaffAssignmentRequest::new(
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "review the same issue in one flow",
    );
    request.project = Some(PROJECT.into());
    request.submission_id = Some(key.into());
    request.profile = Some("codex_53_fast".into());
    request.issue_ref = Some("kckylechen1/Tachi#2290".into());
    request.flow_id = Some("submission-flow".into());
    request
}

fn status_params(key: &str) -> TachiStaffParams {
    serde_json::from_value(json!({"action": "status", "submission_id": key})).unwrap()
}

fn reserve(request: &SubmissionRequest) -> SubmissionReservation {
    match request.reserve().unwrap() {
        ReservationOutcome::Reserved(reservation) => reservation,
        ReservationOutcome::Existing(raw) => panic!("expected new reservation: {raw}"),
    }
}

fn write_receipt(request: &SubmissionRequest, state: &str) {
    let anchor = AnchoredRunStatus::open(&request.run_dir()).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    anchor
        .write_atomic(
            &serde_json::to_vec(&json!({
                "dispatch_id": request.dispatch_id, "state": state,
                "status_revision": 3, "project": PROJECT,
            }))
            .unwrap(),
            &fence,
        )
        .unwrap();
}

#[test]
fn submission_binding_survives_reconnect_restart_terminal_and_result_expiry() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", runs.path());
    bind(&server, "submission.owner");
    let first = SubmissionRequest::from_start(&server, &request("first"))
        .unwrap()
        .unwrap();
    let reservation = reserve(&first);
    let incomplete: Value = serde_json::from_str(&first.response(true).unwrap()).unwrap();
    assert_eq!(incomplete["submission"]["binding_state"], "incomplete");
    assert_eq!(incomplete["submission"]["retry_safe"], false);
    reservation
        .verify_launch(&server, &first.run_dir(), &first.dispatch_id, Some(PROJECT))
        .unwrap();
    write_receipt(&first, "TASK_STATE_WORKING");
    let result_request = rmcp::model::CallToolRequestParams::new("tachi_staff").with_arguments(
        json!({"action": "result", "dispatch_id": first.dispatch_id, "format": "json"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let rmcp::model::CallToolResponse::Task(result_task) =
        super::super::begin_result(&server, &result_request).unwrap()
    else {
        panic!("the existing Tasks result handle must work for a keyed run");
    };
    assert_eq!(result_task.task.status, rmcp::model::TaskStatus::Working);
    write_receipt(&first, "TASK_STATE_COMPLETED");
    std::fs::write(first.run_dir().join("result.md"), "one completed result").unwrap();
    let anchor = AnchoredRunStatus::open(&first.run_dir()).unwrap();
    let lock = anchor.lock();
    {
        let _guard = lock.lock().unwrap();
        let fence = anchor.acquire_fence().unwrap();
        let mut binding = read_binding(&anchor, &first.owner, &first.dispatch_id).unwrap();
        assert_eq!(binding.submission.as_ref(), Some(&first.binding));
        binding.result.as_mut().unwrap().created_at = "2000-01-01T00:00:00Z".into();
        anchor
            .write_mcp_binding(&serde_json::to_vec(&binding).unwrap(), &fence)
            .unwrap();
    }
    let before = std::fs::read(first.run_dir().join("mcp-tasks.json")).unwrap();
    bind(&server, "submission.owner");
    let recovered: Value =
        serde_json::from_str(&status(&server, &status_params("first")).unwrap()).unwrap();
    assert_eq!(recovered["dispatch_id"], first.dispatch_id);
    assert_eq!(recovered["submission"]["binding_state"], "bound");
    assert!(matches!(
        first.reserve().unwrap(),
        ReservationOutcome::Existing(_)
    ));
    let global = server.global_db_path_buf();
    let home = global.parent().unwrap().parent().unwrap().join("home");
    let project = server
        .resolve_server_named_project_db_path(PROJECT)
        .unwrap();
    let restarted = MemoryServer::new_with_home_for_test(global, Some(project), home).unwrap();
    bind(&restarted, "submission.owner");
    assert_ne!(server.controller_epoch, restarted.controller_epoch);
    let recovered_after_restart = status(&restarted, &status_params("first")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&recovered_after_restart).unwrap(),
        recovered
    );
    assert_eq!(
        std::fs::read(first.run_dir().join("mcp-tasks.json")).unwrap(),
        before
    );
    let second = SubmissionRequest::from_start(&restarted, &request("second"))
        .unwrap()
        .unwrap();
    let _second = reserve(&second);
    assert_ne!(
        first.dispatch_id, second.dispatch_id,
        "same issue/flow/text cannot choose the original run"
    );
    assert_eq!(
        std::fs::read_dir(crate::dispatch_ops::dispatch_runs_root())
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn concurrent_same_key_has_one_reservation_and_changed_input_cannot_replace_it() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", runs.path());
    bind(&server, "submission.owner");
    let original = SubmissionRequest::from_start(&server, &request("race"))
        .unwrap()
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let outcomes = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let original = &original;
                scope.spawn(move || {
                    barrier.wait();
                    original.reserve().unwrap()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ReservationOutcome::Reserved(_)))
            .count(),
        1
    );
    let before = std::fs::read(original.run_dir().join("mcp-tasks.json")).unwrap();
    let mut changed = request("race");
    changed.task.push_str(" but execute writes instead");
    let changed = SubmissionRequest::from_start(&server, &changed)
        .unwrap()
        .unwrap();
    assert!(changed.reserve().err().unwrap().contains("conflicts"));
    assert_eq!(
        std::fs::read(original.run_dir().join("mcp-tasks.json")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_dir(crate::dispatch_ops::dispatch_runs_root())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn foreign_missing_and_unconfirmable_keys_never_disclose_or_recreate_a_run() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", runs.path());
    bind(&server, "submission.owner");
    let original = SubmissionRequest::from_start(&server, &request("private-key"))
        .unwrap()
        .unwrap();
    let _reserved = reserve(&original);
    write_receipt(&original, "TASK_STATE_WORKING");
    bind(&server, "submission.other");
    let foreign: Value =
        serde_json::from_str(&status(&server, &status_params("private-key")).unwrap()).unwrap();
    assert_eq!(foreign, unconfirmed("private-key", false));
    assert!(!foreign.to_string().contains(&original.dispatch_id));
    bind(&server, "submission.owner");
    let (other_project, _) = crate::tests::make_server_with_project_fixture("different-project");
    bind_project(&other_project, "submission.owner", "different-project");
    let foreign_project: Value =
        serde_json::from_str(&status(&other_project, &status_params("private-key")).unwrap())
            .unwrap();
    assert_eq!(foreign_project, unconfirmed("private-key", false));
    let (other_source, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    bind(&other_source, "submission.owner");
    let foreign_source: Value =
        serde_json::from_str(&status(&other_source, &status_params("private-key")).unwrap())
            .unwrap();
    assert_eq!(foreign_source, unconfirmed("private-key", false));
    let binding_path = original.run_dir().join("mcp-tasks.json");
    std::fs::remove_file(&binding_path).unwrap();
    let before = std::fs::read(original.run_dir().join("status.json")).unwrap();
    assert!(matches!(
        original.reserve().unwrap(),
        ReservationOutcome::Existing(_)
    ));
    let unknown: Value =
        serde_json::from_str(&status(&server, &status_params("private-key")).unwrap()).unwrap();
    assert_eq!(unknown, unconfirmed("private-key", false));
    assert!(!binding_path.exists());
    assert_eq!(
        std::fs::read(original.run_dir().join("status.json")).unwrap(),
        before
    );
    let absent: Value =
        serde_json::from_str(&status(&server, &status_params("never-recorded")).unwrap()).unwrap();
    assert_eq!(absent, unconfirmed("never-recorded", false));
    assert_eq!(
        std::fs::read_dir(crate::dispatch_ops::dispatch_runs_root())
            .unwrap()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn reserved_directory_replacement_cannot_inherit_the_launch_token() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", runs.path());
    bind(&server, "submission.owner");
    let original = SubmissionRequest::from_start(&server, &request("reserved"))
        .unwrap()
        .unwrap();
    let reserved = reserve(&original);
    let saved = runs.path().join("saved");
    std::fs::rename(original.run_dir(), &saved).unwrap();
    std::fs::create_dir(original.run_dir()).unwrap();
    std::fs::copy(
        saved.join("mcp-tasks.json"),
        original.run_dir().join("mcp-tasks.json"),
    )
    .unwrap();
    assert!(reserved
        .verify_launch(
            &server,
            &original.run_dir(),
            &original.dispatch_id,
            Some(PROJECT)
        )
        .is_err());
}

#[cfg(unix)]
#[test]
fn recovery_rejects_directory_replacement_after_read() {
    use crate::managed_run_control::test_hooks::{install_status_io_hook, StatusIoHookStage};
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", runs.path());
    bind(&server, "submission.owner");
    for replayed in [false, true] {
        let key = if replayed {
            "replay-swap"
        } else {
            "status-swap"
        };
        let original = SubmissionRequest::from_start(&server, &request(key))
            .unwrap()
            .unwrap();
        let _reserved = reserve(&original);
        write_receipt(&original, "TASK_STATE_WORKING");
        let parked = runs.path().join(key);
        let saved = parked.clone();
        install_status_io_hook(
            StatusIoHookStage::AfterStatusRead,
            original.run_dir(),
            move |path| {
                std::fs::rename(path, &parked).unwrap();
                std::fs::create_dir(path).unwrap();
            },
        );
        let raw = if replayed {
            original.response(true)
        } else {
            status(&server, &status_params(key))
        }
        .unwrap();
        let response: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(response, unconfirmed(key, replayed));
        assert!(!raw.contains(&original.dispatch_id));
        assert!(saved.join("status.json").is_file());
        assert!(!original.run_dir().join("mcp-tasks.json").exists());
    }
}
