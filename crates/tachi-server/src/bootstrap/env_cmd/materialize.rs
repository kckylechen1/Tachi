use std::path::{Path, PathBuf};

use memcore::vault::VaultEntry;

use super::super::{open_cli_store, vault_cli};
use super::bindings::{
    default_project_env_output_path, load_project_env_bindings, project_root_from_bindings_path,
};
use super::shell::{is_upper_snake_env_name, shell_export_line, upsert_env_secret};
use super::types::{ProjectEnvSyncReport, UnlockedVaultStore};

pub(super) fn unlock_cli_vault(
    global_db_path: &PathBuf,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
    insecure_password_file: bool,
) -> Result<UnlockedVaultStore, Box<dyn std::error::Error>> {
    let store = open_cli_store(global_db_path)?;
    let config = store
        .vault_get_config()
        .map_err(|e| format!("Failed to read vault config: {e}"))?
        .ok_or_else(|| {
            "Vault not initialized. Run `tachi vault init` first, or initialize it via MCP."
                .to_string()
        })?;

    let mut password = vault_cli::read_vault_password(
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;
    let password = crate::vault_crypto::ZeroizingStringRef::new(&mut password);
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    // tachi#1080: derive using the STORED `vault_config.kdf_params`, not a
    // compile-time constant. A malformed or unsupported stored value fails
    // loud and versioned here — before `verify_password` — so it is never
    // misread as "Wrong password".
    let key_result = match crate::vault_crypto::parse_stored_kdf_params(&config.kdf_params) {
        Ok(params) => {
            crate::vault_crypto::DerivedVaultKey::derive_with_params(&password, &salt, &params)
                .map_err(|e| e.to_string())
        }
        Err(err) => Err(err.to_string()),
    };
    let key = key_result?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Err("Wrong password".into());
    }

    Ok(UnlockedVaultStore { store, key })
}

pub(super) fn resolve_project_env_values(
    unlocked: &mut UnlockedVaultStore,
    cwd: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let (_, bindings, ignored_lines) = load_project_env_bindings(cwd)?;
    if !ignored_lines.is_empty() {
        for ignored in ignored_lines {
            eprintln!(
                "WARNING: ignored .tachi/vault.env line {}: {}",
                ignored.line, ignored.reason
            );
        }
    }
    let mut exports = Vec::new();
    for binding in bindings {
        let value = crate::vault_ops::read_vault_secret_from_store(
            &mut unlocked.store,
            unlocked.key.bytes(),
            &binding.secret_name,
            true,
        )
        .map_err(|e| format!("{} (line {})", e, binding.line))?;
        upsert_env_secret(&mut exports, binding.env_name, value);
    }
    Ok(exports)
}

pub(super) fn filter_project_exports(
    exports: Vec<(String, String)>,
    filter: Option<&str>,
    env_only: bool,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let glob_pattern = filter.map(glob::Pattern::new).transpose()?;
    Ok(exports
        .into_iter()
        .filter(|(name, _)| {
            if env_only && !is_upper_snake_env_name(name) {
                return false;
            }
            if let Some(pattern) = glob_pattern.as_ref() {
                return pattern.matches(name);
            }
            true
        })
        .collect())
}

#[cfg(test)]
pub(super) fn follow_lane_slot_plain(
    name: &str,
    plain: String,
    entries: &[VaultEntry],
    store: &memcore::MemoryStore,
    key: &[u8; 32],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !crate::vault_ops::is_lane_slot_secret_name(name) {
        return Ok(Some(plain));
    }
    let Some(target) = crate::provider_config::parse_vault_alias(&plain) else {
        return Ok(None);
    };
    if crate::vault_ops::is_lane_slot_secret_name(target) {
        return Ok(None);
    }
    let Some(target_entry) = entries.iter().find(|entry| entry.name == target) else {
        return Ok(None);
    };
    let target_plain = decrypt_entry_value(target_entry, key)?;
    let slot_health = store
        .vault_get_key_health(name, &target_entry.name)
        .map_err(|e| format!("vault_get_key_health: {e}"))?;
    let target_health = store
        .vault_get_key_health(&target_entry.name, &target_entry.name)
        .map_err(|e| format!("vault_get_key_health: {e}"))?;
    if crate::vault_ops::account_bind::refuse_unusable_account_target(
        name,
        target_entry,
        &target_plain,
        None,
        crate::vault_ops::account_bind::slot_target_health_unusable(
            slot_health.as_ref(),
            target_health.as_ref(),
        ),
    )
    .is_err()
    {
        return Ok(None);
    }
    Ok(Some(target_plain))
}

#[cfg(test)]
pub(super) fn decrypt_entry_value(
    entry: &VaultEntry,
    key: &[u8; 32],
) -> Result<String, Box<dyn std::error::Error>> {
    if entry
        .allowed_agents
        .as_ref()
        .is_some_and(|agents| !agents.is_empty())
    {
        return Err(format!("Vault secret '{}' is agent-restricted", entry.name).into());
    }
    let decrypted = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
    let value = String::from_utf8(decrypted)
        .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
    if value.trim().is_empty() {
        return Err(format!("Vault secret '{}' is empty", entry.name).into());
    }
    Ok(value)
}

pub(super) fn sync_project_env(
    unlocked: &mut UnlockedVaultStore,
    cwd: &Path,
    output_path: Option<&Path>,
    dry_run: bool,
    force: bool,
    filter: Option<&str>,
    env_only: bool,
) -> Result<ProjectEnvSyncReport, Box<dyn std::error::Error>> {
    let (bindings_path, _, _) = load_project_env_bindings(cwd)?;
    let resolved_output_path = output_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_project_env_output_path(&bindings_path));
    let exports =
        filter_project_exports(resolve_project_env_values(unlocked, cwd)?, filter, env_only)?;
    let report = ProjectEnvSyncReport {
        cwd: project_root_from_bindings_path(&bindings_path)
            .to_string_lossy()
            .to_string(),
        bindings_path: bindings_path.to_string_lossy().to_string(),
        output_path: resolved_output_path.to_string_lossy().to_string(),
        binding_count: exports.len(),
        written: !dry_run,
        dry_run,
    };
    if dry_run {
        return Ok(report);
    }
    if resolved_output_path.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to overwrite",
            resolved_output_path.display()
        )
        .into());
    }
    if let Some(parent) = resolved_output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    content
        .push_str("# Generated by Tachi from .tachi/vault.env. DO NOT COMMIT plaintext secrets.\n");
    content.push_str("# Source bindings contain vault:<secret> aliases; this file contains materialized values.\n");
    for (name, value) in &exports {
        content.push_str(&shell_export_line(name, value));
        content.push('\n');
    }
    write_secret_file(&resolved_output_path, content.as_bytes())?;
    Ok(report)
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    crate::utils::write_owner_only_file_atomic(path, bytes)
        .map_err(|err| Box::<dyn std::error::Error>::from(std::io::Error::other(err)))
}

pub(super) fn run_with_project_env(
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
        Err(format!("Command exited with status {status}").into())
    }
}
