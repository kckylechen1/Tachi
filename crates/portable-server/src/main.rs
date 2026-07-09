//! Portable memory-kernel MCP server (tachi #924).
//!
//! A stripped server profile: the memory kernel (`portable-kernel` = memcore
//! with `admin` off) served over MCP stdio, with the save/search/get/status
//! keep-set and nothing else. No dependency on `tachi-server`, so no operator
//! or product surface can be compiled in.
//!
//! Boot: `portable-server [--global-db <path>] [--decay-policy <name>]`
//! (env fallbacks `PORTABLE_MEMORY_DB` / `PORTABLE_DECAY_POLICY`). Speaks MCP
//! over stdin/stdout — point an MCP client at this binary as a stdio server.

mod config;
mod decay;
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
