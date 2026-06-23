use super::{open_cli_store, open_cli_store_read_only, vault_sync};
use std::io::Read;
use std::path::{Path, PathBuf};

mod daemon;
mod keys;
mod output;
mod password;

use daemon::{call_daemon_vault_unlock, detect_matching_daemon};
use keys::{
    decrypt_profile_secret_values, read_vault_config_for_key, read_verified_vault_key,
    run_vault_setup_keys, vault_config_exists_cli,
};
use output::{
    build_key_health_result, normalize_rotation_strategy_cli, print_lease_output,
    print_vault_list_output, vault_get_output,
};

pub(super) use keys::{vault_init_with_password, vault_upsert_secret_with_key};
pub(super) use output::lease_api_key_from_store;
use password::read_vault_init_password;
pub(super) use password::read_vault_password;

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
            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
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
            insecure_password_file,
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
                let mut secret_values = decrypt_profile_secret_values(
                    global_db_path,
                    &profile_def,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
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
                );
                for value in secret_values.values_mut() {
                    crate::vault_crypto::zero_string(value);
                }
                let result = result?;
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
            let path = vault_sync::resolve_vault_sync_path(path)?;
            let status = vault_sync::vault_sync_status(&path)?;
            vault_sync::print_status(&status);
            Ok(())
        }

        VaultAction::SetupKeys {
            stdin_password,
            keychain,
            password_file,
            confirm_password_file,
            insecure_password_file,
            include_deprecated,
        } => run_vault_setup_keys(
            global_db_path,
            stdin_password,
            keychain,
            password_file.as_deref(),
            confirm_password_file.as_deref(),
            insecure_password_file,
            include_deprecated,
        ),

        VaultAction::SyncExport {
            output,
            allow_cloud,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let output = vault_sync::resolve_vault_sync_path(output)?;
            let config = read_vault_config_for_key(global_db_path)?;
            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;
            let status =
                vault_sync::export_vault_bundle(global_db_path, &output, allow_cloud, key.bytes())?;
            println!("Vault sync export complete.");
            vault_sync::print_status(&status);
            println!(
                "  contents: signed encrypted Vault config, entries, and key-rotation metadata"
            );
            Ok(())
        }

        VaultAction::SyncImport {
            input,
            allow_unsigned,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let input = vault_sync::resolve_vault_sync_path(input)?;
            let key = if allow_unsigned && !vault_sync::bundle_has_signature(&input)? {
                None
            } else {
                let config = vault_sync::read_bundle_vault_config(&input)?;
                Some(read_verified_vault_key(
                    &config,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?)
            };
            let report = vault_sync::import_vault_bundle(
                global_db_path,
                &input,
                key.as_ref().map(|key| key.bytes()),
                allow_unsigned,
            )?;
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
                .vault_set_config(&memory_core::vault::VaultConfig {
                    salt: salt_b64,
                    verifier,
                    kdf_algorithm: "argon2id".to_string(),
                    kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
                    cipher: memory_core::vault::VaultCipher::Aes256Gcm,
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
            let key = match crate::vault_crypto::DerivedVaultKey::derive(&password, &salt) {
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

        VaultAction::Set {
            name,
            secret_type,
            description,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
            value_stdin,
        } => {
            crate::vault_crypto::validate_secret_name(&name)?;

            let store_ro = open_cli_store_read_only(global_db_path)?;
            let config = store_ro
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            drop(store_ro);

            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

            let mut secret_value = if value_stdin {
                let mut buf = String::new();
                std::io::stdin().read_line(&mut buf)?;
                buf.trim().to_string()
            } else {
                rpassword::prompt_password(format!("Value for {name}: "))?
            };
            if secret_value.is_empty() {
                return Err("Secret value cannot be empty".into());
            }

            let encrypt_result = crate::vault_crypto::encrypt(key.bytes(), secret_value.as_bytes());
            crate::vault_crypto::zero_string(&mut secret_value);
            let (encrypted_value, nonce) = encrypt_result?;

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
            insecure_password_file,
            values_stdin,
        } => {
            if !values_stdin {
                return Err("Use --values-stdin and provide one API key per line.".into());
            }
            crate::vault_crypto::validate_secret_name(&prefix)?;
            if !crate::utils::is_shell_env_name(&prefix) {
                return Err(format!(
                    "API key pool prefix '{prefix}' must be a shell env name such as OPENAI_API_KEY"
                )
                .into());
            }

            let mut raw_values = String::new();
            std::io::stdin().read_to_string(&mut raw_values)?;
            let mut values = raw_values
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            crate::vault_crypto::zero_string(&mut raw_values);
            if values.is_empty() {
                return Err("No API key values received on stdin.".into());
            }

            let store_ro = open_cli_store_read_only(global_db_path)?;
            let config = store_ro
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            drop(store_ro);

            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

            let now = chrono::Utc::now().to_rfc3339();
            let mut entries = Vec::with_capacity(values.len());
            let build_entries = (|| -> Result<(), Box<dyn std::error::Error>> {
                for (idx, value) in values.iter().enumerate() {
                    let name = format!("{}_{}", prefix, idx + 1);
                    let (encrypted_value, nonce) =
                        crate::vault_crypto::encrypt(key.bytes(), value.as_bytes())?;
                    entries.push(memory_core::vault::VaultEntry {
                        name,
                        encrypted_value,
                        nonce,
                        secret_type: "api_key".to_string(),
                        description: description.clone().unwrap_or_default(),
                        allowed_agents: None,
                        created_at: now.clone(),
                        updated_at: now.clone(),
                        accessed_at: String::new(),
                        access_count: 0,
                    });
                }
                Ok(())
            })();
            for value in &mut values {
                crate::vault_crypto::zero_string(value);
            }
            build_entries?;

            let strategy = normalize_rotation_strategy_cli(&strategy);
            let rotation = memory_core::vault::VaultKeyRotation {
                prefix: prefix.clone(),
                current_index: 1,
                total_keys: entries.len() as i64,
                rotation_strategy: strategy.clone(),
                created_at: now.clone(),
                updated_at: now,
            };
            let mut store = open_cli_store(global_db_path)?;
            let removed_members = store
                .vault_replace_api_key_pool(&prefix, &entries, &rotation)
                .map_err(|e| format!("vault_replace_api_key_pool: {e}"))?;

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "stored": true,
                    "logical_name": prefix,
                    "total_keys": entries.len(),
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
            insecure_password_file,
            json,
        } => {
            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
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
            if !crate::utils::is_shell_env_name(&env_name) {
                return Err(format!("env name '{env_name}' is not a valid shell env name").into());
            }
            let store = open_cli_store(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

            let (key_id, mut value) = lease_api_key_from_store(&store, key.bytes(), &name)?;
            let body = serde_json::json!({
                "leased": true,
                "logical_name": name,
                "key_id": key_id,
                "env_name": env_name,
                "env": { env_name: value.clone() },
            });
            let out = serde_json::to_string(&body)?;
            crate::vault_crypto::zero_string(&mut value);
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
            reveal,
            json,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let config = store
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;

            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

            let entry = store
                .vault_get_entry(&name)
                .map_err(|e| format!("vault_get_entry: {e}"))?
                .ok_or(format!("Secret '{name}' not found"))?;

            let decrypted =
                crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
            let mut value = String::from_utf8(decrypted)
                .map_err(|e| format!("Secret is not valid UTF-8: {e}"))?;

            let output = vault_get_output(&name, &value, reveal, json)?;
            crate::vault_crypto::zero_string(&mut value);
            print!("{output}");
            Ok(())
        }

        VaultAction::Remove {
            name,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            crate::vault_crypto::validate_secret_name(&name)?;

            let store_ro = open_cli_store_read_only(global_db_path)?;
            let config = store_ro
                .vault_get_config()
                .map_err(|e| format!("vault_get_config: {e}"))?
                .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
            drop(store_ro);

            let _key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

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
            insecure_password_file: _,
        } => {
            if let Some(info) = detect_matching_daemon(app_home, global_db_path).await {
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

#[cfg(test)]
mod tests;
