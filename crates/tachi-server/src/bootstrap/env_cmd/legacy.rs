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
    let mut password = vault_cli::read_vault_password(
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;
    let password = crate::vault_crypto::ZeroizingStringRef::new(&mut password);

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

    // 4. List and decrypt all entries
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;
    for rotation in store
        .vault_list_rotations()
        .map_err(|e| format!("Failed to list vault rotations: {e}"))?
    {
        memcore::validate_api_key_rotation(&entries, &rotation)
            .map_err(|error| format!("{error}; refusing legacy env export"))?;
    }

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

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::bootstrap::open_cli_store;

    #[tokio::test]
    async fn legacy_env_export_rejects_a_poisoned_configured_rotation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("memory.db");
        let password_file = temp.path().join("vault-password");
        std::fs::write(&password_file, b"legacy-env-password\n").expect("password file");
        std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
            .expect("password file permissions");
        let key = crate::bootstrap::vault_cli::vault_init_with_password(
            &db_path,
            "legacy-env-password".to_string(),
        )
        .expect("initialize fixture vault");
        let store = open_cli_store(&db_path).expect("open fixture store");
        let now = "2026-01-01T00:00:00Z".to_string();
        for (idx, secret_type) in [(1, "api_key"), (2, "api_key"), (3, "config")] {
            let (encrypted_value, nonce) =
                crate::vault_crypto::encrypt(key.bytes(), format!("key-{idx}").as_bytes())
                    .expect("encrypt member");
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: format!("LEGACY_POOL_API_KEY_{idx}"),
                    encrypted_value,
                    nonce,
                    secret_type: secret_type.to_string(),
                    description: "rotation member".to_string(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .expect("seed member");
        }
        store
            .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                prefix: "LEGACY_POOL_API_KEY".to_string(),
                current_index: 1,
                total_keys: 2,
                rotation_strategy: "round_robin".to_string(),
                created_at: now.clone(),
                updated_at: now,
            })
            .expect("seed rotation");
        drop(store);

        let error = run_legacy_env_export(
            &db_path,
            None,
            false,
            false,
            false,
            Some(&password_file),
            false,
        )
        .await
        .expect_err("legacy env export must reject a poisoned configured rotation")
        .to_string();
        assert!(
            error.contains("LEGACY_POOL_API_KEY_3")
                && error.contains("config")
                && error.contains("refusing legacy env export"),
            "{error}"
        );
    }
}
