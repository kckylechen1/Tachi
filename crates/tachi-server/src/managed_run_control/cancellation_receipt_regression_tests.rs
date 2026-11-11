use super::*;
use serde_json::json;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn closed_control_channel_returns_the_committed_unavailable_receipt_without_relocking() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs = tempfile::tempdir().expect("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260823T182501Z-closed-channel";
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run directory");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 1,
            "execution_classification": "managed_custom",
            "lifecycle_owner": "memory_server_managed_custom",
        })
        .to_string(),
    )
    .expect("status");
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_id)
        .expect("register control");
    drop(receiver);

    let response = request_managed_custom_cancel(&server, dispatch_id, 1)
        .await
        .expect("closed channel returns an unavailable receipt");
    let status: Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("status.json")).expect("canonical status"),
    )
    .expect("canonical JSON");
    assert_eq!(
        serde_json::from_str::<Value>(&response).expect("response JSON"),
        status["cancellation"],
        "response must be the committed canonical cancellation receipt"
    );
    assert_eq!(
        status["cancellation"]["observed_status_revision"], status["status_revision"],
        "fallback receipt must carry the revision committed by its one write"
    );
    assert_eq!(
        response,
        serde_json::to_string(&status["cancellation"])
            .expect("serialize canonical fallback receipt")
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn full_control_channel_returns_the_committed_unavailable_receipt() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs = tempfile::tempdir().expect("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260823T182502Z-full-channel";
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run directory");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 1,
            "execution_classification": "managed_custom",
            "lifecycle_owner": "memory_server_managed_custom",
        })
        .to_string(),
    )
    .expect("status");
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (_receiver, _guard) = server
        .managed_run_controls
        .register(dispatch_id)
        .expect("register control");
    let sender = server
        .managed_run_controls
        .entries
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(dispatch_id)
        .expect("registered sender")
        .sender
        .clone();
    let (response_tx, _response_rx) = oneshot::channel();
    sender
        .try_send(ManagedCancelCommand {
            expected_status_revision: 0,
            response: response_tx,
            #[cfg(unix)]
            status_anchor: AnchoredRunStatus::open(&run_dir).expect("anchor managed run"),
            #[cfg(test)]
            test_observation: None,
        })
        .expect("fill bounded control channel");
    let response = request_managed_custom_cancel(&server, dispatch_id, 1)
        .await
        .expect("full channel returns committed unavailable receipt");
    let status: Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("status.json")).expect("canonical status"),
    )
    .expect("canonical JSON");
    assert_eq!(
        serde_json::from_str::<Value>(&response).expect("response JSON"),
        status["cancellation"]
    );
    assert_eq!(
        status["cancellation"]["observed_status_revision"],
        status["status_revision"]
    );
}

#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn cancel_reuses_the_accepted_anchor_after_visible_run_directory_replacement() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs = tempfile::tempdir().expect("runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let dispatch_id = "20260825T182503Z-accepted-anchor";
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("accepted run directory");
    let status_body = json!({
        "dispatch_id": dispatch_id,
        "state": "TASK_STATE_WORKING",
        "status_revision": 1,
        "execution_classification": "managed_custom",
        "lifecycle_owner": "memory_server_managed_custom",
    })
    .to_string();
    std::fs::write(run_dir.join("status.json"), &status_body).expect("accepted status");

    let server =
        Arc::new(MemoryServer::new(home.path().join("server.sqlite"), None).expect("server"));
    let (mut receiver, _run_guard) = server
        .managed_run_controls
        .register(dispatch_id)
        .expect("register control");
    let accepted = AnchoredRunStatus::open(&run_dir).expect("open accepted anchor");
    assert!(
        server
            .managed_run_controls
            .retain_accepted_status_anchor(dispatch_id, accepted.clone())
            .expect("retain accepted anchor"),
        "the run registration must own the first accepted anchor"
    );

    let parked_run_dir = runs.path().join("parked-accepted-run");
    std::fs::rename(&run_dir, &parked_run_dir).expect("park accepted run directory");
    std::fs::create_dir_all(&run_dir).expect("replacement run directory");
    std::fs::write(run_dir.join("status.json"), &status_body).expect("replacement status");
    let replacement_before =
        std::fs::read(run_dir.join("status.json")).expect("replacement status before cancel");

    let request_server = Arc::clone(&server);
    let request = tokio::spawn(async move {
        request_managed_custom_cancel(&request_server, dispatch_id, 1).await
    });
    let command = receiver.recv().await.expect("receive cancellation command");
    assert!(
        command.status_anchor.same_opened_directory(&accepted),
        "cancellation must reuse the descriptor authority accepted before replacement"
    );
    assert!(
        command
            .response
            .send(CancelCompletion::Unavailable("test_unavailable"))
            .is_ok(),
        "return cancellation outcome"
    );
    let response: Value = serde_json::from_str(
        &request
            .await
            .expect("join cancellation request")
            .expect("cancellation response"),
    )
    .expect("response JSON");

    let accepted_status: Value = serde_json::from_slice(
        &std::fs::read(parked_run_dir.join("status.json")).expect("accepted status after cancel"),
    )
    .expect("accepted status JSON");
    assert_eq!(response["receipt"], "cancellation_unavailable");
    assert_eq!(response["reason"], "test_unavailable");
    assert_eq!(accepted_status["cancellation"], response);
    assert_eq!(
        std::fs::read(run_dir.join("status.json")).expect("replacement status after cancel"),
        replacement_before,
        "the replacement run directory must remain untouched"
    );
}

#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn managed_custom_cancel_rejects_a_symlinked_run_directory_outside_runs_root() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let outside = tempfile::tempdir().expect("outside run target");
    let runs_root = home.path().join("runs");
    std::fs::create_dir_all(&runs_root).expect("runs root");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let dispatch_id = "20260825T182601Z-symlink-escape";
    let escaped_run_dir = outside.path().join(dispatch_id);
    std::fs::create_dir_all(&escaped_run_dir).expect("escaped run directory");
    let escaped_status_path = escaped_run_dir.join("status.json");
    std::fs::write(
        &escaped_status_path,
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 7,
            "execution_classification": "managed_custom",
            "lifecycle_owner": "memory_server_managed_custom",
        })
        .to_string(),
    )
    .expect("escaped status");
    let escaped_status_before =
        std::fs::read(&escaped_status_path).expect("read escaped status before cancel");
    std::os::unix::fs::symlink(&escaped_run_dir, runs_root.join(dispatch_id))
        .expect("symlink escaped run directory");

    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _run_guard) = server
        .managed_run_controls
        .register(dispatch_id)
        .expect("register same-daemon control");

    let response: Value = serde_json::from_str(
        &request_managed_custom_cancel(&server, dispatch_id, 7)
            .await
            .expect("escaped run cancellation response"),
    )
    .expect("response JSON");
    assert_eq!(response["receipt"], "cancellation_unavailable");
    assert_eq!(response["reason"], "run_directory_outside_runs_root");
    assert_eq!(
        std::fs::read(&escaped_status_path).expect("read escaped status after cancel"),
        escaped_status_before,
        "an escaped target must not receive a cancellation mutation"
    );
    assert!(
        receiver.try_recv().is_err(),
        "an escaped target must not enqueue or send a cancellation command"
    );
}

#[cfg(not(unix))]
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn managed_custom_cancel_remains_explicitly_unavailable_on_non_unix() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let runs_root = home.path().join("runs");
    let dispatch_id = "20260825T182602Z-non-unix";
    let run_dir = runs_root.join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run directory");
    let status_path = run_dir.join("status.json");
    std::fs::write(
        &status_path,
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 7,
            "execution_classification": "managed_custom",
            "lifecycle_owner": "memory_server_managed_custom",
        })
        .to_string(),
    )
    .expect("status");
    let status_before = std::fs::read(&status_path).expect("read status before cancel");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &runs_root);
    let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
    let (mut receiver, _run_guard) = server
        .managed_run_controls
        .register(dispatch_id)
        .expect("register same-daemon control");

    let response: Value = serde_json::from_str(
        &request_managed_custom_cancel(&server, dispatch_id, 7)
            .await
            .expect("non-Unix cancellation response"),
    )
    .expect("response JSON");
    assert_eq!(response["receipt"], "cancellation_unavailable");
    assert_eq!(response["reason"], "unsupported_platform");
    assert_eq!(
        std::fs::read(&status_path).expect("read status after cancel"),
        status_before,
        "unsupported cancellation must not mutate status"
    );
    assert!(
        receiver.try_recv().is_err(),
        "unsupported cancellation must not enqueue a command"
    );
}
