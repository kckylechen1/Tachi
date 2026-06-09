use super::*;
use std::io::Read;
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
                    &crate::credential_profile::CredentialApplyOptions {
                        allow_existing,
                        run_dir: None,
                    },
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

        VaultAction::Cleanup {
            run_dir,
            profile,
            consumer,
            dry_run: _,
            apply,
            mark_only,
        } => {
            let store = if apply {
                open_cli_store(global_db_path)?
            } else {
                open_cli_store_read_only(global_db_path)?
            };
            let report = crate::credential_profile::cleanup_managed_credential_materializations(
                &store,
                &crate::credential_profile::CredentialCleanupOptions {
                    run_dir,
                    profile,
                    consumer,
                    dry_run: !apply,
                    mark_only,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }

        VaultAction::Doctor {
            profile,
            consumer,
            config,
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
            let store = open_cli_store_read_only(global_db_path)?;
            let mut body = serde_json::json!(crate::credential_profile::doctor_credential_profile(
                &profile,
                &profile_def,
                &consumer,
                &store,
            )?);
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

        VaultAction::Init {
            stdin_password,
            keychain,
            password_file,
        } => {
            if vault_config_exists_cli(global_db_path)? {
                println!("Vault already initialized.");
                return Ok(());
            }

            let password = if stdin_password || keychain || password_file.is_some() {
                read_vault_password(stdin_password, keychain, password_file.as_deref())?
            } else {
                let password = rpassword::prompt_password("New vault password: ")?;
                if password.is_empty() {
                    return Err("Password cannot be empty".into());
                }
                let confirm = rpassword::prompt_password("Confirm password: ")?;
                if password != confirm {
                    return Err("Passwords do not match".into());
                }
                password
            };

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

        VaultAction::SetPool {
            prefix,
            strategy,
            description,
            stdin_password,
            keychain,
            password_file,
            values_stdin,
        } => {
            if !values_stdin {
                return Err("Use --values-stdin and provide one API key per line.".into());
            }
            crate::vault_crypto::validate_secret_name(&prefix)?;
            if !is_shell_env_name(&prefix) {
                return Err(format!(
                    "API key pool prefix '{prefix}' must be a shell env name such as OPENAI_API_KEY"
                )
                .into());
            }

            let mut raw_values = String::new();
            std::io::stdin().read_to_string(&mut raw_values)?;
            let values = raw_values
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if values.is_empty() {
                return Err("No API key values received on stdin.".into());
            }

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

            let now = chrono::Utc::now().to_rfc3339();
            let store = open_cli_store(global_db_path)?;
            let existing_entries = store
                .vault_list_entries_by_type("api_key")
                .map_err(|e| format!("vault_list_entries_by_type: {e}"))?;
            let mut removed_members = Vec::new();
            for (idx, value) in values.iter().enumerate() {
                let name = format!("{}_{}", prefix, idx + 1);
                let is_new = !store
                    .vault_entry_exists(&name)
                    .map_err(|e| format!("vault_entry_exists: {e}"))?;
                let (encrypted_value, nonce) =
                    crate::vault_crypto::encrypt(&key, value.as_bytes())?;
                store
                    .vault_upsert_entry(&memory_core::vault::VaultEntry {
                        name,
                        encrypted_value,
                        nonce,
                        secret_type: "api_key".to_string(),
                        description: description.clone().unwrap_or_default(),
                        allowed_agents: None,
                        created_at: if is_new { now.clone() } else { String::new() },
                        updated_at: now.clone(),
                        accessed_at: String::new(),
                        access_count: 0,
                    })
                    .map_err(|e| format!("vault_upsert_entry: {e}"))?;
            }
            for entry in existing_entries {
                if api_key_pool_member_index_cli(&entry.name, &prefix)
                    .is_some_and(|idx| idx > values.len())
                {
                    if store
                        .vault_delete_entry(&entry.name)
                        .map_err(|e| format!("vault_delete_entry: {e}"))?
                    {
                        removed_members.push(entry.name);
                    }
                }
            }

            let strategy = normalize_rotation_strategy_cli(&strategy);
            store
                .vault_set_rotation(&memory_core::vault::VaultKeyRotation {
                    prefix: prefix.clone(),
                    current_index: 1,
                    total_keys: values.len() as i64,
                    rotation_strategy: strategy.clone(),
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|e| format!("vault_set_rotation: {e}"))?;

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "stored": true,
                    "logical_name": prefix,
                    "total_keys": values.len(),
                    "strategy": strategy,
                    "removed_members": removed_members,
                }))?
            );
            Ok(())
        }

        VaultAction::Lease {
            name,
            env_name,
            stdin_password,
            keychain,
            password_file,
            json,
        } => {
            if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
                let mut args = serde_json::Map::new();
                args.insert("name".to_string(), serde_json::json!(name));
                if let Some(env_name) = env_name {
                    args.insert("env_name".to_string(), serde_json::json!(env_name));
                }
                let out =
                    crate::cli_client::call_daemon_tool(&info, "vault_lease_api_key", args).await?;
                print_lease_output(&out, json)?;
                return Ok(());
            }

            let env_name = env_name.unwrap_or_else(|| name.clone());
            if !is_shell_env_name(&env_name) {
                return Err(format!("env name '{env_name}' is not a valid shell env name").into());
            }
            let store = open_cli_store(global_db_path)?;
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

            let (key_id, value) = lease_api_key_from_store(&store, &key, &name)?;
            let body = serde_json::json!({
                "leased": true,
                "logical_name": name,
                "key_id": key_id,
                "env_name": env_name,
                "env": { env_name: value },
            });
            let out = serde_json::to_string(&body)?;
            print_lease_output(&out, json)?;
            Ok(())
        }

        VaultAction::RecordKeyResult {
            logical_name,
            key_id,
            status_code,
            outcome,
            retry_after_secs,
            reason,
            json,
        } => {
            let store = open_cli_store(global_db_path)?;
            store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            let health = build_key_health_result(
                &store,
                &logical_name,
                &key_id,
                status_code,
                outcome.as_deref(),
                retry_after_secs,
                reason.as_deref(),
            )?;
            store
                .vault_upsert_key_health(&health)
                .map_err(|e| format!("vault_upsert_key_health: {e}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&health)?);
            } else {
                println!(
                    "Recorded key health: {}:{} status={} auth_failed={} cooldown={}",
                    health.logical_name,
                    health.key_id,
                    health.status,
                    health.auth_failed,
                    health.cooldown_until.as_deref().unwrap_or("-")
                );
            }
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

fn vault_config_exists_cli(global_db_path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    if !global_db_path.exists() {
        return Ok(false);
    }
    let store = open_cli_store_read_only(&global_db_path.to_path_buf())?;
    Ok(store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .is_some())
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

fn is_shell_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn api_key_pool_member_index_cli(name: &str, prefix: &str) -> Option<usize> {
    name.strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('_'))
        .and_then(|suffix| suffix.parse::<usize>().ok())
        .filter(|idx| *idx > 0)
}

fn normalize_rotation_strategy_cli(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" => "round_robin".to_string(),
        "random" => "random".to_string(),
        "least_recently_used" | "least-recently-used" | "lru" => "least_recently_used".to_string(),
        _ => "round_robin".to_string(),
    }
}

fn key_health_blocks_cli(health: &memory_core::vault::VaultKeyHealth) -> bool {
    if health.disabled || health.auth_failed {
        return true;
    }
    match health.status.as_str() {
        "exhausted" => true,
        "rate_limited" | "cooldown" => health
            .cooldown_until
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|until| until.with_timezone(&chrono::Utc) > chrono::Utc::now()),
        _ => false,
    }
}

fn rotation_member_name(prefix: &str, idx: i64) -> String {
    format!("{prefix}_{idx}")
}

pub(super) fn lease_api_key_from_store(
    store: &memory_core::MemoryStore,
    key: &[u8; 32],
    logical_name: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let rotation = store
        .vault_get_rotation(logical_name)
        .map_err(|e| format!("vault_get_rotation: {e}"))?;

    let candidate_names = if let Some(rotation) = rotation.as_ref() {
        let total = rotation.total_keys.max(0);
        if total == 0 {
            Vec::new()
        } else {
            let start = if rotation.current_index <= 0 {
                1
            } else {
                rotation.current_index
            };
            (0..total)
                .map(|offset| {
                    let idx = ((start - 1 + offset) % total) + 1;
                    rotation_member_name(logical_name, idx)
                })
                .collect::<Vec<_>>()
        }
    } else {
        vec![logical_name.to_string()]
    };

    for candidate in candidate_names {
        let Some(entry) = entries.iter().find(|entry| entry.name == candidate) else {
            continue;
        };
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }
        if let Some(health) = store
            .vault_get_key_health(logical_name, &candidate)
            .map_err(|e| format!("vault_get_key_health: {e}"))?
        {
            if key_health_blocks_cli(&health) {
                continue;
            }
        }

        let decrypted = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
        if value.trim().is_empty() {
            continue;
        }

        if let Some(rotation) = rotation.as_ref() {
            if let Some((prefix, idx)) =
                crate::provider_config::parse_rotation_member_name(&entry.name)
            {
                if prefix == logical_name && rotation.total_keys > 0 {
                    store
                        .vault_set_rotation(&memory_core::vault::VaultKeyRotation {
                            current_index: (idx as i64 % rotation.total_keys) + 1,
                            updated_at: chrono::Utc::now().to_rfc3339(),
                            ..rotation.clone()
                        })
                        .map_err(|e| format!("vault_set_rotation: {e}"))?;
                }
            }
        }
        let _ = store.vault_touch_entry(&entry.name);
        return Ok((entry.name.clone(), value));
    }

    Err(format!(
        "No usable API key available for '{logical_name}' (missing, restricted, disabled, auth-failed, exhausted, or rate-limited)."
    )
    .into())
}

fn print_lease_output(out: &str, json_output: bool) -> Result<(), Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(out)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let env_name = value
        .get("env_name")
        .and_then(|value| value.as_str())
        .ok_or("lease response missing env_name")?;
    let secret = value
        .get("env")
        .and_then(|env| env.get(env_name))
        .and_then(|value| value.as_str())
        .ok_or("lease response missing env value")?;
    let escaped = secret.replace('\'', "'\\''");
    println!("export {env_name}='{escaped}'");
    eprintln!(
        "# tachi vault lease: {} -> {}",
        value
            .get("logical_name")
            .and_then(|value| value.as_str())
            .unwrap_or(env_name),
        value
            .get("key_id")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
    );
    Ok(())
}

fn build_key_health_result(
    store: &memory_core::MemoryStore,
    logical_name: &str,
    key_id: &str,
    status_code: Option<u16>,
    outcome: Option<&str>,
    retry_after_secs: Option<u64>,
    reason: Option<&str>,
) -> Result<memory_core::vault::VaultKeyHealth, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let mut health = store
        .vault_get_key_health(logical_name, key_id)
        .map_err(|e| format!("vault_get_key_health: {e}"))?
        .unwrap_or_else(|| memory_core::vault::VaultKeyHealth {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            ..memory_core::vault::VaultKeyHealth::default()
        });
    let outcome = outcome.map(|value| value.to_ascii_lowercase());
    health.last_attempt = Some(now.to_rfc3339());
    health.updated_at = now.to_rfc3339();
    if status_code == Some(429) || matches!(outcome.as_deref(), Some("rate_limited" | "cooldown")) {
        let cooldown = retry_after_secs.unwrap_or(60).clamp(1, 3600);
        health.status = "rate_limited".to_string();
        health.cooldown_until =
            Some((now + chrono::Duration::seconds(cooldown as i64)).to_rfc3339());
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| Some(format!("rate limited; retry after {cooldown}s")));
        health.error_count += 1;
    } else if matches!(status_code, Some(401 | 403))
        || matches!(outcome.as_deref(), Some("auth_failed"))
    {
        health.status = "auth_failed".to_string();
        health.auth_failed = true;
        health.cooldown_until = None;
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| Some("auth failure".to_string()));
        health.error_count += 1;
    } else if matches!(outcome.as_deref(), Some("exhausted")) {
        health.status = "exhausted".to_string();
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| Some("key exhausted".to_string()));
        health.error_count += 1;
    } else if status_code.is_some_and(|code| (200..300).contains(&code))
        || matches!(outcome.as_deref(), Some("success" | "ok"))
    {
        health.status = "ok".to_string();
        health.auth_failed = false;
        health.cooldown_until = None;
        health.last_success = Some(now.to_rfc3339());
        health.last_error = None;
        health.error_count = 0;
    } else {
        health.status = "error".to_string();
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| status_code.map(|code| format!("provider returned HTTP {code}")));
        health.error_count += 1;
    }
    Ok(health)
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
