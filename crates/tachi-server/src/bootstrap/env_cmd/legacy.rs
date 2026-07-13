use std::path::PathBuf;

use super::super::{open_cli_store_read_only, vault_cli};
use super::materialize::decrypt_entry_value;
use super::shell::{is_upper_snake_env_name, shell_export_line};

pub(super) async fn run_legacy_env_export(
    global_db_path: &PathBuf,
    filter: Option<&str>,
    env_only: bool,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
    insecure_password_file: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;

    // 1. Check vault is initialized
    let config = store
        .vault_get_config()
        .map_err(|e| format!("Failed to read vault config: {e}"))?
        .ok_or_else(|| {
            "Vault not initialized. Run `tachi serve` and call vault_init first, \
             or set up the vault via an MCP client."
                .to_string()
        })?;

    // 2. Resolve password from the requested portable source.
    let password = vault_cli::read_vault_password(
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;

    // 3. Derive key and verify
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    // tachi#1080: derive using the STORED `vault_config.kdf_params`, not a
    // compile-time constant. A malformed or unsupported stored value fails
    // loud and versioned here — before `verify_password` — so it is never
    // misread as "Wrong password".
    let key_result = match crate::vault_crypto::parse_stored_kdf_params(&config.kdf_params) {
        Ok(params) => crate::vault_crypto::DerivedVaultKey::derive_with_params(&password, &salt, &params)
            .map_err(|e| e.to_string()),
        Err(err) => Err(err.to_string()),
    };
    let key = key_result?;

    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Err("Wrong password".into());
    }

    // 4. List and decrypt all entries
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;

    // Build glob pattern if provided
    let glob_pattern = filter.map(glob::Pattern::new).transpose()?;

    let mut emitted = 0usize;
    for entry in entries {
        // Skip agent-restricted secrets — those aren't meant for env injection
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }

        if !crate::utils::is_shell_env_name(&entry.name) {
            eprintln!(
                "WARNING: skipped secret '{}' because it is not a valid shell environment name",
                entry.name
            );
            continue;
        }

        // --env-only: skip names that don't look like env vars (UPPER_SNAKE_CASE)
        if env_only && !is_upper_snake_env_name(&entry.name) {
            continue;
        }

        // --filter: apply glob pattern
        if let Some(ref pat) = glob_pattern {
            if !pat.matches(&entry.name) {
                continue;
            }
        }

        let value = match decrypt_entry_value(&entry, key.bytes()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARNING: failed to decrypt '{}': {}", entry.name, e);
                continue;
            }
        };

        print!("{}", shell_export_line(&entry.name, &value));
        println!();
        emitted += 1;
    }

    eprintln!("# tachi env: {} secret(s) emitted", emitted);
    Ok(())
}
