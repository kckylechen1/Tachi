use super::*;
use std::io::{BufRead, Read, Write};
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
            let path = super::vault_sync::resolve_vault_sync_path(path)?;
            let status = super::vault_sync::vault_sync_status(&path)?;
            super::vault_sync::print_status(&status);
            Ok(())
        }

        VaultAction::SyncExport {
            output,
            allow_cloud,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let output = super::vault_sync::resolve_vault_sync_path(output)?;
            let config = read_vault_config_for_key(global_db_path)?;
            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;
            let status = super::vault_sync::export_vault_bundle(
                global_db_path,
                &output,
                allow_cloud,
                key.bytes(),
            )?;
            println!("Vault sync export complete.");
            super::vault_sync::print_status(&status);
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
            let input = super::vault_sync::resolve_vault_sync_path(input)?;
            let key = if allow_unsigned && !super::vault_sync::bundle_has_signature(&input)? {
                None
            } else {
                let config = super::vault_sync::read_bundle_vault_config(&input)?;
                Some(read_verified_vault_key(
                    &config,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?)
            };
            let report = super::vault_sync::import_vault_bundle(
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
            insecure_password_file,
        } => {
            let mut password = read_vault_password(
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;

            if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
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
            if !is_shell_env_name(&prefix) {
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

#[cfg(unix)]
async fn call_daemon_vault_unlock(
    app_home: &Path,
    info: &crate::cli_client::DaemonInfo,
    mut password: String,
) -> Result<String, Box<dyn std::error::Error>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let fifo_path = match (|| -> Result<_, Box<dyn std::error::Error>> {
        let unlock_dir = app_home.join("runtime").join("vault-unlock");
        std::fs::create_dir_all(&unlock_dir)?;
        std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))?;
        let fifo_path = unlock_dir.join(format!(
            "unlock-{}-{}.fifo",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let c_path = CString::new(fifo_path.as_os_str().as_bytes())?;
        let mkfifo_rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if mkfifo_rc != 0 {
            return Err(format!(
                "create unlock FIFO {}: {}",
                fifo_path.display(),
                std::io::Error::last_os_error()
            )
            .into());
        }
        Ok(fifo_path)
    })() {
        Ok(path) => path,
        Err(err) => {
            crate::vault_crypto::zero_string(&mut password);
            return Err(err);
        }
    };

    let mut args = serde_json::Map::new();
    args.insert(
        "password_fifo_path".to_string(),
        serde_json::json!(fifo_path.display().to_string()),
    );

    let writer_path = fifo_path.clone();
    let writer = tokio::task::spawn_blocking(move || {
        write_password_to_fifo_nonblocking(&writer_path, password)
    });
    let (call_result, writer_result) = tokio::join!(
        crate::cli_client::call_daemon_tool(info, "vault_unlock", args),
        async {
            writer
                .await
                .map_err(|e| format!("unlock FIFO writer task failed: {e}"))?
                .map_err(|e| e.to_string())
        }
    );
    let _ = std::fs::remove_file(&fifo_path);
    writer_result.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let out = call_result.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(out)
}

#[cfg(not(unix))]
async fn call_daemon_vault_unlock(
    _app_home: &Path,
    _info: &crate::cli_client::DaemonInfo,
    mut password: String,
) -> Result<String, Box<dyn std::error::Error>> {
    crate::vault_crypto::zero_string(&mut password);
    Err("daemon vault unlock password forwarding is disabled on non-Unix platforms".into())
}

#[cfg(unix)]
fn write_password_to_fifo_nonblocking(
    path: &Path,
    mut password: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let result = (|| {
        let c_path = CString::new(path.as_os_str().as_bytes())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
                file.write_all(password.as_bytes())?;
                file.flush()?;
                return Ok(());
            }

            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ENXIO) || std::time::Instant::now() >= deadline {
                return Err(format!("open unlock FIFO for writing failed: {err}").into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    })();
    crate::vault_crypto::zero_string(&mut password);
    result
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

fn read_vault_config_for_key(
    global_db_path: &PathBuf,
) -> Result<memory_core::vault::VaultConfig, Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;
    store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .ok_or_else(|| "Vault not initialized. Run `tachi vault init` first.".into())
}

fn read_verified_vault_key(
    config: &memory_core::vault::VaultConfig,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<crate::vault_crypto::DerivedVaultKey, Box<dyn std::error::Error>> {
    let mut password = read_vault_password(
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;
    derive_verified_vault_key_from_password(config, &mut password)
}

fn derive_verified_vault_key_from_password(
    config: &memory_core::vault::VaultConfig,
    password: &mut String,
) -> Result<crate::vault_crypto::DerivedVaultKey, Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key_result = crate::vault_crypto::DerivedVaultKey::derive(password, &salt);
    crate::vault_crypto::zero_string(password);
    let key = key_result?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Err("Wrong password".into());
    }
    Ok(key)
}

fn decrypt_profile_secret_values(
    global_db_path: &PathBuf,
    profile: &crate::credential_profile::CredentialProfile,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<std::collections::HashMap<String, String>, Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;
    let config = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
    let key = read_verified_vault_key(
        &config,
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;

    let mut values = std::collections::HashMap::new();
    for name in crate::credential_profile::profile_secret_names(profile) {
        let entry = store
            .vault_get_entry(&name)
            .map_err(|e| format!("vault_get_entry: {e}"))?
            .ok_or_else(|| format!("Vault secret '{name}' is missing"))?;
        let decrypted =
            crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
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
        let mut value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
        if value.trim().is_empty() {
            crate::vault_crypto::zero_string(&mut value);
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

fn vault_get_output(
    name: &str,
    value: &str,
    reveal: bool,
    json_output: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    if json_output {
        let mut body = serde_json::json!({
            "name": name,
            "revealed": reveal,
        });
        if reveal {
            body["value"] = serde_json::json!(value);
        } else {
            body["value"] = serde_json::json!("<redacted>");
            body["hint"] = serde_json::json!("Pass --reveal to print the decrypted secret value.");
        }
        return Ok(format!("{}\n", serde_json::to_string_pretty(&body)?));
    }

    if reveal {
        return Ok(format!("{value}\n"));
    }

    Ok(format!(
        "Secret '{name}' exists; value hidden. Re-run with --reveal to print the decrypted value.\n"
    ))
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
    insecure_password_file: bool,
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
        read_password_file(path, insecure_password_file)?
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

pub(super) fn read_vault_init_password(
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    confirm_password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let (mut password, mut confirm) = if stdin_password {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        read_vault_init_password_stdin_lines(
            &mut stdin,
            confirm_password_file,
            insecure_password_file,
        )?
    } else if keychain || password_file.is_some() {
        let password = read_vault_password(false, keychain, password_file, insecure_password_file)?;
        let Some(path) = confirm_password_file else {
            return Err(
                "Non-interactive vault init requires --confirm-password-file. Use interactive `tachi vault init` or provide a separate confirmation file."
                    .into(),
            );
        };
        (password, read_password_file(path, insecure_password_file)?)
    } else {
        let password = rpassword::prompt_password("New vault password: ")?;
        let confirm = rpassword::prompt_password("Confirm password: ")?;
        (password, confirm)
    };

    if password.is_empty() {
        crate::vault_crypto::zero_string(&mut password);
        crate::vault_crypto::zero_string(&mut confirm);
        return Err("Password cannot be empty".into());
    }
    if password != confirm {
        crate::vault_crypto::zero_string(&mut password);
        crate::vault_crypto::zero_string(&mut confirm);
        return Err("Passwords do not match".into());
    }
    crate::vault_crypto::zero_string(&mut confirm);
    Ok(password)
}

fn read_vault_init_password_stdin_lines(
    reader: &mut impl BufRead,
    confirm_password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let mut password = String::new();
    reader.read_line(&mut password)?;
    let password = password.trim().to_string();
    let confirm = if let Some(path) = confirm_password_file {
        read_password_file(path, insecure_password_file)?
    } else {
        let mut confirm = String::new();
        reader.read_line(&mut confirm)?;
        confirm.trim().to_string()
    };
    Ok((password, confirm))
}

fn read_password_file(
    path: &Path,
    insecure_password_file: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read password file {}: {e}", path.display()))?;
    let password = raw.lines().next().unwrap_or_default().trim().to_string();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                if insecure_password_file {
                    eprintln!(
                        "WARNING: password file {} is readable by group/other (mode {:o}); prefer 0600",
                        path.display(),
                        mode
                    );
                } else {
                    return Err(format!(
                        "Password file {} is readable by group/other (mode {:o}). Set permissions to 0600 or pass --insecure-password-file.",
                        path.display(),
                        mode
                    )
                    .into());
                }
            }
        }
    }

    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;

    #[cfg(unix)]
    fn make_owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("set owner-only permissions");
    }

    #[cfg(not(unix))]
    fn make_owner_only(_path: &Path) {}

    #[cfg(unix)]
    fn make_group_readable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))
            .expect("set group-readable permissions");
    }

    fn config_for_password(password: &str) -> memory_core::vault::VaultConfig {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};

        let salt = crate::vault_crypto::generate_salt();
        let key =
            crate::vault_crypto::DerivedVaultKey::derive(password, &salt).expect("derive test key");
        let verifier = crate::vault_crypto::create_verifier(key.bytes()).expect("create verifier");
        memory_core::vault::VaultConfig {
            salt: B64.encode(salt),
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
            cipher: memory_core::vault::VaultCipher::Aes256Gcm,
            created_at: "2026-06-14T00:00:00Z".to_string(),
            updated_at: "2026-06-14T00:00:00Z".to_string(),
        }
    }

    fn string_is_zeroed(value: &str) -> bool {
        value.as_bytes().iter().all(|byte| *byte == 0)
    }

    #[test]
    fn stdin_init_password_reads_two_lines_without_waiting_for_eof() {
        let mut input =
            Cursor::new("correct horse battery staple\ncorrect horse battery staple\nextra\n");
        let (password, confirm) =
            read_vault_init_password_stdin_lines(&mut input, None, false).expect("stdin lines");

        assert_eq!(password, "correct horse battery staple");
        assert_eq!(confirm, "correct horse battery staple");
        let mut remaining = String::new();
        input
            .read_to_string(&mut remaining)
            .expect("read remaining stdin");
        assert_eq!(remaining, "extra\n");
    }

    #[test]
    fn derive_verified_vault_key_zeroes_password_on_success() {
        let config = config_for_password("correct horse battery staple");
        let mut password = "correct horse battery staple".to_string();

        let _key = derive_verified_vault_key_from_password(&config, &mut password)
            .expect("verified password should derive key");

        assert!(
            string_is_zeroed(&password),
            "password buffer was not zeroed"
        );
    }

    #[test]
    fn derive_verified_vault_key_zeroes_password_on_wrong_password() {
        let config = config_for_password("correct horse battery staple");
        let mut password = "wrong horse battery staple".to_string();

        let err = match derive_verified_vault_key_from_password(&config, &mut password) {
            Ok(_) => panic!("wrong password should fail verification"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("Wrong password"), "{err}");
        assert!(
            string_is_zeroed(&password),
            "password buffer was not zeroed"
        );
    }

    #[test]
    fn vault_get_output_redacts_by_default() {
        let out =
            vault_get_output("GH_TOKEN", "ghp_secret_value", false, false).expect("format output");
        assert!(out.contains("GH_TOKEN"));
        assert!(out.contains("--reveal"));
        assert!(
            !out.contains("ghp_secret_value"),
            "default get output must not reveal the secret: {out}"
        );
    }

    #[test]
    fn vault_get_output_reveals_only_when_requested() {
        let plain =
            vault_get_output("GH_TOKEN", "ghp_secret_value", true, false).expect("plain output");
        assert_eq!(plain, "ghp_secret_value\n");

        let json =
            vault_get_output("GH_TOKEN", "ghp_secret_value", false, true).expect("json output");
        assert!(json.contains("\"revealed\": false"));
        assert!(json.contains("<redacted>"));
        assert!(
            !json.contains("ghp_secret_value"),
            "redacted JSON must not reveal the secret: {json}"
        );
    }

    #[test]
    fn noninteractive_init_password_file_requires_confirmation_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let password_file = dir.path().join("password.txt");
        std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
        make_owner_only(&password_file);

        let err = read_vault_init_password(false, false, Some(&password_file), None, false)
            .expect_err("missing confirmation file should fail");
        assert!(err.to_string().contains("--confirm-password-file"), "{err}");
    }

    #[test]
    fn noninteractive_init_password_file_must_match_confirmation_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let password_file = dir.path().join("password.txt");
        let confirm_file = dir.path().join("confirm.txt");
        std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
        std::fs::write(&confirm_file, "wrong horse battery staple\n").expect("confirm file");
        make_owner_only(&password_file);
        make_owner_only(&confirm_file);

        let err = read_vault_init_password(
            false,
            false,
            Some(&password_file),
            Some(&confirm_file),
            false,
        )
        .expect_err("mismatched confirmation should fail");
        assert!(err.to_string().contains("Passwords do not match"), "{err}");

        std::fs::write(&confirm_file, "correct horse battery staple\n").expect("confirm file");
        make_owner_only(&confirm_file);
        let password = read_vault_init_password(
            false,
            false,
            Some(&password_file),
            Some(&confirm_file),
            false,
        )
        .expect("matching confirmation should succeed");
        assert_eq!(password, "correct horse battery staple");
    }

    #[test]
    #[cfg(unix)]
    fn password_file_rejects_group_or_other_readable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let password_file = dir.path().join("password.txt");
        std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
        make_group_readable(&password_file);

        let err = read_password_file(&password_file, false)
            .expect_err("group-readable password file should be rejected");
        let msg = err.to_string();
        assert!(msg.contains("readable by group/other"), "{msg}");
        assert!(msg.contains("--insecure-password-file"), "{msg}");
    }

    #[test]
    #[cfg(unix)]
    fn password_file_allows_insecure_opt_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let password_file = dir.path().join("password.txt");
        std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
        make_group_readable(&password_file);

        let password = read_password_file(&password_file, true).expect(
            "group-readable password file should be accepted with --insecure-password-file",
        );
        assert_eq!(password, "correct horse battery staple");
    }

    #[test]
    fn password_file_accepts_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let password_file = dir.path().join("password.txt");
        std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
        make_owner_only(&password_file);

        let password = read_password_file(&password_file, false)
            .expect("owner-only password file should be accepted");
        assert_eq!(password, "correct horse battery staple");
    }
}
