use crate::server_state::{CachedVaultKey, MemoryServer};
use crate::vault_crypto as crypto;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use memory_core::vault::{
    normalize_secret_type, VaultCipher, VaultConfig, VaultEntry, VaultKeyRotation,
    SECRET_TYPE_API_KEY,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

use super::access::{
    ensure_agent_allowed, load_unlocked_api_key_secret_pool, record_successful_vault_access,
    select_vault_entry,
};
use super::audit::{record_vault_audit, result_with_vault_audit_warning};
use super::env::attach_provider_refresh_warning;
use super::params::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use super::rotation::{
    collect_rotation_entries, normalize_allowed_agents, normalize_rotation_strategy,
};
use super::session::{
    clear_cached_vault_state, ensure_vault_unlock_allowed, ensure_vault_unlocked,
    is_vault_initialized, maybe_auto_lock_vault, read_unlock_password_fifo,
    record_vault_unlock_failure, with_vault_key,
};

pub(crate) async fn handle_vault_init(
    server: &MemoryServer,
    params: VaultInitParams,
) -> Result<String, String> {
    let result = (|| {
        if is_vault_initialized(server)? {
            return Err("Vault already initialized. Use vault_unlock to unlock.".into());
        }

        if params.password.len() < 8 {
            return Err("Password must be at least 8 characters long.".into());
        }

        let salt = crypto::generate_salt();
        let salt_b64 = B64.encode(salt);
        let key = crypto::DerivedVaultKey::derive(&params.password, &salt)?;
        let verifier = crypto::create_verifier(key.bytes())?;
        let now = Utc::now().to_rfc3339();
        let config = VaultConfig {
            salt: salt_b64,
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
            cipher: VaultCipher::Aes256Gcm,
            created_at: now.clone(),
            updated_at: now,
        };

        server
            .with_global_store(|store| store.vault_set_config(&config).map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to save vault config: {e}"))?;

        {
            let mut v = server.vault_write();
            v.key = Some(CachedVaultKey::copy_from(key.bytes()));
            v.unlock_time = Some(Instant::now());
            v.failed_attempts = (0, None);
        }

        serde_json::to_string(&json!({
            "initialized": true,
            "locked": false,
            "message": "Vault initialized and unlocked"
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    let audit_result = record_vault_audit(
        server,
        "vault_init",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_unlock(
    server: &MemoryServer,
    params: VaultUnlockParams,
) -> Result<String, String> {
    let result = async {
        ensure_vault_unlock_allowed(server)?;

        let config = server
            .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to load vault config: {e}"))?
            .ok_or_else(|| "Vault not initialized. Call vault_init first.".to_string())?;

        let salt = B64
            .decode(&config.salt)
            .map_err(|e| format!("Invalid salt in vault config: {e}"))?;
        let fifo_path = params
            .password_fifo_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if fifo_path.is_some() && !params.password.is_empty() {
            return Err(
                "vault_unlock accepts either password or password_fifo_path, not both".to_string(),
            );
        }
        let mut fifo_password = match fifo_path {
            Some(path) => Some(
                tokio::task::spawn_blocking(move || read_unlock_password_fifo(&path))
                    .await
                    .map_err(|e| format!("unlock FIFO reader task failed: {e}"))??,
            ),
            None => None,
        };
        let password = match fifo_password.as_deref() {
            Some(password) => password,
            None => {
                if params.password.is_empty() {
                    return Err("vault_unlock requires password or password_fifo_path".to_string());
                }
                &params.password
            }
        };
        let key = match crypto::DerivedVaultKey::derive(password, &salt) {
            Ok(key) => key,
            Err(err) => {
                if let Some(password) = fifo_password.as_mut() {
                    crypto::zero_string(password);
                }
                return Err(err);
            }
        };
        if let Some(password) = fifo_password.as_mut() {
            crypto::zero_string(password);
        }

        if !crypto::verify_password(key.bytes(), &config.verifier)? {
            return record_vault_unlock_failure(server);
        }

        {
            let mut v = server.vault_write();
            v.key = Some(CachedVaultKey::copy_from(key.bytes()));
            v.unlock_time = Some(Instant::now());
            v.failed_attempts = (0, None);
        }

        serde_json::to_string(&json!({
            "unlocked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    }
    .await;

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    let audit_result = record_vault_audit(
        server,
        "vault_unlock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_lock(server: &MemoryServer) -> Result<String, String> {
    let result = {
        clear_cached_vault_state(server);
        serde_json::to_string(&json!({
            "locked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    };

    let audit_result = record_vault_audit(
        server,
        "vault_lock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_set(
    server: &MemoryServer,
    params: VaultSetParams,
) -> Result<String, String> {
    let secret_name = params.name.clone();
    let result = (|| {
        crypto::validate_secret_name(&params.name)?;
        with_vault_key(server, |key| {
            let secret_type = normalize_secret_type(&params.secret_type);
            let allowed_agents = normalize_allowed_agents(params.allowed_agents.clone());
            let (encrypted_value, nonce) = crypto::encrypt(key, params.value.as_bytes())?;

            let is_new = !server
                .with_global_store(|store| {
                    store
                        .vault_entry_exists(&params.name)
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| format!("Failed to check existing entry: {e}"))?;

            let now = Utc::now().to_rfc3339();
            let entry = VaultEntry {
                name: params.name.clone(),
                encrypted_value,
                nonce,
                secret_type: secret_type.to_string(),
                description: params.description.clone(),
                allowed_agents,
                created_at: if is_new { now.clone() } else { String::new() },
                updated_at: now,
                accessed_at: String::new(),
                access_count: 0,
            };

            server
                .with_global_store(|store| {
                    store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
                })
                .map_err(|e| format!("Failed to save secret: {e}"))?;

            if params.enable_rotation {
                if let Some(pos) = params.name.rfind('_') {
                    let suffix = &params.name[pos + 1..];
                    if suffix.parse::<u32>().is_ok() {
                        let prefix = &params.name[..pos];
                        let strategy = normalize_rotation_strategy(
                            &params
                                .rotation_strategy
                                .clone()
                                .unwrap_or_else(|| "round_robin".to_string()),
                        );

                        let all_entries = server
                            .with_global_store_read(|store| {
                                store.vault_list_entries().map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("Failed to list entries: {e}"))?;

                        let total_keys = collect_rotation_entries(all_entries, prefix).len() as i64;
                        let rotation = VaultKeyRotation {
                            prefix: prefix.to_string(),
                            current_index: 1,
                            total_keys,
                            rotation_strategy: strategy,
                            created_at: Utc::now().to_rfc3339(),
                            updated_at: Utc::now().to_rfc3339(),
                        };

                        server
                            .with_global_store(|store| {
                                store
                                    .vault_set_rotation(&rotation)
                                    .map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("Failed to save rotation config: {e}"))?;
                    }
                }
            }

            serde_json::to_string(&json!({
                "stored": true,
                "name": params.name,
                "secret_type": secret_type,
                "created": is_new
            }))
            .map_err(|e| format!("serialize: {e}"))
        })
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    let audit_result = record_vault_audit(
        server,
        "vault_set",
        Some(&secret_name),
        result.is_ok(),
        match &result {
            Ok(_) => Some("stored"),
            Err(err) => Some(err.as_str()),
        },
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_get(
    server: &MemoryServer,
    params: VaultGetParams,
) -> Result<String, String> {
    let requested_name = params.name.clone();
    let result = with_vault_key(server, |key| {
        let selected = server.with_global_store(|store| select_vault_entry(store, &params))?;

        ensure_agent_allowed(&selected.entry, params.agent_id.as_deref())?;

        let decrypted =
            crypto::decrypt(key, &selected.entry.encrypted_value, &selected.entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted value is not valid UTF-8: {e}"))?;

        let new_access_count = server
            .with_global_store(|store| {
                record_successful_vault_access(
                    store,
                    &selected.target_name,
                    selected.pending_rotation.as_ref(),
                )
            })
            .map_err(|e| format!("Failed to update access stats: {e}"))?;

        serde_json::to_string(&json!({
            "name": selected.entry.name,
            "value": value,
            "secret_type": selected.entry.secret_type,
            "description": selected.entry.description,
            "allowed_agents": selected.entry.allowed_agents,
            "access_count": new_access_count,
        }))
        .map_err(|e| format!("serialize: {e}"))
    });

    let audit_result = record_vault_audit(
        server,
        "vault_get",
        Some(&requested_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_list(
    server: &MemoryServer,
    params: VaultListParams,
) -> Result<String, String> {
    if !is_vault_initialized(server)? {
        return Err("Vault not initialized. Call vault_init first.".into());
    }

    let entries = if let Some(ref secret_type) = params.secret_type {
        let secret_type = normalize_secret_type(secret_type);
        server.with_global_store_read(|store| {
            store
                .vault_list_entries_by_type(secret_type)
                .map_err(|e| e.to_string())
        })
    } else {
        server.with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
    }
    .map_err(|e| format!("Failed to list secrets: {e}"))?;

    let payload: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "name": e.name,
                "secret_type": e.secret_type,
                "description": e.description,
                "allowed_agents": e.allowed_agents,
                "created_at": e.created_at,
                "updated_at": e.updated_at,
                "access_count": e.access_count,
            })
        })
        .collect();

    let resp = json!({
        "count": payload.len(),
        "secrets": payload,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vault_remove(
    server: &MemoryServer,
    params: VaultRemoveParams,
) -> Result<String, String> {
    let secret_name = params.name.clone();
    let result = (|| {
        ensure_vault_unlocked(server)?;
        let removed = server
            .with_global_store(|store| {
                store
                    .vault_delete_entry(&params.name)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("Failed to remove secret: {e}"))?;

        if removed {
            serde_json::to_string(&json!({
                "removed": true,
                "name": params.name
            }))
            .map_err(|e| format!("serialize: {e}"))
        } else {
            Err(format!("Secret not found: {}", params.name))
        }
    })();

    let audit_result = record_vault_audit(
        server,
        "vault_remove",
        Some(&secret_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );

    let result = match result {
        Ok(value) => Ok(value),
        Err(err) if err.starts_with("Secret not found: ") => serde_json::to_string(&json!({
            "removed": false,
            "error": err,
        }))
        .map_err(|e| format!("serialize: {e}")),
        Err(err) => Err(err),
    };
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_status(server: &MemoryServer) -> Result<String, String> {
    let initialized = is_vault_initialized(server)?;
    maybe_auto_lock_vault(server);
    let (locked, auto_lock_secs) = {
        let v = server.vault_read();
        (v.key.is_none(), v.auto_lock_after_secs)
    };
    let entry_count = if initialized {
        server
            .with_global_store_read(|store| store.vault_count_entries().map_err(|e| e.to_string()))
            .unwrap_or(0)
    } else {
        0
    };

    let resp = json!({
        "initialized": initialized,
        "locked": locked,
        "entry_count": entry_count,
        "auto_lock_after_secs": auto_lock_secs,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vault_setup_rotation(
    server: &MemoryServer,
    params: VaultSetupRotationParams,
) -> Result<String, String> {
    ensure_vault_unlocked(server)?;

    if params.total_keys < 2 {
        return Err("Rotation requires at least 2 keys".into());
    }

    let strategy = normalize_rotation_strategy(&params.strategy);
    let all_entries = server
        .with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
        .map_err(|e| format!("Failed to list entries: {e}"))?;

    let mut found_keys = 0;
    for i in 1..=params.total_keys {
        let key_name = format!("{}_{}", params.prefix, i);
        if all_entries.iter().any(|e| e.name == key_name) {
            found_keys += 1;
        }
    }

    if found_keys < params.total_keys {
        return Err(format!(
            "Expected {} keys for prefix '{}', found {}. Please set all keys first.",
            params.total_keys, params.prefix, found_keys
        ));
    }

    let now = Utc::now().to_rfc3339();
    let rotation = VaultKeyRotation {
        prefix: params.prefix.clone(),
        current_index: 1,
        total_keys: params.total_keys,
        rotation_strategy: strategy.clone(),
        created_at: now.clone(),
        updated_at: now,
    };

    server
        .with_global_store(|store| {
            store
                .vault_set_rotation(&rotation)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| format!("Failed to save rotation config: {e}"))?;

    let resp = json!({
        "setup": true,
        "prefix": params.prefix,
        "total_keys": params.total_keys,
        "strategy": strategy,
    });
    let body = serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))?;
    attach_provider_refresh_warning(server, body)
}

pub(crate) async fn handle_vault_set_api_key_pool(
    server: &MemoryServer,
    params: VaultSetApiKeyPoolParams,
) -> Result<String, String> {
    let logical_name = params.prefix.clone();
    let result = (|| {
        crypto::validate_secret_name(&params.prefix)?;
        if !crate::utils::is_shell_env_name(&params.prefix) {
            return Err(format!(
                "API key pool prefix '{}' must be a shell env name such as OPENAI_API_KEY",
                params.prefix
            ));
        }
        let values = params
            .values
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() {
            return Err("API key pool requires at least one non-empty value".to_string());
        }

        let allowed_agents = normalize_allowed_agents(params.allowed_agents.clone());
        let now = Utc::now().to_rfc3339();
        let strategy = normalize_rotation_strategy(&params.strategy);
        let removed_members = with_vault_key(server, |key| {
            let mut entries = Vec::with_capacity(values.len());
            for (idx, value) in values.iter().enumerate() {
                let name = format!("{}_{}", params.prefix, idx + 1);
                let (encrypted_value, nonce) = crypto::encrypt(key, value.as_bytes())?;
                entries.push(VaultEntry {
                    name,
                    encrypted_value,
                    nonce,
                    secret_type: SECRET_TYPE_API_KEY.to_string(),
                    description: params.description.clone(),
                    allowed_agents: allowed_agents.clone(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    accessed_at: String::new(),
                    access_count: 0,
                });
            }
            let rotation = VaultKeyRotation {
                prefix: params.prefix.clone(),
                current_index: 1,
                total_keys: values.len() as i64,
                rotation_strategy: strategy.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            server
                .with_global_store(|store| {
                    store
                        .vault_replace_api_key_pool(&params.prefix, &entries, &rotation)
                        .map_err(|e| format!("vault_replace_api_key_pool: {e}"))
                })
                .map_err(|e| format!("save API key pool: {e}"))
        })?;

        serde_json::to_string(&json!({
            "stored": true,
            "logical_name": params.prefix,
            "total_keys": values.len(),
            "strategy": strategy,
            "members": (1..=values.len()).map(|idx| format!("{}_{}", logical_name, idx)).collect::<Vec<_>>(),
            "removed_members": removed_members,
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));
    let audit_result = record_vault_audit(
        server,
        "vault_set_api_key_pool",
        Some(&logical_name),
        result.is_ok(),
        match &result {
            Ok(_) => Some("stored"),
            Err(err) => Some(err.as_str()),
        },
    );
    result_with_vault_audit_warning(result, audit_result)
}

fn advance_rotation_after_key(
    server: &MemoryServer,
    logical_name: &str,
    key_id: &str,
) -> Result<(), String> {
    let Some((prefix, idx)) = crate::provider_config::parse_rotation_member_name(key_id) else {
        return Ok(());
    };
    if prefix != logical_name {
        return Ok(());
    }

    server
        .with_global_store(|store| {
            let Some(rotation) = store
                .vault_get_rotation(logical_name)
                .map_err(|e| e.to_string())?
            else {
                return Ok(());
            };
            if rotation.total_keys <= 0 {
                return Ok(());
            }
            let next = (idx as i64 % rotation.total_keys) + 1;
            let updated = VaultKeyRotation {
                current_index: next,
                updated_at: Utc::now().to_rfc3339(),
                ..rotation
            };
            store
                .vault_set_rotation(&updated)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| format!("advance rotation: {e}"))
}

pub(crate) async fn handle_vault_lease_api_key(
    server: &MemoryServer,
    params: VaultLeaseApiKeyParams,
) -> Result<String, String> {
    let requested_name = params.name.clone();
    let mut success_audit_detail = None;
    let result = (|| {
        let env_name = params
            .env_name
            .clone()
            .unwrap_or_else(|| params.name.clone());
        if !crate::utils::is_shell_env_name(&env_name) {
            return Err(format!(
                "env_name '{}' must be a valid shell env name",
                env_name
            ));
        }

        let pool = load_unlocked_api_key_secret_pool(server, &params.name)?;
        let selected = pool.first().ok_or_else(|| {
            format!(
                "No usable API key available for '{}'. Vault may be locked, missing, or all keys are disabled/auth-failed/rate-limited.",
                params.name
            )
        })?;

        advance_rotation_after_key(server, &params.name, &selected.key_id)?;
        let access_count = server
            .with_global_store(|store| {
                store
                    .vault_touch_entry(&selected.key_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap_or(0);

        success_audit_detail = Some(
            json!({
                "logical_name": params.name.clone(),
                "key_id": selected.key_id.clone(),
                "env_name": env_name.clone(),
                "agent_id": params.agent_id.clone(),
            })
            .to_string(),
        );

        serde_json::to_string(&json!({
            "leased": true,
            "logical_name": params.name,
            "key_id": selected.key_id,
            "env_name": env_name,
            "agent_id": params.agent_id,
            "env": {
                env_name: selected.value
            },
            "access_count": access_count,
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let audit_detail = match result.as_ref() {
        Ok(_) => success_audit_detail.as_deref(),
        Err(err) => Some(err.as_str()),
    };
    let audit_result = record_vault_audit(
        server,
        "vault_lease_api_key",
        Some(&requested_name),
        result.is_ok(),
        audit_detail,
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_record_key_result(
    server: &MemoryServer,
    params: VaultRecordKeyResultParams,
) -> Result<String, String> {
    let logical_name = params.logical_name.trim().to_string();
    let key_id = params.key_id.trim().to_string();
    if logical_name.is_empty() || key_id.is_empty() {
        return Err("logical_name and key_id are required".to_string());
    }
    let llm = Arc::clone(&server.llm);
    let record_logical_name = logical_name.clone();
    let record_key_id = key_id.clone();
    let outcome = params.outcome.clone();
    let reason = params.reason.clone();
    let status_code = params.status_code;
    let retry_after_secs = params.retry_after_secs;
    let health = tokio::task::spawn_blocking(move || {
        llm.record_provider_key_result_blocking(
            &record_logical_name,
            &record_key_id,
            status_code,
            outcome.as_deref(),
            retry_after_secs,
            reason.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("record provider key result task failed: {e}"))?;
    let skipped_by_lease = health.disabled
        || health.auth_failed
        || matches!(
            health.status.as_str(),
            "exhausted" | "rate_limited" | "cooldown"
        );
    let body = json!({
        "recorded": true,
        "logical_name": logical_name,
        "key_id": key_id,
        "status_code": params.status_code,
        "outcome": params.outcome,
        "skipped_by_lease": skipped_by_lease,
        "health": {
            "status": health.status,
            "cooldown_until": health.cooldown_until,
            "auth_failed": health.auth_failed,
            "disabled": health.disabled,
            "error_count": health.error_count,
        },
    });
    let result = serde_json::to_string(&body).map_err(|e| format!("serialize: {e}"));
    let audit_result = record_vault_audit(
        server,
        "vault_record_key_result",
        Some(&params.logical_name),
        result.is_ok(),
        params.reason.as_deref(),
    );
    result_with_vault_audit_warning(result, audit_result)
}
