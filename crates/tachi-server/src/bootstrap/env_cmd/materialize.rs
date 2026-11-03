use std::path::{Path, PathBuf};

use super::super::{open_cli_store, vault_cli, VaultCommandExit};
use super::types::UnlockedVaultStore;

pub(super) fn unlock_cli_vault(
    global_db_path: &PathBuf,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<UnlockedVaultStore, Box<dyn std::error::Error>> {
    let config = vault_cli::read_vault_config_for_key(global_db_path)?;
    let key = vault_cli::read_verified_vault_key(
        &config,
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;
    Ok(UnlockedVaultStore {
        store: open_cli_store(global_db_path)?,
        key,
    })
}

pub(super) fn resolve_project_env_values(
    unlocked: &mut UnlockedVaultStore,
    cwd: &Path,
) -> Result<crate::vault_ops::VaultEnvironment, Box<dyn std::error::Error>> {
    let (_, bindings, ignored_lines) = crate::vault_ops::load_project_env_bindings(cwd)?;
    for ignored in ignored_lines {
        eprintln!(
            "WARNING: ignored .tachi/vault.env line {}: {}",
            ignored.line, ignored.reason
        );
    }
    crate::vault_ops::resolve_explicit_environment(
        &mut unlocked.store,
        unlocked.key.bytes(),
        bindings
            .into_iter()
            .map(|binding| (binding.env_name, binding.secret_name)),
    )
    .map_err(Into::into)
}

pub(crate) fn run_with_project_env(
    cwd: &Path,
    command: &[String],
    exports: &[(String, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((program, args)) = command.split_first() else {
        return Err("No command provided".into());
    };
    let mut child = std::process::Command::new(program);
    child.args(args).current_dir(cwd);
    for (name, value) in exports {
        child.env(name, value);
    }
    let status = child.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Box::new(VaultCommandExit::from_status(status)))
    }
}
