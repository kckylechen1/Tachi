//! Separately authenticated remote transport. It never mounts the local MCP router.

mod admission;
mod config;
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
        | RemoteA2aAction::Probe { config, .. }
        | RemoteA2aAction::Revoke { config, .. } => config,
    };
    let config = GatewayConfig::load(path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if let RemoteA2aAction::Probe { peer, .. } = action {
            let result = transport::probe(&config, peer).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        // Set the process home before constructing any server/runtime, without
        // loading user/project dotenv or resolving an implicit live database.
        std::env::set_var("TACHI_HOME", &config.home);
        std::env::remove_var(memcore::db::SCHEMA_MIGRATION_LEGACY_ENV);
        let migration = if allow_schema_migration {
            memcore::MigrationAuthority::Allow {
                approved_by: "a2a remote --allow-schema-migration".into(),
            }
        } else {
            memcore::MigrationAuthority::Deny
        };
        let server = crate::MemoryServer::new_without_background_workers(
            config.home.join("global.db"),
            None,
            migration,
            config.home.clone(),
        )?;
        match action {
            RemoteA2aAction::Serve { .. } => transport::serve(config, server).await,
            RemoteA2aAction::Revoke { admission, .. } => {
                admission::revoke(&server, &config, admission)?;
                println!("{{\"revoked\":true,\"scope\":\"mtls-peer-key\"}}");
                Ok(())
            }
            RemoteA2aAction::Probe { .. } => unreachable!(),
        }
    })
}
