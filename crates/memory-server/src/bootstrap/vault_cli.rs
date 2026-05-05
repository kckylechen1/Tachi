use super::*;

// ─── `tachi vault` handler ──────────────────────────────────────────────────

pub(super) async fn run_vault_command(
    global_db_path: &PathBuf,
    action: crate::cli::VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cli::VaultAction;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    match action {
        VaultAction::Status => {
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

        VaultAction::Init => {
            let store = open_cli_store_read_only(global_db_path)?;
            if store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .is_some()
            {
                println!("Vault already initialized.");
                return Ok(());
            }
            drop(store);

            let password = rpassword::prompt_password("New vault password: ")?;
            if password.is_empty() {
                return Err("Password cannot be empty".into());
            }
            let confirm = rpassword::prompt_password("Confirm password: ")?;
            if password != confirm {
                return Err("Passwords do not match".into());
            }

            let salt = crate::vault_crypto::generate_salt();
            let key = crate::vault_crypto::derive_key(&password, &salt)?;
            let verifier = crate::vault_crypto::create_verifier(&key)?;
            let salt_b64 = B64.encode(salt);
            let now = chrono::Utc::now().to_rfc3339();

            let store = open_cli_store(global_db_path)?;
            store
                .vault_set_config(&memory_core::vault::VaultConfig {
                    salt: salt_b64,
                    verifier,
                    kdf_algorithm: "argon2id".to_string(),
                    kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
                    cipher: "aes-256-gcm".to_string(),
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|e| format!("vault_set_config: {e}"))?;

            println!("Vault initialized successfully.");
            Ok(())
        }

        VaultAction::Lock => {
            println!("Vault lock is a runtime operation (affects the running daemon).");
            println!(
                "The CLI operates statelessly — vault is only 'unlocked' during command execution."
            );
            Ok(())
        }

        VaultAction::Unlock {
            stdin_password,
            keychain,
        } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let password = read_vault_password(stdin_password, keychain)?;
            let salt = B64
                .decode(&config.salt)
                .map_err(|e| format!("Invalid vault salt: {e}"))?;
            let key = crate::vault_crypto::derive_key(&password, &salt)?;

            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("Wrong password".into());
            }
            println!("Vault password verified. Vault is accessible.");
            Ok(())
        }

        VaultAction::Set {
            name,
            secret_type,
            description,
            stdin_password,
            keychain,
            value_stdin,
        } => {
            crate::vault_crypto::validate_secret_name(&name)?;

            let store_ro = open_cli_store_read_only(global_db_path)?;
            let config = store_ro
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            drop(store_ro);

            let password = read_vault_password(stdin_password, keychain)?;
            let salt = B64
                .decode(&config.salt)
                .map_err(|e| format!("Invalid vault salt: {e}"))?;
            let key = crate::vault_crypto::derive_key(&password, &salt)?;

            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("Wrong password".into());
            }

            let secret_value = if value_stdin {
                let mut buf = String::new();
                std::io::stdin().read_line(&mut buf)?;
                buf.trim().to_string()
            } else {
                rpassword::prompt_password(format!("Value for {name}: "))?
            };
            if secret_value.is_empty() {
                return Err("Secret value cannot be empty".into());
            }

            let (encrypted_value, nonce) =
                crate::vault_crypto::encrypt(&key, secret_value.as_bytes())?;

            let is_new = !open_cli_store_read_only(global_db_path)?
                .vault_entry_exists(&name)
                .map_err(|e| format!("vault_entry_exists: {e}"))?;

            let now = chrono::Utc::now().to_rfc3339();
            let entry = memory_core::vault::VaultEntry {
                name: name.clone(),
                encrypted_value,
                nonce,
                secret_type: secret_type.clone(),
                description: description.unwrap_or_default(),
                allowed_agents: None,
                created_at: if is_new { now.clone() } else { String::new() },
                updated_at: now,
                accessed_at: String::new(),
                access_count: 0,
            };

            let store = open_cli_store(global_db_path)?;
            store
                .vault_upsert_entry(&entry)
                .map_err(|e| format!("vault_upsert_entry: {e}"))?;

            println!("Secret '{name}' saved (type: {secret_type}).");
            Ok(())
        }

        VaultAction::Get {
            name,
            stdin_password,
            keychain,
        } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let password = read_vault_password(stdin_password, keychain)?;
            let salt = B64
                .decode(&config.salt)
                .map_err(|e| format!("Invalid vault salt: {e}"))?;
            let key = crate::vault_crypto::derive_key(&password, &salt)?;

            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("Wrong password".into());
            }

            let entry = store
                .vault_get_entry(&name)
                .map_err(|e| format!("vault_get_entry: {e}"))?
                .ok_or(format!("Secret '{name}' not found"))?;

            let decrypted =
                crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
            let value = String::from_utf8(decrypted)
                .map_err(|e| format!("Secret is not valid UTF-8: {e}"))?;

            println!("{value}");
            Ok(())
        }

        VaultAction::List {
            stdin_password,
            keychain,
        } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let password = read_vault_password(stdin_password, keychain)?;
            let salt = B64
                .decode(&config.salt)
                .map_err(|e| format!("Invalid vault salt: {e}"))?;
            let key = crate::vault_crypto::derive_key(&password, &salt)?;

            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("Wrong password".into());
            }

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
    }
}

fn read_vault_password(
    stdin_password: bool,
    keychain: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let password = if keychain {
        let output = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "tachi-vault",
                "-a",
                "default",
                "-w",
            ])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "Failed to read from Keychain: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        String::from_utf8(output.stdout)?.trim().to_string()
    } else if stdin_password {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        buf.trim().to_string()
    } else {
        rpassword::prompt_password("Vault password: ")?
    };

    if password.is_empty() {
        return Err("Password cannot be empty".into());
    }
    Ok(password)
}
