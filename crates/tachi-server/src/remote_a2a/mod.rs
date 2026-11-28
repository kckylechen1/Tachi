//! Separately authenticated remote transport. It never mounts the local MCP router.

mod admission;
mod config;
mod executor;
mod profile;
mod rpc;
mod submissions;
mod transport;

use config::GatewayConfig;
use std::error::Error;
use tachi_bootstrap::cli::RemoteA2aAction;

pub(crate) fn run(
    action: &RemoteA2aAction,
    allow_schema_migration: bool,
) -> Result<(), Box<dyn Error>> {
    let path = match action {
        RemoteA2aAction::Serve { config }
        | RemoteA2aAction::Submit { config, .. }
        | RemoteA2aAction::Resume { config, .. }
        | RemoteA2aAction::Status { config, .. }
        | RemoteA2aAction::Cancel { config, .. }
        | RemoteA2aAction::Probe { config, .. }
        | RemoteA2aAction::Revoke { config, .. } => config,
    };
    let config = GatewayConfig::load(path)?;
    let probe = match action {
        RemoteA2aAction::Probe { peer, .. } => Some(peer),
        _ => None,
    };
    if probe.is_none() {
        // Set the process home before constructing any server, and before the
        // runtime's workers exist: `set_var` must not race another thread's
        // `getenv`. User/project dotenv is not loaded and no implicit live
        // database is resolved.
        std::env::set_var("TACHI_HOME", &config.home);
        std::env::remove_var(memcore::db::SCHEMA_MIGRATION_LEGACY_ENV);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if let Some(peer) = probe {
            let result = transport::probe(&config, peer).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        let migration = if allow_schema_migration {
            memcore::MigrationAuthority::Allow {
                approved_by: "a2a remote --allow-schema-migration".into(),
            }
        } else {
            memcore::MigrationAuthority::Deny
        };
        let database = match action {
            RemoteA2aAction::Submit { .. }
            | RemoteA2aAction::Resume { .. }
            | RemoteA2aAction::Status { .. }
            | RemoteA2aAction::Cancel { .. } => config
                .origin_database
                .clone()
                .unwrap_or_else(|| config.home.join("global.db")),
            _ => config.home.join("global.db"),
        };
        let supervisor_lock = if matches!(action, RemoteA2aAction::Serve { .. }) {
            Some(
                transport::acquire_gateway_supervisor_lock(&config.canonical_home).map_err(
                    |err| format!("cannot acquire remote gateway supervisor lock: {err}"),
                )?,
            )
        } else {
            None
        };
        let server = crate::MemoryServer::new_without_background_workers(
            database,
            None,
            migration.clone(),
            config.home.clone(),
        )?;
        match action {
            RemoteA2aAction::Serve { .. } => {
                transport::serve(
                    config,
                    server,
                    supervisor_lock.expect("serve acquired lock"),
                )
                .await
            }
            RemoteA2aAction::Revoke { admission, .. } => {
                admission::revoke(&server, &config, admission)?;
                println!("{{\"revoked\":true,\"scope\":\"mtls-peer-key\"}}");
                Ok(())
            }
            // A probe returned above, before any store was opened.
            RemoteA2aAction::Probe { .. } => unreachable!("probe returned before the store"),
            RemoteA2aAction::Submit { .. }
            | RemoteA2aAction::Resume { .. }
            | RemoteA2aAction::Status { .. }
            | RemoteA2aAction::Cancel { .. } => {
                // Origin WorkClaims may live in the ordinary daemon's store;
                // Serve/Revoke retain the gateway authority ledger.
                let authority = if config.origin_database.is_some() {
                    crate::MemoryServer::new_without_background_workers(
                        config.home.join("global.db"),
                        None,
                        migration,
                        config.home.clone(),
                    )?
                } else {
                    server.clone()
                };
                submissions::run(&server, &authority, &config, action).await
            }
        }
    })
}
