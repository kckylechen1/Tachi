use super::*;
use crate::dispatch_ops::probe_harness_server_status;
use crate::test_support::{spawn_opencode_probe_server, EnvRestore};
use serde_json::json;
use std::ffi::OsStr;
use std::io::{Read, Write};

#[test]
fn dispatch_timestamp_key_extracts_embedded_timestamp() {
    assert_eq!(
        dispatch_timestamp_key(OsStr::new("flow_20260606T151945Z_tachi")),
        Some("20260606T151945Z".to_string())
    );
    assert_eq!(
        dispatch_timestamp_key(OsStr::new("20260607T045032Z-codex-09802a7f")),
        Some("20260607T045032Z".to_string())
    );
    assert_eq!(dispatch_timestamp_key(OsStr::new("mcp-smoke-test")), None);
}

#[test]
fn harness_probe_rejects_non_local_urls() {
    let status = probe_harness_server_status(Some("https://example.com:4321"));
    assert_eq!(status["reachable"], serde_json::Value::Null);
    assert_eq!(status["evidence_strength"], json!("none"));
}

#[test]
fn harness_probe_labels_tcp_only_as_weak_evidence() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local listener");
    let port = listener.local_addr().expect("local addr").port();

    let status = probe_harness_server_status(Some(&format!("http://127.0.0.1:{port}")));

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["probe"], json!("tcp"));
    assert_eq!(status["evidence_strength"], json!("weak"));
    assert_eq!(status["readiness"], json!("tcp_only"));
    assert!(
        status["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("OpenCode API version")),
        "TCP-only probe must explain what it did not verify: {status:#}"
    );
}

#[test]
fn harness_probe_reports_http_responsive_readiness() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local listener");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept probe");
        let mut buf = [0_u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\nopencode service")
            .expect("write response");
    });

    let status = probe_harness_server_status(Some(&format!("http://127.0.0.1:{port}")));
    server.join().expect("probe server thread");

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["probe"], json!("http"));
    assert_eq!(status["evidence_strength"], json!("medium"));
    assert_eq!(status["readiness"], json!("http_responsive"));
    assert_eq!(status["layers"]["tcp_reachable"], json!("passed"));
    assert_eq!(status["layers"]["http_health"], json!("passed"));
    assert_eq!(status["opencode_hint"], json!(true));
}

#[test]
fn harness_probe_requires_password_for_opencode_api_attach_ready() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::remove("OPENCODE_SERVER_PASSWORD");
    let (server_url, server) = spawn_opencode_probe_server();

    let status = probe_harness_server_status(Some(&server_url));
    server.join().expect("probe server thread");

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["attach_ready"], json!(false));
    assert_eq!(status["readiness"], json!("server_auth_required"));
    assert_eq!(status["layers"]["opencode_api_version"], json!("passed"));
    assert_eq!(
        status["layers"]["session_create_smoke"],
        json!("route_available")
    );
    assert_eq!(
        status["layers"]["credential_ready"],
        json!("unsafe_to_probe")
    );
    assert!(status["sensitive_endpoints_skipped"]
        .as_array()
        .is_some_and(|items| items.contains(&json!("/api/model"))));
}

#[test]
fn harness_probe_reports_attach_ready_for_authenticated_opencode_schema() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::set("OPENCODE_SERVER_PASSWORD", "test-password");
    let (server_url, server) = spawn_opencode_probe_server();

    let status = probe_harness_server_status(Some(&server_url));
    server.join().expect("probe server thread");

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["attach_ready"], json!(true));
    assert_eq!(status["readiness"], json!("attach_ready"));
    assert_eq!(status["evidence_strength"], json!("strong"));
    assert_eq!(
        status["api_capabilities"]["routes"]["session_create"],
        json!(true)
    );
    assert_eq!(
        status["layers"]["model_available"],
        json!("unsafe_to_probe")
    );
}
