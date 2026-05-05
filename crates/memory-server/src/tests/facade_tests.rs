use super::*;

// ─── cli_client: daemon detection + in-process fallback ─────────────────────

#[tokio::test]
async fn cli_client_detect_daemon_returns_none_when_pid_file_missing() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    let info = crate::cli_client::detect_daemon(&temp).await;
    assert!(
        info.is_none(),
        "expected None when ~/.tachi/daemon.pid is missing"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_detect_daemon_returns_none_for_stale_pid_file() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    // Write a pid file pointing at a port nobody is listening on. Pick a high
    // port that is extremely unlikely to be in use during the test.
    let pid_path = temp.join("daemon.pid");
    std::fs::write(
        &pid_path,
        serde_json::to_string(&json!({
            "pid": 99999,
            "port": 1u16,           // privileged port we won't be bound to
            "url": "http://127.0.0.1:1/mcp",
            "global_db": "/tmp/none.db",
            "project_db": null,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = crate::cli_client::detect_daemon(&temp).await;
    assert!(
        info.is_none(),
        "expected None when port in pid file is not listening"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_detect_daemon_succeeds_when_port_is_open() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    // Bind a real listener on an OS-assigned port so the TCP probe succeeds.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let port = addr.port();

    let pid_path = temp.join("daemon.pid");
    std::fs::write(
        &pid_path,
        serde_json::to_string(&json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "global_db": "/tmp/none.db",
            "project_db": null,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = crate::cli_client::detect_daemon(&temp)
        .await
        .expect("expected Some(DaemonInfo) when port is listening");
    assert_eq!(info.port, port);
    assert!(info.url.contains(&format!("127.0.0.1:{port}")));

    drop(listener);
    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_in_process_remember_round_trips_through_handler() {
    // Verifies the in-process fallback path: build a transient MemoryServer
    // and call the same `handle_remember` the MCP tool uses. This is the
    // critical guarantee that `tachi remember` from the shell behaves
    // identically to the `remember` MCP tool when no daemon is running.
    ensure_test_env();

    let db_path = std::env::temp_dir().join(format!(
        "tachi-cli-remember-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = crate::cli_client::build_in_process_server(&db_path, None)
        .expect("build in-process server");

    let body = crate::memory_search_ops::handle_remember(
        &server,
        crate::tool_params::RememberParams {
            text: "cli round-trip note about a single concrete fact".to_string(),
            summary: String::new(),
            tags: vec!["cli-test".to_string()],
            topic: String::new(),
            importance: Some(0.6),
            scope: Some("project".to_string()),
            project: None,
            path: Some("/notes/cli-roundtrip".to_string()),
            category: None,
            domain: None,
            retention_policy: None,
            force: true, // bypass noise filter for the deterministic test string
        },
    )
    .await
    .expect("remember should succeed in-process");

    let parsed: Value = serde_json::from_str(&body).expect("remember body is JSON");
    // handle_remember delegates to handle_save_memory which returns either
    // {"saved": true, ...} or {"id": "...", ...} depending on the path; we
    // only need to assert the call landed without an error key.
    assert!(
        parsed.get("error").is_none(),
        "remember returned error: {body}"
    );

    let _ = std::fs::remove_file(&db_path);
}
