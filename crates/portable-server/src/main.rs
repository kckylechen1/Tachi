//! Portable memory-kernel MCP server (tachi #924).
//!
//! A stripped server profile: the memory kernel (`portable-kernel` = memcore
//! with `admin` off) served over MCP, with the save/search/get/status
//! keep-set and nothing else. No dependency on `tachi-server`, so no operator
//! or product surface can be compiled in.
//!
//! Boot: `portable-server [--global-db <path>] [--decay-policy <name>]
//! [--daemon] [--port <n>]` (env fallbacks `PORTABLE_MEMORY_DB` /
//! `PORTABLE_DECAY_POLICY` / `PORTABLE_DAEMON` / `PORTABLE_PORT`).
//!
//! Two transports over the same [`PortableServer`] tool surface (tachi
//! #938):
//! * default — MCP over stdin/stdout; point an MCP client at this binary as
//!   a stdio server.
//! * `--daemon` — MCP streamable-HTTP resident on `127.0.0.1:<port>`
//!   (default port `7919`; see `http.rs`), plus a `/health` probe.

mod config;
mod decay;
mod http;
mod service;

use config::{Config, IN_MEMORY};
use portable_kernel::MemoryStore;
use service::PortableServer;

fn build_server(config: Config) -> Result<PortableServer, String> {
    let store = if config.db_path == IN_MEMORY {
        MemoryStore::open_in_memory().map_err(|e| format!("open in-memory store: {e}"))?
    } else {
        MemoryStore::open(&config.db_path)
            .map_err(|e| format!("open store at {}: {e}", config.db_path))?
    };
    Ok(PortableServer::new(
        store,
        config.decay_policy,
        config.decay_policy_name,
        config.db_path,
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_args_and_env().map_err(|e| {
        eprintln!("portable-server: {e}");
        e
    })?;

    let daemon = config.daemon;
    let port = config.port;

    if daemon {
        eprintln!(
            "[portable-server] db={} decay_policy={} — serving memory kernel (save/search/get/status) over MCP streamable-HTTP on 127.0.0.1:{port}",
            config.db_path, config.decay_policy_name
        );
        let server = build_server(config)?;
        // Fail loud (#936 lesson): any exit from `serve_http` other than a
        // commanded SIGINT/SIGTERM shutdown is a bug, not something to idle
        // through — log it and exit non-zero instead of lingering deaf.
        if let Err(e) = http::serve_http(server, port).await {
            eprintln!("[portable-server] ERROR: daemon exited: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    eprintln!(
        "[portable-server] db={} decay_policy={} — serving memory kernel (save/search/get/status) over MCP stdio",
        config.db_path, config.decay_policy_name
    );

    let server = build_server(config)?;

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let running = rmcp::service::serve_server(server, transport).await?;

    tokio::select! {
        reason = running.waiting() => {
            eprintln!("[portable-server] stopped: {reason:?}");
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[portable-server] SIGINT — shutting down");
        }
    }
    Ok(())
}
