use super::*;

#[tokio::test]
async fn runtime_info_reports_identity_and_db_routing() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("openclaw").expect("openclaw profile should parse"),
    ));

    let info = server
        .runtime_info()
        .await
        .expect("runtime_info should serialize");
    let value: serde_json::Value = serde_json::from_str(&info).expect("runtime_info JSON");
    assert_eq!(value["runtime"]["name"], json!("tachi"));
    assert_eq!(
        value["runtime"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        value["runtime"]["tool_profile"],
        json!("observe,remember,operate")
    );
    assert!(value["databases"]["global"]["path"].as_str().is_some());
    assert_eq!(value["databases"]["project"], serde_json::Value::Null);
    assert_eq!(value["databases"]["single_db_mode"], json!(true));
    assert!(value["process"]["pid"].as_u64().is_some());
    let process_role = value["process"]["process_role"]
        .as_str()
        .expect("process_role");
    assert!(
        matches!(process_role, "embedded_stdio" | "stdio_daemon_client"),
        "unexpected process_role: {process_role}"
    );
    let authoritative_runtime = value["process"]["authoritative_runtime"]
        .as_str()
        .expect("authoritative_runtime");
    assert!(
        matches!(authoritative_runtime, "current_process" | "daemon"),
        "unexpected authoritative_runtime: {authoritative_runtime}"
    );
    assert_eq!(value["process"]["stdio_adapter"], json!(true));
    assert!(value["process"]["write_forwarding"]["expected"]
        .as_bool()
        .is_some());
    assert!(value["process"]["provider_secret_count"].as_u64().is_some());
    assert_eq!(value["process"]["vault"]["unlocked"], json!(false));
}

#[tokio::test]
async fn runtime_observability_treats_stale_daemon_pid_as_single_process() {
    let (server, temp_home) = make_server_with_temp_home();
    let app_home = temp_home.temp_home.join(".tachi");
    std::fs::create_dir_all(&app_home).expect("app home");
    std::fs::write(app_home.join("daemon.lock"), "999999").expect("daemon lock");

    let runtime = crate::status_ops::runtime_observability_json(&server, &app_home, None, false);

    assert_eq!(runtime["daemon"]["pid"], json!(999999));
    assert_eq!(runtime["daemon"]["running"], json!(false));
    assert_eq!(runtime["mode"], json!("single_process"));
    assert_eq!(runtime["process_role"], json!("embedded_stdio"));
    assert_eq!(runtime["authoritative_runtime"], json!("current_process"));
    assert_eq!(runtime["write_forwarding"]["expected"], json!(false));
}

#[tokio::test]
async fn runtime_observability_marks_stdio_daemon_client_when_daemon_is_alive() {
    let server = make_server();
    let app_home = tempfile::tempdir().expect("app home");
    let mut child = std::process::Command::new("python3")
        .args(["-c", "import time; time.sleep(30)"])
        .spawn()
        .expect("spawn live daemon stand-in");
    let runtime = crate::status_ops::runtime_observability_json(
        &server,
        app_home.path(),
        Some(&crate::status_ops::DaemonStatus::Running {
            pid: child.id() as i32,
            lock_path: app_home.path().join("daemon.lock"),
        }),
        false,
    );
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(runtime["mode"], json!("sidecar_or_stdio"));
    assert_eq!(runtime["process_role"], json!("stdio_daemon_client"));
    assert_eq!(runtime["authoritative_runtime"], json!("daemon"));
    assert_eq!(runtime["stdio_adapter"], json!(true));
    assert_eq!(runtime["write_forwarding"]["expected"], json!(true));
    assert_eq!(runtime["write_forwarding"]["target"], json!("daemon"));
}

#[tokio::test]
async fn runtime_observability_does_not_forward_to_foreign_daemon() {
    let server = make_server();
    let app_home = tempfile::tempdir().expect("app home");
    let mut child = std::process::Command::new("python3")
        .args(["-c", "import time; time.sleep(30)"])
        .spawn()
        .expect("spawn live foreign daemon stand-in");
    let runtime = crate::status_ops::runtime_observability_json(
        &server,
        app_home.path(),
        Some(&crate::status_ops::DaemonStatus::Foreign {
            pid: child.id() as i32,
            lock_path: app_home.path().join("daemon.lock"),
            reason: "daemon global_db /tmp/openclaw.db does not match /tmp/tachi.db".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            port: Some(6919),
            global_db: Some("/tmp/openclaw.db".to_string()),
        }),
        false,
    );
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(runtime["mode"], json!("single_process"));
    assert_eq!(runtime["process_role"], json!("embedded_stdio"));
    assert_eq!(runtime["authoritative_runtime"], json!("current_process"));
    assert_eq!(runtime["daemon"]["process_running"], json!(true));
    assert_eq!(runtime["daemon"]["running"], json!(false));
    assert_eq!(runtime["daemon"]["foreign"], json!(true));
    assert_eq!(runtime["daemon"]["authoritative"], json!(false));
    assert_eq!(runtime["write_forwarding"]["expected"], json!(false));
    assert_eq!(
        runtime["write_forwarding"]["target"],
        json!("current_process")
    );
}

// ─── Rate Limiter Tests ──────────────────────────────────────────────────────
