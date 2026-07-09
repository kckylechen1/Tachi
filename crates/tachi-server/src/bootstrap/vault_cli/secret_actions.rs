use super::daemon::detect_matching_daemon;
use super::keys::read_verified_vault_key;
use super::output::{
    build_key_health_result, lease_api_key_from_store, normalize_rotation_strategy_cli,
    print_lease_output, vault_get_output,
};
use crate::bootstrap::{open_cli_store, open_cli_store_read_only};
use std::io::Read;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::VaultAction;

pub(super) async fn run_secret_action(
    global_db_path: &PathBuf,
    app_home: &Path,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
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
            let entry = memcore::vault::VaultEntry {
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
                    entries.push(memcore::vault::VaultEntry {
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
            let rotation = memcore::vault::VaultKeyRotation {
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
                    crate::cli_client::call_daemon_tool(&info, "vault_lease_api_key", args, None)
                        .await?;
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
        _ => unreachable!("secret action router received non-secret action"),
    }
}
