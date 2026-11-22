use super::*;
use std::time::Duration;

async fn subscribe(
    url: &str,
    identity: Option<&str>,
    project: &str,
    profile: &str,
) -> (
    reqwest::Client,
    reqwest::header::HeaderMap,
    tokio::sync::mpsc::Receiver<serde_json::Value>,
    tokio::task::JoinHandle<()>,
) {
    let client = task_client();
    let mut pairs = vec![
        ("x-tachi-project", project),
        ("x-tachi-profile", profile),
        ("x-tachi-client", "reiclaw"),
    ];
    if let Some(identity) = identity {
        pairs.push(("x-tachi-agent-identity", identity));
    }
    let mut headers = http_headers(&pairs);
    let init = client
        .post(url)
        .headers(headers.clone())
        .json(&json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"reiclaw","version":"test"}
            }
        }))
        .send()
        .await
        .unwrap();
    headers.insert(
        "mcp-session-id",
        init.headers()
            .get("mcp-session-id")
            .expect("legacy session")
            .clone(),
    );
    let result = parse_http_mcp_payload(&init.text().await.unwrap(), 1);
    assert!(result.get("error").is_none(), "{result:#}");
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    http_mcp_initialized(&client, url, headers.clone()).await;
    let mut response = client
        .get(url)
        .headers(headers.clone())
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let reader = tokio::spawn(async move {
        let mut pending = String::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            pending.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = pending.find('\n') {
                let line = pending[..end].trim_end_matches('\r').to_string();
                pending.drain(..=end);
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                        if value["method"] == "notifications/tasks" && tx.send(value).await.is_err()
                        {
                            return;
                        }
                    }
                }
            }
        }
    });
    (client, headers, rx, reader)
}

#[test]
fn legacy_sse_push_is_owner_scoped_and_does_not_mint_tasks() {
    let home = tempfile::tempdir().unwrap();
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        let other = server.tachi_home_dir().join("projects/another-project");
        std::fs::create_dir_all(&other).unwrap();
        memcore::MemoryStore::open_with_label(
            other.join(memcore::MEMORY_DB_FILENAME).to_str().unwrap(),
            "project:another-project",
        )
        .unwrap();
        test_runtime().block_on(async {
            let (daemon, stop, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let (client, headers, mut owner, owner_reader) =
                subscribe(&daemon.url, Some(OWNER), PROJECT, "standard").await;
            let (_, _, mut foreign, foreign_reader) =
                subscribe(&daemon.url, Some("agent.intruder"), PROJECT, "standard").await;
            let (_, _, mut wrong_project, wrong_reader) =
                subscribe(&daemon.url, Some(OWNER), "another-project", "standard").await;
            let (_, _, mut observer, observer_reader) =
                subscribe(&daemon.url, Some(OWNER), PROJECT, "observe").await;
            let (_, _, mut anonymous, anonymous_reader) =
                subscribe(&daemon.url, None, PROJECT, "standard").await;
            let original_binding = receipt_bytes(&run).1;
            for (revision, state, expected) in [
                (2, "TASK_STATE_INPUT_REQUIRED", "input_required"),
                (3, "TASK_STATE_COMPLETED", "completed"),
                (4, "TASK_STATE_FAILED", "failed"),
                (5, "TASK_STATE_CANCELED", "cancelled"),
            ] {
                write_run_status(&run, state, revision);
                server.task_notifications.notify(DISPATCH);
                let hint = tokio::time::timeout(Duration::from_secs(2), owner.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(hint["jsonrpc"], "2.0");
                assert!(hint.get("id").is_none());
                assert_eq!(hint["params"]["status"], expected);
                let observed_hint = tokio::time::timeout(Duration::from_secs(2), observer.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    observed_hint["params"]["status"], expected,
                    "observe permits result reads under the current profile contract"
                );
                let task_id = hint["params"]["taskId"].as_str().unwrap();
                assert_eq!(
                    crate::mcp_tasks::dispatch_from_task(task_id).unwrap(),
                    DISPATCH
                );
                assert_eq!(
                    receipt_bytes(&run).1,
                    original_binding,
                    "a wake hint cannot create a Tasks grant"
                );
            }
            for receiver in [&mut foreign, &mut wrong_project, &mut anonymous] {
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), receiver.recv())
                        .await
                        .is_err(),
                    "no dispatch identifiers may reach another scope"
                );
            }
            // A Tasks result handle, once issued, remains the notification id.
            write_run_status(&run, "TASK_STATE_WORKING", 6);
            let (_, created) = task_rpc(
                &client,
                &daemon.url,
                20,
                "tools/call",
                result_params(task_meta(OWNER)),
                &[],
            )
            .await;
            write_run_status(&run, "TASK_STATE_COMPLETED", 7);
            server.task_notifications.notify(DISPATCH);
            let hint = tokio::time::timeout(Duration::from_secs(2), owner.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(hint["params"]["taskId"], created["result"]["taskId"]);
            let before = server.task_notifications.peer_count();
            client
                .delete(&daemon.url)
                .headers(headers)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            owner_reader.abort();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while server.task_notifications.peer_count() >= before
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(
                server.task_notifications.peer_count() < before,
                "closed peers are pruned without another status write"
            );
            write_run_status(&run, "TASK_STATE_COMPLETED", 8);
            server.task_notifications.notify(DISPATCH);
            for reader in [
                foreign_reader,
                wrong_reader,
                observer_reader,
                anonymous_reader,
            ] {
                reader.abort();
            }
            stop.cancel();
            task.await.unwrap();
        });
    });
}
