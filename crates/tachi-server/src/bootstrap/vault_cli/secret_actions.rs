use super::daemon::detect_matching_daemon;
use super::keys::read_verified_vault_key;
use super::output::{
    build_key_health_result, lease_api_key_from_store, normalize_rotation_strategy_cli,
    print_lease_output, vault_get_output,
};
use crate::bootstrap::{open_cli_store, open_cli_store_read_only};
use std::io::BufRead;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::VaultAction;

pub(super) struct ZeroizingSecretString<'a>(pub(super) &'a mut String);

impl Deref for ZeroizingSecretString<'_> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl Drop for ZeroizingSecretString<'_> {
    fn drop(&mut self) {
        crate::vault_crypto::zero_string(self.0);
    }
}

pub(super) async fn run_secret_action(
    global_db_path: &PathBuf,
    app_home: &Path,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    run_secret_action_with_reader(global_db_path, app_home, action, &mut stdin).await
}

async fn run_secret_action_with_reader(
    global_db_path: &PathBuf,
    app_home: &Path,
    action: VaultAction,
    stdin: &mut impl BufRead,
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
            rebind,
        } => {
            crate::vault_crypto::validate_secret_name(&name)?;
            let secret_type = secret_type
                .as_deref()
                .map(memcore::normalize_secret_type)
                .unwrap_or_else(|| memcore::infer_vault_secret_type(&name))
                .to_string();
            crate::vault_ops::validate_lane_slot_secret_type(&name, &secret_type)?;
            memcore::reject_api_key_type_for_lane_config(&name, &secret_type)?;

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
                let mut input = String::new();
                if let Err(error) = stdin.read_line(&mut input) {
                    crate::vault_crypto::zero_string(&mut input);
                    return Err(error.into());
                }
                let value = input.trim().to_string();
                crate::vault_crypto::zero_string(&mut input);
                value
            } else {
                rpassword::prompt_password(format!("Value for {name}: "))?
            };
            let secret_value = ZeroizingSecretString(&mut secret_value);
            if secret_value.trim().is_empty() {
                return Err("Secret value cannot be empty".into());
            }
            if memcore::is_lane_config_url_name(&name) {
                if let Some(leak) =
                    memcore::catalog::endpoint::endpoint_credential_leak(&secret_value)
                {
                    return Err(format!(
                        "Vault name '{name}' value embeds a credential in the endpoint ({leak}); refusing write"
                    ).into());
                }
            }

            let now = chrono::Utc::now().to_rfc3339();
            let mut store = open_cli_store(global_db_path)?;
            let transaction = store
                .begin_vault_transaction()
                .map_err(|e| format!("begin vault transaction: {e}"))?;
            let is_new = !transaction
                .vault_entry_exists(&name)
                .map_err(|e| format!("vault_entry_exists: {e}"))?;

            if crate::vault_ops::is_lane_slot_secret_name(&name)
                && secret_type == memcore::vault::SECRET_TYPE_API_KEY
            {
                // This scan and the slot decision are deliberately inside the
                // same IMMEDIATE transaction as the upsert. It prevents both
                // concurrent first writers from observing an empty slot, and
                // applies copy protection to existing slots as well.
                let entries = transaction
                    .vault_list_entries()
                    .map_err(|e| format!("vault_list_entries: {e}"))?;
                for other in entries {
                    if other.name == name
                        || other.secret_type != memcore::vault::SECRET_TYPE_API_KEY
                        || crate::vault_ops::is_lane_slot_secret_name(&other.name)
                    {
                        continue;
                    }
                    let provider_kind =
                        crate::status_ops::status_health::provider_kind_for_env_name(&other.name)
                            .unwrap_or("unregistered");
                    let Ok(plain) = crate::vault_crypto::decrypt(
                        key.bytes(),
                        &other.encrypted_value,
                        &other.nonce,
                    ) else {
                        continue;
                    };
                    let Ok(mut other_value) = crate::vault_crypto::decode_utf8_zeroizing(
                        plain,
                        "Vault account secret is not valid UTF-8",
                    ) else {
                        continue;
                    };
                    let other_fingerprint = crate::vault_ops::fingerprint_secret(
                        key.bytes(),
                        provider_kind,
                        &other_value,
                    );
                    crate::vault_crypto::zero_string(&mut other_value);
                    if other_fingerprint
                        == crate::vault_ops::fingerprint_secret(
                            key.bytes(),
                            provider_kind,
                            &secret_value,
                        )
                    {
                        return Err(crate::vault_ops::copy_existing_account_message(
                            &name,
                            &other.name,
                        )
                        .into());
                    }
                }

                if let Some(existing) = transaction
                    .vault_get_entry(&name)
                    .map_err(|e| format!("vault_get_entry: {e}"))?
                {
                    crate::vault_ops::validate_existing_lane_slot_secret_type(
                        &name,
                        &existing.secret_type,
                    )?;
                    let old_bytes = crate::vault_crypto::decrypt(
                        key.bytes(),
                        &existing.encrypted_value,
                        &existing.nonce,
                    )?;
                    let mut old_value = crate::vault_crypto::decode_utf8_zeroizing(
                        old_bytes,
                        format!("Existing slot '{name}' is not valid UTF-8"),
                    )?;
                    let provider_kind =
                        crate::status_ops::status_health::provider_kind_for_env_name(&name)
                            .unwrap_or("unknown");
                    let overwrite = crate::vault_ops::evaluate_lane_slot_overwrite(
                        &old_value,
                        &secret_value,
                        provider_kind,
                        key.bytes(),
                        rebind,
                    );
                    crate::vault_crypto::zero_string(&mut old_value);
                    if let Err(err) = overwrite {
                        return Err(err.operator_message(&name).into());
                    }
                }
            }

            let encrypt_result = crate::vault_crypto::encrypt(key.bytes(), secret_value.as_bytes());
            let (encrypted_value, nonce) = encrypt_result?;

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

            transaction
                .vault_upsert_entry(&entry)
                .map_err(|e| format!("vault_upsert_entry: {e}"))?;
            transaction
                .commit()
                .map_err(|e| format!("commit vault transaction: {e}"))?;

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
            memcore::reject_api_key_type_for_lane_config(&prefix, memcore::SECRET_TYPE_API_KEY)?;
            if !crate::utils::is_shell_env_name(&prefix) {
                return Err(format!(
                    "API key pool prefix '{prefix}' must be a shell env name such as OPENAI_API_KEY"
                )
                .into());
            }

            let mut raw_values = crate::vault_crypto::ZeroizingString::new(String::new());
            stdin.read_to_string(raw_values.as_mut_string())?;
            let values = raw_values
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| crate::vault_crypto::ZeroizingString::new(line.to_string()))
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
            memcore::reject_api_key_type_for_lane_config(&name, memcore::SECRET_TYPE_API_KEY)?;
            if global_db_path.exists() {
                let store = open_cli_store_read_only(global_db_path)?;
                super::validate_api_key_lease_target(&store, &name)?;
            }
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

            let (logical_name, key_id, mut value) =
                lease_api_key_from_store(&store, key.bytes(), &name)?;
            let body = serde_json::json!({
                "leased": true,
                "logical_name": logical_name,
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
            let logical_name =
                crate::vault_ops::canonical_api_key_health_logical_name(&store, &logical_name)?;
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
            let mut value =
                crate::vault_crypto::decode_utf8_zeroizing(decrypted, "Secret is not valid UTF-8")?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn cli_lease_refuses_lane_config_before_daemon_or_store_access() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("must-not-be-created.sqlite");
        let action = VaultAction::Lease {
            name: "ENABLE_FALLBACK_API_KEY".to_string(),
            env_name: None,
            stdin_password: false,
            keychain: false,
            password_file: None,
            insecure_password_file: false,
            json: true,
        };
        let error = run_secret_action_with_reader(
            &db_path,
            temp.path(),
            action,
            &mut Cursor::new(Vec::<u8>::new()),
        )
        .await
        .expect_err("lane config must be refused before daemon forwarding")
        .to_string();
        assert!(
            error.contains("lane config") && error.contains("api_key"),
            "{error}"
        );
        assert!(
            !db_path.exists(),
            "preflight refusal must not create or open the Vault DB"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_cli_set_refuses_legacy_lane_type_without_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("memory.db");
        let password_file = temp.path().join("vault-password");
        std::fs::write(&password_file, b"direct-cli-password\n").expect("password file");
        std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
            .expect("password file permissions");

        let key = super::super::keys::vault_init_with_password(
            &db_path,
            "direct-cli-password".to_string(),
        )
        .expect("initialize fixture vault");
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key.bytes(), b"legacy-family").expect("encrypt fixture");
        let legacy = memcore::vault::VaultEntry {
            name: "EXTRACT_API_KEY".to_string(),
            encrypted_value: encrypted_value.clone(),
            nonce: nonce.clone(),
            secret_type: "other".to_string(),
            description: "legacy lane fixture".to_string(),
            allowed_agents: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            accessed_at: String::new(),
            access_count: 0,
        };
        open_cli_store(&db_path)
            .expect("open fixture store")
            .vault_upsert_entry(&legacy)
            .expect("seed legacy lane");

        let action = VaultAction::Set {
            name: "EXTRACT_API_KEY".to_string(),
            secret_type: Some("api_key".to_string()),
            description: None,
            stdin_password: false,
            keychain: false,
            password_file: Some(password_file),
            insecure_password_file: false,
            value_stdin: true,
            rebind: false,
        };
        let mut input = Cursor::new(b"replacement-family\n".to_vec());
        let error = run_secret_action_with_reader(&db_path, temp.path(), action, &mut input)
            .await
            .expect_err("legacy lane type must fail closed")
            .to_string();
        assert!(error.contains("legacy secret_type 'other'"), "{error}");
        assert!(!error.contains("replacement-family"), "{error}");

        let retained = open_cli_store_read_only(&db_path)
            .expect("open fixture store read-only")
            .vault_get_entry("EXTRACT_API_KEY")
            .expect("read lane")
            .expect("legacy lane remains");
        assert_eq!(retained.secret_type, "other");
        assert_eq!(retained.encrypted_value, encrypted_value);
        assert_eq!(retained.nonce, nonce);
    }
}
