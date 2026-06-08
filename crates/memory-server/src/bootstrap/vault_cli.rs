use super::*;
use std::path::Path;

// ─── `tachi vault` handler ──────────────────────────────────────────────────

pub(super) async fn run_vault_command(
    global_db_path: &PathBuf,
    app_home: &Path,
    action: crate::cli::VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cli::VaultAction;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    match action {
        VaultAction::Status => {
            if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
                let out = crate::cli_client::call_daemon_tool(
                    &info,
                    "vault_status",
                    serde_json::Map::new(),
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

        VaultAction::Materialize {
            profile,
            consumer,
            config,
            dry_run: _,
            apply,
            allow_existing,
            stdin_password,
            keychain,
            password_file,
        } => {
            let (config_path, profile_def) = if let Some(path) = config {
                let profile_def =
                    crate::credential_profile::load_credential_profile_from_path(&path, &profile)?;
                (path, profile_def)
            } else {
                crate::credential_profile::find_credential_profile(
                    &crate::credential_profile::default_credentials_dir(),
                    &profile,
                )?
            };

            let store = if apply {
                open_cli_store(global_db_path)?
            } else {
                open_cli_store_read_only(global_db_path)?
            };
            let report = crate::credential_profile::plan_credential_materialization(
                &profile,
                &profile_def,
                &consumer,
                &store,
            )?;
            let mut body = if apply {
                let secret_values = decrypt_profile_secret_values(
                    global_db_path,
                    &profile_def,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                )?;
                let result = crate::credential_profile::apply_credential_materialization(
                    &profile,
                    &profile_def,
                    &consumer,
                    &store,
                    &secret_values,
                    &crate::credential_profile::CredentialApplyOptions { allow_existing },
                )?;
                let mut value =
                    crate::credential_profile::credential_materialize_report_json(&result.report);
                value["env_outputs"] = serde_json::json!(result
                    .env
                    .keys()
                    .map(|key| format!("env:{key}"))
                    .collect::<Vec<_>>());
                value
            } else {
                crate::credential_profile::credential_materialize_report_json(&report)
            };
            body["config_path"] = serde_json::json!(config_path.to_string_lossy());
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(())
        }

        VaultAction::SyncStatus { path } => {
            let path = super::vault_sync::resolve_vault_sync_path(path)?;
            let status = super::vault_sync::vault_sync_status(&path)?;
            super::vault_sync::print_status(&status);
            Ok(())
        }

        VaultAction::SyncExport { output } => {
            let output = super::vault_sync::resolve_vault_sync_path(output)?;
            let status = super::vault_sync::export_vault_bundle(global_db_path, &output)?;
            println!("Vault sync export complete.");
            super::vault_sync::print_status(&status);
            println!("  contents: encrypted Vault config, entries, and key-rotation metadata");
            Ok(())
        }

        VaultAction::SyncImport { input } => {
            let input = super::vault_sync::resolve_vault_sync_path(input)?;
            let report = super::vault_sync::import_vault_bundle(global_db_path, &input)?;
            println!("Vault sync import complete.");
            println!("  path: {}", report.path);
            println!("  initialized_vault: {}", report.initialized_vault);
            println!("  entries_imported: {}", report.entries_imported);
            println!("  rotations_imported: {}", report.rotations_imported);
            println!("  note: import only upserts encrypted rows; it does not delete local extras");
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
            if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
                let out = crate::cli_client::call_daemon_tool(
                    &info,
                    "vault_lock",
                    serde_json::Map::new(),
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
        } => {
            let password = read_vault_password(stdin_password, keychain, password_file.as_deref())?;

            if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
                let mut args = serde_json::Map::new();
                args.insert("password".to_string(), serde_json::json!(password));
                let out = crate::cli_client::call_daemon_tool(&info, "vault_unlock", args).await?;
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
            let key = crate::vault_crypto::derive_key(&password, &salt)?;

            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("Wrong password".into());
            }
            println!("Vault password verified. No running daemon was detected; this CLI verification is stateless.");
            Ok(())
        }

        VaultAction::Set {
            name,
            secret_type,
            description,
            stdin_password,
            keychain,
            password_file,
            value_stdin,
        } => {
            crate::vault_crypto::validate_secret_name(&name)?;

            let store_ro = open_cli_store_read_only(global_db_path)?;
            let config = store_ro
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            drop(store_ro);

            let password = read_vault_password(stdin_password, keychain, password_file.as_deref())?;
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
            password_file,
        } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let password = read_vault_password(stdin_password, keychain, password_file.as_deref())?;
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

        VaultAction::Remove {
            name,
            stdin_password,
            keychain,
            password_file,
        } => {
            crate::vault_crypto::validate_secret_name(&name)?;

            let store_ro = open_cli_store_read_only(global_db_path)?;
            let config = store_ro
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            drop(store_ro);

            let password = read_vault_password(stdin_password, keychain, password_file.as_deref())?;
            let salt = B64
                .decode(&config.salt)
                .map_err(|e| format!("Invalid vault salt: {e}"))?;
            let key = crate::vault_crypto::derive_key(&password, &salt)?;

            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("Wrong password".into());
            }

            let store = open_cli_store(global_db_path)?;
            let removed = store
                .vault_delete_entry(&name)
                .map_err(|e| format!("vault_delete_entry: {e}"))?;

            if removed {
                println!("Secret '{name}' removed.");
            } else {
                println!("Secret '{name}' was not found.");
            }
            Ok(())
        }

        VaultAction::List {
            stdin_password: _,
            keychain: _,
            password_file: _,
        } => {
            if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
                if let Ok(out) =
                    crate::cli_client::call_daemon_tool(&info, "vault_list", serde_json::Map::new())
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
    }
}

fn decrypt_profile_secret_values(
    global_db_path: &PathBuf,
    profile: &crate::credential_profile::CredentialProfile,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
) -> Result<std::collections::HashMap<String, String>, Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let store = open_cli_store_read_only(global_db_path)?;
    let config = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
    let password = read_vault_password(stdin_password, keychain, password_file)?;
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;
    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Err("Wrong password".into());
    }

    let mut values = std::collections::HashMap::new();
    for name in crate::credential_profile::profile_secret_names(profile) {
        let entry = store
            .vault_get_entry(&name)
            .map_err(|e| format!("vault_get_entry: {e}"))?
            .ok_or_else(|| format!("Vault secret '{name}' is missing"))?;
        let decrypted = crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{name}' is not valid UTF-8: {e}"))?;
        values.insert(name, value);
    }
    Ok(values)
}

fn print_vault_list_output(out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(out) else {
        println!("{out}");
        return Ok(());
    };
    let Some(secrets) = value.get("secrets").and_then(|v| v.as_array()) else {
        println!("{out}");
        return Ok(());
    };

    if secrets.is_empty() {
        println!("(no secrets stored)");
        return Ok(());
    }

    println!("{:<30} {:<12} DESCRIPTION", "NAME", "TYPE");
    for entry in secrets {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let secret_type = entry
            .get("secret_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("{name:<30} {secret_type:<12} {description}");
    }
    let count = value
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(secrets.len() as u64);
    println!("\n{count} secret(s) total.");
    Ok(())
}

pub(super) fn read_vault_password(
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
) -> Result<String, Box<dyn std::error::Error>> {
    let password = if keychain {
        if !cfg!(target_os = "macos") {
            return Err(
                "--keychain is only supported on macOS; use --password-file on Linux/Windows"
                    .into(),
            );
        }
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
    } else if let Some(path) = password_file {
        read_password_file(path)?
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

fn read_password_file(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read password file {}: {e}", path.display()))?;
    let password = raw.lines().next().unwrap_or_default().trim().to_string();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "WARNING: password file {} is readable by group/other (mode {:o}); prefer 0600",
                    path.display(),
                    mode
                );
            }
        }
    }

    Ok(password)
}
