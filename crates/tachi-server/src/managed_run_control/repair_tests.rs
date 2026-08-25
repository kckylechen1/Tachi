use super::*;
use serde_json::{json, Value};
use std::path::Path;
use tokio::sync::mpsc;

fn write_managed_status(run_dir: &Path, dispatch_id: &str, revision: u64) {
    std::fs::create_dir_all(run_dir).expect("run directory");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": revision,
            "execution_classification": "managed_custom",
            "lifecycle_owner": "memory_server_managed_custom",
        })
        .to_string(),
    )
    .expect("managed status");
}

async fn cancel_and_observe_command(
    server: &MemoryServer,
    dispatch_id: &str,
    expected: u64,
    receiver: &mut mpsc::Receiver<ManagedCancelCommand>,
) -> (Value, bool) {
    let request = request_managed_custom_cancel(server, dispatch_id, expected);
    tokio::pin!(request);
    tokio::select! {
        response = &mut request => {
            let response = response.expect("cancellation response");
            (serde_json::from_str(&response).expect("cancellation JSON"), false)
        }
        command = receiver.recv() => {
            let command = command.expect("managed cancellation command");
            let _ = command.response.send(CancelCompletion::Unavailable("test_runner_unavailable"));
            let response = request.await.expect("cancellation response after command");
            (serde_json::from_str(&response).expect("cancellation JSON"), true)
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn issue_1833_in_root_alias_cannot_mutate_b_or_send_an_a_command() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    std::fs::create_dir_all(&runs_root).expect("runs root");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_a = "20260825T183301Z-alias-a";
    let dispatch_b = "20260825T183301Z-alias-b";
    let run_b = runs_root.join(dispatch_b);
    write_managed_status(&run_b, dispatch_b, 7);
    let b_before = std::fs::read(run_b.join("status.json")).expect("status B before");
    std::os::unix::fs::symlink(&run_b, runs_root.join(dispatch_a)).expect("A -> B alias");
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_a)
        .expect("register A control");

    let (response, command_seen) =
        cancel_and_observe_command(&server, dispatch_a, 7, &mut receiver).await;

    assert_eq!(response["receipt"], "cancellation_unavailable");
    assert!(!command_seen, "an A -> B alias must not send an A command");
    assert_eq!(
        std::fs::read(run_b.join("status.json")).expect("status B after"),
        b_before,
        "an A -> B alias must not mutate B"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn issue_1833_receipt_dispatch_id_mismatch_is_rejected_before_mutation_or_command() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_a = "20260825T183302Z-receipt-a";
    let dispatch_b = "20260825T183302Z-receipt-b";
    let run_a = runs_root.join(dispatch_a);
    write_managed_status(&run_a, dispatch_b, 7);
    let status_before = std::fs::read(run_a.join("status.json")).expect("status before");
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_a)
        .expect("register A control");

    let (response, command_seen) =
        cancel_and_observe_command(&server, dispatch_a, 7, &mut receiver).await;

    assert_eq!(response["receipt"], "cancellation_unavailable");
    assert_eq!(response["reason"], "dispatch_identity_mismatch");
    assert!(
        !command_seen,
        "a mismatched receipt must not send a command"
    );
    assert_eq!(
        std::fs::read(run_a.join("status.json")).expect("status after"),
        status_before,
        "a mismatched receipt must not be mutated"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn issue_1833_post_validation_read_stays_bound_to_the_opened_a_directory() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_a = "20260825T183303Z-read-a";
    let dispatch_b = "20260825T183303Z-read-b";
    let run_a = runs_root.join(dispatch_a);
    let run_b = runs_root.join(dispatch_b);
    let parked_a = runs_root.join("parked-read-a");
    write_managed_status(&run_a, dispatch_a, 7);
    write_managed_status(&run_b, dispatch_b, 99);
    let b_before = std::fs::read(run_b.join("status.json")).expect("status B before");
    let swap_a = run_a.clone();
    let swap_b = run_b.clone();
    let swap_parked = parked_a.clone();
    super::test_hooks::install_status_io_hook(
        super::test_hooks::StatusIoHookStage::AfterDirectoryValidation,
        run_a.clone(),
        move |_| {
            std::fs::rename(&swap_a, &swap_parked).expect("park A after validation");
            std::fs::rename(&swap_b, &swap_a).expect("replace A path with B");
        },
    );
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_a)
        .expect("register A control");

    let (response, command_seen) =
        cancel_and_observe_command(&server, dispatch_a, 7, &mut receiver).await;

    assert!(
        command_seen,
        "the opened A receipt should admit the A command"
    );
    assert_eq!(response["reason"], "test_runner_unavailable");
    assert_eq!(
        std::fs::read(run_a.join("status.json")).expect("replacement B after"),
        b_before,
        "a post-validation replacement must not redirect the A read or fallback write to B"
    );
    let parked: Value = serde_json::from_slice(
        &std::fs::read(parked_a.join("status.json")).expect("parked A status"),
    )
    .expect("parked A JSON");
    assert_eq!(parked["dispatch_id"], dispatch_a);
    assert_eq!(
        parked["cancellation"]["receipt"],
        "cancellation_unavailable"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn issue_1833_atomic_rename_stays_bound_to_a_after_a_post_validation_swap() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_a = "20260825T183304Z-write-a";
    let dispatch_b = "20260825T183304Z-write-b";
    let run_a = runs_root.join(dispatch_a);
    let run_b = runs_root.join(dispatch_b);
    let parked_a = runs_root.join("parked-write-a");
    write_managed_status(&run_a, dispatch_a, 7);
    write_managed_status(&run_b, dispatch_b, 7);
    let b_before = std::fs::read(run_b.join("status.json")).expect("status B before");
    let swap_a = run_a.clone();
    let swap_b = run_b.clone();
    let swap_parked = parked_a.clone();
    super::test_hooks::install_status_io_hook(
        super::test_hooks::StatusIoHookStage::BeforeAtomicRename,
        run_a.clone(),
        move |_| {
            std::fs::rename(&swap_a, &swap_parked).expect("park A before rename");
            std::fs::rename(&swap_b, &swap_a).expect("replace A path with B");
        },
    );
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_a)
        .expect("register A control");

    let (response, command_seen) =
        cancel_and_observe_command(&server, dispatch_a, 7, &mut receiver).await;

    assert!(
        command_seen,
        "the validated A receipt should admit the A command"
    );
    assert_eq!(response["reason"], "test_runner_unavailable");
    assert_eq!(
        std::fs::read(run_a.join("status.json")).expect("replacement B after"),
        b_before,
        "the descriptor-relative atomic rename must not replace B/status.json"
    );
    let parked: Value = serde_json::from_slice(
        &std::fs::read(parked_a.join("status.json")).expect("parked A status"),
    )
    .expect("parked A JSON");
    assert_eq!(parked["dispatch_id"], dispatch_a);
    assert_eq!(
        parked["cancellation"]["receipt"],
        "cancellation_unavailable"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn issue_1833_wait_loop_observes_the_opened_a_after_the_visible_path_becomes_b() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_a = "20260825T183305Z-wait-a";
    let dispatch_b = "20260825T183305Z-wait-b";
    let run_a = runs_root.join(dispatch_a);
    let run_b = runs_root.join(dispatch_b);
    let parked_a = runs_root.join("parked-wait-a");
    write_managed_status(&run_a, dispatch_a, 7);
    write_managed_status(&run_b, dispatch_b, 7);
    let b_before = std::fs::read(run_b.join("status.json")).expect("status B before");
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_a)
        .expect("register A control");

    let request = request_managed_custom_cancel(&server, dispatch_a, 7);
    tokio::pin!(request);
    let command = tokio::select! {
        command = receiver.recv() => command.expect("A cancellation command"),
        response = &mut request => panic!("request returned before command dequeue: {response:?}"),
    };
    std::fs::rename(&run_a, &parked_a).expect("park A while waiter is pending");
    std::fs::rename(&run_b, &run_a).expect("replace visible A path with B");
    let mut terminal: Value = serde_json::from_slice(
        &std::fs::read(parked_a.join("status.json")).expect("parked A requested status"),
    )
    .expect("parked A JSON");
    terminal["state"] = Value::String("TASK_STATE_CANCELED".to_string());
    terminal["status_revision"] = Value::from(9_u64);
    terminal["cancellation"] = cancellation_receipt(
        "cancellation_confirmed",
        dispatch_a,
        7,
        9,
        None,
        Some("unix_process_group_absent"),
    );
    std::fs::write(
        parked_a.join("status.json"),
        serde_json::to_vec_pretty(&terminal).expect("terminal A body"),
    )
    .expect("terminalize parked A");
    drop(command.response);

    let response: Value =
        serde_json::from_str(&request.await.expect("wait-loop cancellation response"))
            .expect("wait-loop cancellation JSON");
    assert_eq!(response["receipt"], "cancellation_confirmed");
    assert_eq!(response["dispatch_id"], dispatch_a);
    assert_eq!(
        std::fs::read(run_a.join("status.json")).expect("replacement B after"),
        b_before,
        "wait-loop observations must not follow the visible A path to B"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn issue_1833_terminal_finalization_stays_with_the_accepted_physical_run() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_a = "20260825T183306Z-terminal-a";
    let run_a = runs_root.join(dispatch_a);
    let parked_a = runs_root.join("parked-terminal-a");
    write_managed_status(&run_a, dispatch_a, 7);
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_a)
        .expect("register A control");

    let request = request_managed_custom_cancel(&server, dispatch_a, 7);
    tokio::pin!(request);
    let command = tokio::select! {
        command = receiver.recv() => command.expect("accepted A cancellation command"),
        response = &mut request => panic!("request returned before command dequeue: {response:?}"),
    };
    let accepted: Value = serde_json::from_slice(
        &std::fs::read(run_a.join("status.json")).expect("accepted A status"),
    )
    .expect("accepted A JSON");
    assert_eq!(
        accepted["cancellation"]["receipt"],
        "cancellation_requested"
    );

    std::fs::rename(&run_a, &parked_a).expect("park accepted A");
    std::fs::create_dir_all(&run_a).expect("install replacement A directory");
    std::fs::write(
        run_a.join("status.json"),
        json!({
            "dispatch_id": dispatch_a,
            "state": "TASK_STATE_WORKING",
            "status_revision": 8,
            "execution_classification": "managed_custom",
            "lifecycle_owner": "memory_server_managed_custom",
            "cancellation": cancellation_receipt(
                "cancellation_requested", dispatch_a, 7, 8, None, None
            ),
            "forged_replacement": true,
        })
        .to_string(),
    )
    .expect("install forged replacement A receipt");
    let replacement_before =
        std::fs::read(run_a.join("status.json")).expect("replacement A before finalization");

    let completion = crate::dispatch_ops::write_status_json_with_managed_anchor(
        &run_a,
        dispatch_a,
        false,
        None,
        None,
        "n/a",
        None,
        None,
        None,
        None,
        Some(json!({
            "state": "TASK_STATE_CANCELED",
            "managed_cancellation_finalization": {
                "expected_status_revision": command.expected_status_revision,
                "runner_error": "managed_cancelled",
                "termination_proof": "unix_process_group_absent",
                "credential_cleanup_failed": false,
                "result_persist_failed": false,
            }
        })),
        &command.status_anchor,
    );
    let _ = command
        .response
        .send(completion.expect("terminal completion receipt"));
    let response: Value =
        serde_json::from_str(&request.await.expect("anchored cancellation response"))
            .expect("cancellation response JSON");

    assert_eq!(response["receipt"], "cancellation_confirmed");
    assert_eq!(
        std::fs::read(run_a.join("status.json")).expect("replacement A after finalization"),
        replacement_before,
        "terminal finalization must not mutate a forged replacement A"
    );
    let original: Value = serde_json::from_slice(
        &std::fs::read(parked_a.join("status.json")).expect("parked original A status"),
    )
    .expect("parked original A JSON");
    assert_eq!(original["state"], "TASK_STATE_CANCELED");
    assert_eq!(
        original["cancellation"]["receipt"], "cancellation_confirmed",
        "terminal finalization must remain durable on the accepted physical A"
    );
}
