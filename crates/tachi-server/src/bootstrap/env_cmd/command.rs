use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::EnvAction;

use super::super::open_cli_store_read_only;
use super::bindings::{build_project_env_plan, print_project_env_plan};
use super::edit::{run_env_seal, run_env_set, EnvSealArgs, EnvSetArgs};
use super::materialize::{resolve_project_env_values, run_with_project_env, unlock_cli_vault};
use super::shell::print_shell_exports;

pub(in crate::bootstrap) async fn run_env_command(
    global_db_path: &PathBuf,
    action: EnvAction,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        EnvAction::Plan { cwd, json } => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let store = open_cli_store_read_only(global_db_path)?;
            print_project_env_plan(&build_project_env_plan(&store, &cwd)?, json)
        }
        EnvAction::Export { cwd, json } => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let mut unlocked = unlock_cli_vault(
                global_db_path,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            )?;
            let exports = resolve_project_env_values(&mut unlocked, &cwd)?;
            if json {
                println!("{}", serde_json::to_string_pretty(exports.as_slice())?);
            } else {
                print_shell_exports(exports.as_slice());
                eprintln!(
                    "# tachi env export: {} project secret(s) emitted",
                    exports.as_slice().len()
                );
            }
            Ok(())
        }
        EnvAction::Run { cwd, command } => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let mut unlocked = unlock_cli_vault(
                global_db_path,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            )?;
            let exports = resolve_project_env_values(&mut unlocked, &cwd)?;
            run_with_project_env(&cwd, &command, exports.as_slice())
        }
        EnvAction::Set {
            key,
            secret,
            file,
            secret_type,
            description,
            value_stdin,
            replace,
            cwd,
        } => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            run_env_set(EnvSetArgs {
                global_db_path,
                cwd: &cwd,
                key: &key,
                secret: secret.as_deref(),
                file: file.as_deref(),
                secret_type: secret_type.as_deref(),
                description: description.as_deref(),
                value_stdin,
                replace,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            })
        }
        EnvAction::Seal {
            file,
            keys,
            dry_run,
            replace,
            cwd,
        } => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            run_env_seal(EnvSealArgs {
                global_db_path,
                cwd: &cwd,
                file: file.as_deref(),
                keys: &keys,
                dry_run,
                replace,
                stdin_password,
                keychain,
                password_file,
                insecure_password_file,
            })
        }
    }
}

fn resolve_cwd(cwd: Option<&Path>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = cwd
        .map_or_else(std::env::current_dir, |path| Ok(path.to_path_buf()))?
        .canonicalize()?;
    if !cwd.is_dir() {
        return Err("Project environment cwd must be a directory".into());
    }
    Ok(cwd)
}
