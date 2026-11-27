//! The loopback daemon answers only requests that name it, and `/mcp` only
//! clients that are not a browser page on another site.
use super::*;

#[tokio::test]
async fn health_and_vault_routes_reject_a_host_that_is_not_the_loopback_daemon() {
    let fixture = tempfile::tempdir().unwrap();
    let global = fixture.path().join("memory.db");
    let server = crate::MemoryServer::new(global.clone(), None).unwrap();
    let (daemon, cancel, task) = spawn_test_http_daemon(server.clone(), &global).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let live = daemon.url.replace("/mcp", "/health/live");
    let health = daemon.url.replace("/mcp", "/health");
    let vault = daemon.url.replace("/mcp", "/internal/vault");
    let capability = crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN;

    // The daemon's own names still reach every route.
    for host in [
        "127.0.0.1",
        "127.0.0.1:6919",
        "localhost",
        "localhost:6919",
        "LOCALHOST:6919",
        "[::1]",
        "[::1]:6919",
    ] {
        let status = client
            .get(&live)
            .header(reqwest::header::HOST, host)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::OK, "Host {host}");
    }

    // A name a page on another site controls is the DNS-rebinding path: it
    // resolved to 127.0.0.1, so the request arrives, but it is not the daemon.
    for host in [
        "attacker.example",
        "attacker.example:6919",
        "127.0.0.1.attacker.example",
        "localhost.attacker.example",
        "127.0.0.1@attacker.example",
        "127.0.0.1:",
        "[::1",
        "[::1]junk",
        "[localhost]",
        "[127.0.0.1]",
    ] {
        let status = client
            .get(&live)
            .header(reqwest::header::HOST, host)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "Host {host:?}");
        let status = client
            .get(&health)
            .header(reqwest::header::HOST, host)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            reqwest::StatusCode::FORBIDDEN,
            "health payload for Host {host:?}"
        );
        // Even with the real owner capability the vault channel stays shut.
        let status = client
            .post(&vault)
            .header(reqwest::header::HOST, host)
            .header(capability, daemon.internal_proxy_token.as_deref().unwrap())
            .json(&serde_json::json!({"action":"status"}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            reqwest::StatusCode::FORBIDDEN,
            "vault channel for Host {host:?}"
        );
    }
    assert!(
        server.vault_read().key.is_none(),
        "no rejected request changed vault state"
    );

    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn mcp_refuses_a_browser_origin_and_admits_clients_that_send_none() {
    let fixture = tempfile::tempdir().unwrap();
    let global = fixture.path().join("memory.db");
    let server = crate::MemoryServer::new(global.clone(), None).unwrap();
    let (daemon, cancel, task) = spawn_test_http_daemon(server.clone(), &global).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let initialize = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},
                  "clientInfo":{"name":"origin-boundary-probe","version":"0"}}
    });

    // A CLI or MCP host sends no Origin.
    let status = client
        .post(&daemon.url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&initialize)
        .send()
        .await
        .unwrap()
        .status();
    assert_ne!(
        status,
        reqwest::StatusCode::FORBIDDEN,
        "a client without an Origin is not refused"
    );

    // A browser page always names its site, including an opaque one.
    for origin in [
        "https://attacker.example",
        "http://localhost:8080",
        "http://127.0.0.1:6919",
        "null",
    ] {
        let status = client
            .post(&daemon.url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ORIGIN, origin)
            .json(&initialize)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "Origin {origin}");
    }

    cancel.cancel();
    task.await.unwrap();
}

#[test]
fn loopback_host_names_follow_the_mcp_allowlist() {
    for host in [
        "localhost",
        "LOCALHOST",
        "127.0.0.1",
        "127.0.0.1:6919",
        "[::1]",
        "[::1]:1",
    ] {
        assert!(super::super::daemon::is_loopback_host(host), "{host}");
    }
    for host in [
        "example.com",
        "127.0.0.1.example.com",
        "127.0.0.1:abc",
        "127.0.0.1:",
        "0.0.0.0",
        "::1",
        "[::1",
        "[::1]junk",
        "[::1].attacker.example",
        "[::1]:",
        "[::1]:x",
        "[::1]:99999",
        "[localhost]",
        "[localhost]:6919",
        "[127.0.0.1]",
        "127.0.0.1:99999",
        "",
    ] {
        assert!(!super::super::daemon::is_loopback_host(host), "{host:?}");
    }
}
