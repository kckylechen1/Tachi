//! Streamable-HTTP `--daemon` transport for portable-server (tachi #938).
//!
//! Mirrors the full tachi-server daemon's streamable-HTTP shape
//! (`crates/tachi-server/src/bootstrap/serve/daemon.rs`) at portable scale:
//! same `rmcp::transport::streamable_http_server` + axum `.nest_service`
//! pattern, same `/health` route, same 127.0.0.1-only bind, same tool
//! surface as stdio mode (one [`PortableServer`] service object, two
//! transports). Dropped at portable scale because this crate has no
//! `tachi-server` dependency by construction (see `service.rs`'s module
//! doc): the PR-4 singleton daemon lock, foundry/manifest schedulers, idle
//! reaper, and pid discovery file. This is a plain transport swap, not a
//! daemon-lifecycle port.

use std::sync::Arc;

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::service::PortableServer;

/// Bind loopback-only and serve `server`'s tool surface over streamable HTTP
/// at `/mcp`, plus a `/health` probe at `/health`.
///
/// Loopback (`127.0.0.1`) is hard-coded — there is deliberately no `--bind`
/// flag. This profile's auth posture is loopback-trust (mirroring the full
/// daemon's #732 posture): no bearer-token or ACL layer exists here, so
/// accepting a bind target on anything other than loopback would silently
/// turn "resident on this machine" into "reachable from the network" with
/// no explicit opt-in step from the operator.
///
/// Returns `Ok(())` only on a commanded shutdown (SIGINT/SIGTERM). Any other
/// exit — the serve future returning on its own, or a bind/accept I/O error
/// — is reported as `Err` so the caller fails loud instead of lingering deaf
/// (the #936 lesson: a daemon that swallows a fatal serve error and idles
/// forever looks alive to supervisors while doing nothing).
pub async fn serve_http(
    server: PortableServer,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (listener, local_addr) = bind_loopback(port).await?;
    eprintln!("[portable-server] daemon listening on http://{local_addr}");
    run(listener, server).await
}

/// Bind the loopback listener and report the actual local address (`--port
/// 0` resolves to an OS-assigned ephemeral port — used by tests).
async fn bind_loopback(
    port: u16,
) -> std::io::Result<(tokio::net::TcpListener, std::net::SocketAddr)> {
    let bind_addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
}

/// Build the axum router (`/health` + `/mcp`) and serve it on `listener`
/// until a shutdown signal arrives or the serve future exits on its own.
async fn run(
    listener: tokio::net::TcpListener,
    server: PortableServer,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let health_server = server.clone();
    let http_config = StreamableHttpServerConfig::default(); // stateful_mode: true

    let mcp_service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let router = axum::Router::new()
        .route(
            "/health",
            axum::routing::get(move || {
                let health_server = health_server.clone();
                async move {
                    let db_ok = health_server.health_ok();
                    let code = if db_ok {
                        axum::http::StatusCode::OK
                    } else {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    };
                    (code, axum::Json(health_payload(db_ok)))
                }
            }),
        )
        .nest_service("/mcp", mcp_service);

    // No `with_graceful_shutdown`/cancellation-token wiring: `tokio::select!`
    // drops whichever branch didn't resolve, so the signal branches below
    // are already "stop serving now" — the extra machinery the full daemon
    // carries (idle reaper, singleton lock, manifest scheduler) doesn't
    // apply at portable scale (see module doc).
    tokio::select! {
        result = axum::serve(listener, router) => {
            match result {
                Ok(()) => Err("HTTP server exited unexpectedly (no shutdown signal received)".into()),
                Err(e) => Err(format!("HTTP server failed: {e}").into()),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[portable-server] SIGINT — shutting down");
            Ok(())
        }
        _ = sigterm() => {
            eprintln!("[portable-server] SIGTERM — shutting down");
            Ok(())
        }
    }
}

/// `/health` JSON body. Auth posture v1 is loopback-trust (see [`serve_http`]
/// doc) — advertised explicitly so a client hitting this from off-box (via an
/// operator-added reverse proxy/tunnel, since this crate offers no `--bind`
/// escape hatch) can tell the posture was never multi-user-safe by design.
fn health_payload(db_ok: bool) -> serde_json::Value {
    serde_json::json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "profile": "portable",
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "http",
        "mcp": "streamable-http",
        "bind": "127.0.0.1",
        "auth_posture": "loopback-trust-v1",
        "db_ready": db_ok,
    })
}

/// Resolves on SIGTERM (Unix only); never resolves on non-Unix hosts, so the
/// `tokio::select!` branch that awaits this one simply never fires there.
async fn sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portable_kernel::MemoryStore;

    fn boot() -> PortableServer {
        let store = MemoryStore::open_in_memory().expect("open_in_memory");
        PortableServer::new(store, None, "default".to_string(), ":memory:".to_string())
    }

    /// Install the rustls `ring` crypto provider as the process default —
    /// see the `rustls` line in `Cargo.toml`'s `[dev-dependencies]` for why
    /// this is needed at all (mirrors `tachi-llm::install_tls_provider`).
    /// Idempotent: safe to call from every test in this module.
    fn install_tls_provider() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    #[test]
    fn health_payload_advertises_loopback_trust() {
        let ok = health_payload(true);
        assert_eq!(ok["status"], "ok");
        assert_eq!(ok["profile"], "portable");
        assert_eq!(ok["transport"], "http");
        assert_eq!(ok["mcp"], "streamable-http");
        assert_eq!(ok["bind"], "127.0.0.1");
        assert_eq!(ok["auth_posture"], "loopback-trust-v1");
        assert_eq!(ok["db_ready"], true);

        let degraded = health_payload(false);
        assert_eq!(degraded["status"], "degraded");
        assert_eq!(degraded["db_ready"], false);
    }

    /// Bind an ephemeral port (`--port 0` equivalent), then prove the daemon
    /// serves both `/health` (plain HTTP GET) and `/mcp` (a real MCP
    /// `initialize` + `tools/list` round trip via rmcp's streamable-HTTP
    /// client transport) on the same loopback listener — same tool surface
    /// as stdio mode, per #938 item 3/6.
    #[tokio::test]
    async fn daemon_serves_health_and_mcp_initialize_over_loopback_http() {
        install_tls_provider();
        let (listener, local_addr) = bind_loopback(0).await.expect("bind ephemeral port");
        assert_eq!(
            local_addr.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "must bind loopback only"
        );

        let serve_task = tokio::spawn(run(listener, boot()));

        // /health answers 200 over plain HTTP.
        let health_url = format!("http://{local_addr}/health");
        let resp = tokio::time::timeout(std::time::Duration::from_secs(5), reqwest::get(&health_url))
            .await
            .expect("GET /health timed out")
            .expect("GET /health");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("health json");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["bind"], "127.0.0.1");

        // MCP initialize + tools/list round-trips over streamable HTTP at /mcp.
        use rmcp::transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        };
        use rmcp::ServiceExt;

        let mcp_url = format!("http://{local_addr}/mcp");
        let transport_config = StreamableHttpClientTransportConfig::with_uri(mcp_url);
        let transport = StreamableHttpClientTransport::from_config(transport_config);
        let client = tokio::time::timeout(std::time::Duration::from_secs(5), ServiceExt::serve((), transport))
            .await
            .expect("mcp initialize timed out")
            .expect("mcp initialize handshake");

        let peer = client.peer().clone();
        let tools = tokio::time::timeout(std::time::Duration::from_secs(5), peer.list_tools(None))
            .await
            .expect("tools/list timed out")
            .expect("tools/list");
        let names: Vec<String> = tools.tools.iter().map(|t| t.name.to_string()).collect();
        assert!(
            names.contains(&"save".to_string()) && names.contains(&"status".to_string()),
            "expected the stdio tool surface (save/search/get/status) over HTTP too, got {names:?}"
        );

        drop(client);
        serve_task.abort();
    }
}
