//! Legacy operator helpers keep their original serve/CLI behavior; none of
//! those cases is native Staff acceptance. Native profiles have no such fallback.

use super::*;
use crate::test_support::{spawn_opencode_probe_server, EnvRestore};

#[test]
fn legacy_operator_transport_reports_opencode_serve_fallback() {
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

    let profile = resolve_dispatch_profile("deepseek_explore").unwrap();
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
fn legacy_operator_custom_profile_can_attach_to_opencode_serve() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::set("OPENCODE_SERVER_PASSWORD", "test-password");
    let (server_url, server) = spawn_opencode_probe_server();
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
        .any(|line| line.contains("OpenCode serve transport")));
}

#[test]
fn native_opencode_profile_cannot_fall_back_to_legacy_transport() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _transport = EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "serve");
    let _server = EnvRestore::set("TACHI_OPENCODE_SERVER_URL", "http://127.0.0.1:1");
    let mut params = params();
    params.profile = Some("opencode_terra_write".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(resolved.agent, "opencode");
    assert_eq!(resolved.host_adapter.as_deref(), Some("opencode"));
    assert_eq!(params.agent.as_deref(), Some("opencode"));
    assert_eq!(params.harness_transport.as_deref(), Some("acp_native"));
    assert!(
        params.command.is_empty(),
        "native stdio adapter is selected by its provider binding, never synthesized CLI arguments"
    );
    assert_eq!(params.model.as_deref(), Some("openai/gpt-6.1-terra"));
    assert!(resolved.fallback_chain.is_empty());
    assert!(params.harness_server_url.is_none());
    let (transport, readiness) = recommended_transport_for_profile(
        resolve_dispatch_profile("opencode_terra_write").unwrap(),
    );
    assert_eq!(transport, "acp_native");
    assert_eq!(readiness["requested_model"], json!("openai/gpt-6.1-terra"));
    assert_eq!(readiness["model_support"], json!("negotiated_at_start"));
    assert!(readiness.get("fallback").is_none());
    assert_eq!(
        readiness["filesystem_confinement"],
        json!(tachi_dispatch::native_sandbox_available())
    );
}

#[test]
fn legacy_operator_custom_profile_falls_back_to_cli_when_serve_is_unreachable() {
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
            "--standalone".to_string(),
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
