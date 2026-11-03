use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use tachi_bootstrap::cli::ConnectionAction;

use super::keys::{read_vault_config_for_key, read_verified_vault_key};
use super::open_cli_store;
use crate::vault_ops::connections::{
    catalog_path, context_for_cli, load_catalog, parse_connection, put_connection,
    remove_connection, select_connection, ssh_remote_command, ConnectionTarget, MAX_CATALOG_BYTES,
};

type CliResult = Result<(), Box<dyn std::error::Error>>;

/// Reuse the existing env-run subprocess boundary. The MCP prepare operation
/// only describes this command; it never invokes this handler or authorizes it.
pub(super) fn run_connection_action(
    global_db_path: &PathBuf,
    action: ConnectionAction,
) -> CliResult {
    run_connection_action_with_runner(
        global_db_path,
        action,
        crate::bootstrap::env_cmd::run_with_project_env,
    )
}

fn run_connection_action_with_runner(
    global_db_path: &PathBuf,
    action: ConnectionAction,
    run: impl FnOnce(&Path, &[String], &[(String, String)]) -> CliResult,
) -> CliResult {
    match action {
        ConnectionAction::List => print_json(&load_catalog(global_db_path)?),
        ConnectionAction::Show { name } => {
            let catalog = load_catalog(global_db_path)?;
            let connection = catalog
                .connections
                .iter()
                .find(|connection| connection.name == name)
                .ok_or("Connection is not registered")?;
            print_json(connection)
        }
        ConnectionAction::Put { file, replace } => {
            let mut bytes = Vec::new();
            std::fs::File::open(file)?
                .take(MAX_CATALOG_BYTES + 1)
                .read_to_end(&mut bytes)?;
            let connection = parse_connection(&bytes)?;
            print_json(&put_connection(global_db_path, connection, replace)?)
        }
        ConnectionAction::Remove { name } => print_json(&remove_connection(global_db_path, &name)?),
        ConnectionAction::Context { cwd } => {
            let cwd = resolve_cwd(cwd)?;
            let context = context_for_cli(&cwd);
            print_json(&serde_json::json!({
                "node": context.node,
                "project": context.project,
                "agent_id": context.agent_id,
                "cwd": cwd,
                "catalog_path": catalog_path(global_db_path),
            }))
        }
        ConnectionAction::Run {
            name,
            expect_revision,
            cwd,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
            command,
        } => {
            // Read the current registration on every invocation. A prepared
            // command is neither a cached secret nor an authorization token.
            let catalog = load_catalog(global_db_path)?;
            if expect_revision.is_some_and(|revision| revision != catalog.revision) {
                return Err("Connection catalog changed; prepare the connection again".into());
            }
            let cwd = resolve_cwd(cwd)?;
            let context = context_for_cli(&cwd);
            let connection = select_connection(&catalog, &name, &context)?;
            connection.check_execution(&context)?;
            if command.first().is_none_or(|program| program.is_empty())
                || command.iter().any(|arg| arg.contains('\0'))
            {
                return Err("Command must contain a program and no NUL bytes".into());
            }
            match &connection.target {
                ConnectionTarget::Api { env, .. } => {
                    let config = read_vault_config_for_key(global_db_path)?;
                    let key = read_verified_vault_key(
                        &config,
                        stdin_password,
                        keychain,
                        password_file.as_deref(),
                        insecure_password_file,
                    )?;
                    let mut store = open_cli_store(global_db_path)?;
                    let exports = resolve_api_environment(&mut store, key.bytes(), env)?;
                    run(&cwd, &command, exports.as_slice())
                }
                ConnectionTarget::Ssh { alias, remote_cwd } => {
                    let remote_command = ssh_remote_command(&command, remote_cwd.as_deref())?;
                    // OpenSSH owns Host, IdentityFile, ProxyJump and host-key
                    // policy. Do not inspect config (ssh -G can run Match exec)
                    // or add forwarding options here.
                    let command = vec![
                        "ssh".to_string(),
                        "--".to_string(),
                        alias.clone(),
                        remote_command,
                    ];
                    run(&cwd, &command, &[])
                }
            }
        }
    }
}

fn resolve_cwd(cwd: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = cwd.map_or_else(std::env::current_dir, Ok)?.canonicalize()?;
    if !cwd.is_dir() {
        return Err("Connection working directory must be a directory".into());
    }
    Ok(cwd)
}

fn print_json(value: &impl serde::Serialize) -> CliResult {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn resolve_api_environment(
    store: &mut memcore::MemoryStore,
    key: &[u8; 32],
    bindings: &BTreeMap<String, String>,
) -> Result<crate::vault_ops::VaultEnvironment, Box<dyn std::error::Error>> {
    let bindings = bindings
        .iter()
        .map(|(env_name, reference)| {
            crate::provider_config::parse_vault_alias(reference)
                .map(|secret| (env_name.clone(), secret.to_string()))
                .ok_or("API bindings must contain only vault:NAME references")
        })
        .collect::<Result<Vec<_>, _>>()?;
    crate::vault_ops::resolve_explicit_environment(store, key, bindings).map_err(Into::into)
}

#[cfg(test)]
mod tests;
