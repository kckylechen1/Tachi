use super::*;
use std::io::{Read, Write};

const OPENCODE_DOC_FIXTURE: &str = r#"{
    "openapi":"3.1.0",
    "info":{"title":"opencode","version":"1.0.0"},
    "paths":{
        "/api/session":{"post":{}},
        "/api/session/{sessionID}/prompt":{"post":{}},
        "/api/session/{sessionID}/wait":{"post":{}},
        "/api/model":{"get":{}},
        "/api/provider":{"get":{}}
    }
}"#;

fn spawn_probe_server() -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept probe");
            let mut buf = [0_u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = if request.starts_with("GET /doc ") {
                OPENCODE_DOC_FIXTURE
            } else {
                "<title>OpenCode</title>"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

struct EnvRestore {
    key: &'static str,
    old: Option<String>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(old) = &self.old {
            std::env::set_var(self.key, old);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn recommendation_transport_reports_opencode_serve_fallback() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    let _transport = EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "serve");
    let _server_url = EnvRestore::set(
        "TACHI_OPENCODE_SERVER_URL",
        &format!("http://127.0.0.1:{port}"),
    );

    let profile = resolve_dispatch_profile("opencode_builder").unwrap();
    let (transport, readiness) = recommended_transport_for_profile(profile);

    assert_eq!(transport, "opencode_cli");
    assert_eq!(readiness["requested"], json!("opencode_serve"));
    assert_eq!(readiness["fallback"], json!("opencode_cli"));
    assert_eq!(
        readiness["harness_server_status"]["reachable"],
        json!(false)
    );
}

#[test]
fn custom_profile_can_attach_to_opencode_serve() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::set("OPENCODE_SERVER_PASSWORD", "test-password");
    let (server_url, server) = spawn_probe_server();
    let mut params = params();
    params.profile = Some("deepseek_explore".to_string());
    params.cwd = Some("/tmp/tachi-opencode-project".to_string());
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(server_url.clone());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    server.join().expect("probe server thread");

    assert_eq!(resolved.agent, "custom");
    assert_eq!(
        params.command,
        vec![
            "opencode".to_string(),
            "run".to_string(),
            "--attach".to_string(),
            server_url,
            "--dir".to_string(),
            "/tmp/tachi-opencode-project".to_string(),
            "--agent".to_string(),
            "explore".to_string(),
            "--model".to_string(),
            "deepseek/deepseek-v4-flash".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("opencode serve transport")));
}

#[test]
fn custom_profile_falls_back_to_cli_when_opencode_serve_is_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);

    let mut params = params();
    params.profile = Some("deepseek_explore".to_string());
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(format!("http://127.0.0.1:{port}"));
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(resolved.agent, "custom");
    assert_eq!(params.harness_transport.as_deref(), Some("opencode_cli"));
    assert_eq!(
        params.command,
        vec![
            "opencode".to_string(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            "deepseek/deepseek-v4-flash".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("falling back to opencode CLI")));
}
