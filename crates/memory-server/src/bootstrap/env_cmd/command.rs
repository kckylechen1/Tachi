use crate::cli::EnvAction;
use std::path::{Path, PathBuf};

use super::super::open_cli_store_read_only;
use super::bindings::{build_project_env_plan, print_project_env_plan};
use super::legacy::run_legacy_env_export;
use super::materialize::{
    filter_project_exports, resolve_project_env_values, run_with_project_env, sync_project_env,
    unlock_cli_vault,
};
use super::shell::print_shell_exports;

pub(in crate::bootstrap) async fn run_env_command(
    global_db_path: &PathBuf,
    action: Option<EnvAction>,
    filter: Option<&str>,
    env_only: bool,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
    insecure_password_file: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        Some(EnvAction::Plan { cwd, json }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let store = open_cli_store_read_only(global_db_path)?;
            let plan = build_project_env_plan(&store, &cwd, None)?;
            print_project_env_plan(&plan, json)?;
            Ok(())
        }
        Some(EnvAction::Export { cwd, json }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let unlocked = unlock_cli_vault(
                global_db_path,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            )?;
            let exports = filter_project_exports(
                resolve_project_env_values(&unlocked, &cwd)?,
                filter,
                env_only,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&exports)?);
            } else {
                print_shell_exports(&exports);
                eprintln!(
                    "# tachi env export: {} project secret(s) emitted",
                    exports.len()
                );
            }
            Ok(())
        }
        Some(EnvAction::Sync {
            cwd,
            output,
            dry_run,
            apply,
            force,
            json,
        }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let unlocked = unlock_cli_vault(
                global_db_path,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            )?;
            let preview = dry_run || !apply;
            let report = sync_project_env(
                &unlocked,
                &cwd,
                output.as_deref(),
                preview,
                force,
                filter,
                env_only,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if preview {
                println!("Tachi env sync preview (pass --apply to write).");
                println!("  cwd: {}", report.cwd);
                println!("  bindings: {}", report.bindings_path);
                println!("  output: {}", report.output_path);
                println!("  project secrets: {}", report.binding_count);
            } else {
                println!("Tachi env sync complete.");
                println!("  output: {}", report.output_path);
                println!("  project secrets: {}", report.binding_count);
            }
            Ok(())
        }
        Some(EnvAction::Run { cwd, command }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let unlocked = unlock_cli_vault(
                global_db_path,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            )?;
            let exports = filter_project_exports(
                resolve_project_env_values(&unlocked, &cwd)?,
                filter,
                env_only,
            )?;
            run_with_project_env(&cwd, &command, &exports)
        }
        None => {
            run_legacy_env_export(
                global_db_path,
                filter,
                env_only,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            )
            .await
        }
    }
}

fn resolve_cwd(cwd: Option<&Path>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(match cwd {
        Some(path) => std::fs::canonicalize(path)
            .map_err(|e| format!("Failed to resolve cwd {}: {e}", path.display()))?,
        None => std::env::current_dir()?,
    })
}
