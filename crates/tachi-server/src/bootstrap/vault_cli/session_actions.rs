use super::daemon::{call_daemon_vault_unlock, detect_matching_daemon};
use super::keys::vault_config_exists_cli;
use super::output::print_vault_list_output;
use super::password::{read_vault_init_password, read_vault_password};
use crate::bootstrap::{open_cli_store, open_cli_store_read_only};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::VaultAction;

pub(super) async fn run_session_action(
    global_db_path: &PathBuf,
    app_home: &Path,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        VaultAction::Status => {
            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
                let out = crate::cli_client::call_daemon_tool(
                    &info,
                    "vault_status",
                    serde_json::Map::new(),
                    None,
                )
                .await?;
                println!("{out}");
                return Ok(());
            }

            let store = open_cli_store_read_only(global_db_path)?;
            let initialized = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .is_some();
            let entry_count = if initialized {
                store.vault_list_entries().map(|e| e.len()).unwrap_or(0)
            } else {
                0
            };
            println!("Vault status:");
            println!("  initialized: {initialized}");
            println!("  entries: {entry_count}");
            Ok(())
        }
        VaultAction::Init {
            stdin_password,
            keychain,
            password_file,
            confirm_password_file,
            insecure_password_file,
        } => {
            if vault_config_exists_cli(global_db_path)? {
                println!("Vault already initialized.");
                return Ok(());
            }

            let mut password = read_vault_init_password(
                stdin_password,
                keychain,
                password_file.as_deref(),
                confirm_password_file.as_deref(),
                insecure_password_file,
            )?;

            let salt = crate::vault_crypto::generate_salt();
            let key_result = crate::vault_crypto::DerivedVaultKey::derive(&password, &salt);
            crate::vault_crypto::zero_string(&mut password);
            let key = key_result?;
            let verifier = crate::vault_crypto::create_verifier(key.bytes())?;
            let salt_b64 = B64.encode(salt);
            let now = chrono::Utc::now().to_rfc3339();

            let store = open_cli_store(global_db_path)?;
            store
                .vault_set_config(&memcore::vault::VaultConfig {
                    salt: salt_b64,
                    verifier,
                    kdf_algorithm: "argon2id".to_string(),
                    kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
                    cipher: memcore::vault::VaultCipher::Aes256Gcm,
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|e| format!("vault_set_config: {e}"))?;

            println!("Vault initialized successfully.");
            Ok(())
        }
        VaultAction::Lock => {
            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
                let out = crate::cli_client::call_daemon_tool(
                    &info,
                    "vault_lock",
                    serde_json::Map::new(),
                    None,
                )
                .await?;
                println!("{out}");
                return Ok(());
            }

            println!("Vault lock is a runtime operation (affects the running daemon).");
            println!(
                "No running daemon was detected; stateless CLI commands do not keep the vault unlocked."
            );
            Ok(())
        }
        VaultAction::Unlock {
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let mut password = read_vault_password(
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
                let out = call_daemon_vault_unlock(app_home, &info, password).await?;
                println!("{out}");
                return Ok(());
            }

            let store = open_cli_store_read_only(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let salt = B64
                .decode(&config.salt)
                .map_err(|e| format!("Invalid vault salt: {e}"))?;
            // tachi#1080: derive using the STORED `vault_config.kdf_params`,
            // not a compile-time constant. A malformed or unsupported stored
            // value fails loud and versioned here — before `verify_password` —
            // so it is never misread as "Wrong password". The password is
            // zeroed on BOTH the error and success paths (pre-existing discipline).
            let key_result = match crate::vault_crypto::parse_stored_kdf_params(&config.kdf_params) {
                Ok(params) => crate::vault_crypto::DerivedVaultKey::derive_with_params(&password, &salt, &params)
                    .map_err(|e| e.to_string()),
                Err(err) => Err(err.to_string()),
            };
            let key = match key_result {
                Ok(key) => key,
                Err(err) => {
                    crate::vault_crypto::zero_string(&mut password);
                    return Err(err.into());
                }
            };
            crate::vault_crypto::zero_string(&mut password);

            if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
                return Err("Wrong password".into());
            }
            println!("Vault password verified. No running daemon was detected; this CLI verification is stateless.");
            Ok(())
        }
        VaultAction::List {
            stdin_password: _,
            keychain: _,
            password_file: _,
            insecure_password_file: _,
        } => {
            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
                if let Ok(out) = crate::cli_client::call_daemon_tool(
                    &info,
                    "vault_list",
                    serde_json::Map::new(),
                    None,
                )
                .await
                {
                    print_vault_list_output(&out)?;
                    return Ok(());
                }
            }

            let store = open_cli_store_read_only(global_db_path)?;
            store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let entries = store
                .vault_list_entries()
                .map_err(|e| format!("vault_list_entries: {e}"))?;

            if entries.is_empty() {
                println!("(no secrets stored)");
            } else {
                println!("{:<30} {:<12} DESCRIPTION", "NAME", "TYPE");
                for entry in &entries {
                    println!(
                        "{:<30} {:<12} {}",
                        entry.name, entry.secret_type, entry.description
                    );
                }
                println!("\n{} secret(s) total.", entries.len());
            }
            Ok(())
        }
        _ => unreachable!("session action router received non-session action"),
    }
}
