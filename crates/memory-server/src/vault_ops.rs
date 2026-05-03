// vault_ops.rs — MCP tool handlers for Tachi Vault

use super::*;
use crate::vault_crypto as crypto;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use memory_core::vault::{VaultConfig, VaultEntry, VaultKeyRotation};
use serde_json::json;

const VAULT_UNLOCK_MAX_FAILED_ATTEMPTS: u32 = 5;
const VAULT_UNLOCK_LOCKOUT_SECS: u64 = 300;

fn default_secret_type() -> String {
    "api_key".to_string()
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultInitParams {
    /// Master password for the vault
    pub password: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultUnlockParams {
    /// Master password for the vault
    pub password: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultSetParams {
    /// Secret name (e.g., "OPENAI_API_KEY")
    pub name: String,
    /// Secret value (will be encrypted)
    pub value: String,
    /// Secret type: api_key, oauth_token, json_blob, cookie, other
    #[serde(default = "default_secret_type")]
    pub secret_type: String,
    /// Optional human-readable description
    #[serde(default)]
    pub description: String,
    /// Agent IDs allowed to read this secret. When absent, any caller may read it.
    #[serde(default)]
    pub allowed_agents: Option<Vec<String>>,
    /// If true and secret name ends with _N pattern, set up key rotation
    #[serde(default)]
    pub enable_rotation: bool,
    /// Rotation strategy: round_robin, random, least_recently_used (default: round_robin)
    #[serde(default)]
    pub rotation_strategy: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultGetParams {
    /// Secret name to retrieve
    pub name: String,
    /// Agent ID requesting the secret. Required when allowed_agents is set.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// If true and this is a rotation prefix, auto-select next key
    #[serde(default)]
    pub auto_rotate: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultListParams {
    /// Optional filter by secret type
    #[serde(default)]
    pub secret_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultRemoveParams {
    /// Secret name to remove
    pub name: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultSetupRotationParams {
    /// Key prefix (e.g., "GEMINI_API_KEY" for GEMINI_API_KEY_1, GEMINI_API_KEY_2)
    pub prefix: String,
    /// Total number of keys in rotation
    pub total_keys: i64,
    /// Rotation strategy: round_robin, random, least_recently_used
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,
}

fn default_rotation_strategy() -> String {
    "round_robin".to_string()
}

fn normalize_rotation_strategy(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" => "round_robin".to_string(),
        "random" => "random".to_string(),
        "least_recently_used" | "lru" | "least-recently-used" => "least_recently_used".to_string(),
        _ => "round_robin".to_string(),
    }
}

fn normalize_allowed_agents(allowed_agents: Option<Vec<String>>) -> Option<Vec<String>> {
    allowed_agents.and_then(|agents| {
        let normalized: Vec<String> = agents
            .into_iter()
            .map(|agent| agent.trim().to_string())
            .filter(|agent| !agent.is_empty())
            .collect();
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    })
}

fn rotation_index(name: &str, prefix: &str) -> Option<u32> {
    let suffix = name.strip_prefix(prefix)?.strip_prefix('_')?;
    suffix.parse::<u32>().ok()
}

fn collect_rotation_entries(entries: Vec<VaultEntry>, prefix: &str) -> Vec<(u32, VaultEntry)> {
    let mut matching: Vec<(u32, VaultEntry)> = entries
        .into_iter()
        .filter_map(|entry| rotation_index(&entry.name, prefix).map(|index| (index, entry)))
        .collect();
    matching.sort_by_key(|(index, _)| *index);
    matching
}

fn remaining_lockout_seconds(until: Instant) -> u64 {
    let remaining = until.saturating_duration_since(Instant::now());
    let secs = remaining.as_secs();
    if remaining.subsec_nanos() > 0 {
        secs.saturating_add(1)
    } else {
        secs
    }
}

fn clear_cached_vault_state(server: &MemoryServer) {
    *write_or_recover(&server.vault_key, "vault_key") = None;
    *write_or_recover(&server.vault_unlock_time, "vault_unlock_time") = None;
    server.llm.clear_provider_secrets();
}

fn maybe_auto_lock_vault(server: &MemoryServer) -> bool {
    let unlock_time = *read_or_recover(&server.vault_unlock_time, "vault_unlock_time");
    let Some(unlock_time) = unlock_time else {
        return false;
    };

    if unlock_time.elapsed() > Duration::from_secs(server.vault_auto_lock_after_secs) {
        clear_cached_vault_state(server);
        return true;
    }

    false
}

fn record_vault_audit(
    server: &MemoryServer,
    operation: &str,
    secret_name: Option<&str>,
    success: bool,
    detail: Option<&str>,
) {
    let timestamp = Utc::now().to_rfc3339();
    if let Err(err) = server.with_global_store(|store| {
        store
            .vault_insert_audit(&timestamp, operation, secret_name, success, detail)
            .map_err(|e| e.to_string())
    }) {
        eprintln!("WARNING: failed to record vault audit: {err}");
    }
}

/// Check if vault is unlocked and return the cached key.
fn get_vault_key(server: &MemoryServer) -> Result<[u8; 32], String> {
    if maybe_auto_lock_vault(server) {
        return Err("Vault auto-locked. Call vault_unlock first.".into());
    }

    read_or_recover(&server.vault_key, "vault_key")
        .as_ref()
        .copied()
        .ok_or_else(|| "Vault is locked. Call vault_unlock first.".to_string())
}

/// Lightweight check: vault is unlocked AND contains a secret with the given name.
/// Does not decrypt or touch access stats.
pub(crate) fn vault_has_secret(server: &MemoryServer, name: &str) -> bool {
    if get_vault_key(server).is_err() {
        return false;
    }
    let name_owned = name.to_string();
    server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(&name_owned)
                .map(|opt| opt.is_some())
                .map_err(|e| e.to_string())
        })
        .unwrap_or(false)
}

/// Check if vault is initialized.
fn is_vault_initialized(server: &MemoryServer) -> Result<bool, String> {
    server
        .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
        .map(|opt| opt.is_some())
}

fn ensure_vault_unlock_allowed(server: &MemoryServer) -> Result<(), String> {
    let mut state = lock_or_recover(&server.vault_failed_attempts, "vault_failed_attempts");
    if let Some(until) = state.1 {
        if Instant::now() < until {
            return Err(format!(
                "Vault unlock temporarily locked. Try again in {} seconds.",
                remaining_lockout_seconds(until)
            ));
        }
        *state = (0, None);
    }
    Ok(())
}

fn record_vault_unlock_failure(server: &MemoryServer) -> Result<String, String> {
    let mut state = lock_or_recover(&server.vault_failed_attempts, "vault_failed_attempts");
    state.0 = state.0.saturating_add(1);
    if state.0 >= VAULT_UNLOCK_MAX_FAILED_ATTEMPTS {
        let until = Instant::now() + Duration::from_secs(VAULT_UNLOCK_LOCKOUT_SECS);
        state.1 = Some(until);
        return Err(format!(
            "Too many failed vault unlock attempts. Try again in {} seconds.",
            remaining_lockout_seconds(until)
        ));
    }
    Err("Wrong password".to_string())
}

fn reset_vault_unlock_failures(server: &MemoryServer) {
    *lock_or_recover(&server.vault_failed_attempts, "vault_failed_attempts") = (0, None);
}

fn ensure_agent_allowed(entry: &VaultEntry, agent_id: Option<&str>) -> Result<(), String> {
    let Some(allowed_agents) = entry.allowed_agents.as_ref() else {
        return Ok(());
    };

    let Some(agent_id) = agent_id.map(str::trim).filter(|agent| !agent.is_empty()) else {
        return Err(format!(
            "Access denied for secret '{}': agent_id is required.",
            entry.name
        ));
    };

    if allowed_agents.iter().any(|allowed| allowed == agent_id) {
        Ok(())
    } else {
        Err(format!(
            "Access denied for agent '{}' to secret '{}'.",
            agent_id, entry.name
        ))
    }
}

fn select_vault_entry(
    store: &mut MemoryStore,
    params: &VaultGetParams,
) -> Result<(String, VaultEntry), String> {
    let exact_entry = store
        .vault_get_entry(&params.name)
        .map_err(|e| format!("Failed to get secret: {e}"))?;
    let rotation = store
        .vault_get_rotation(&params.name)
        .map_err(|e| format!("Failed to check rotation: {e}"))?;

    if let Some(rotation) = rotation {
        if params.auto_rotate || exact_entry.is_none() {
            let all_entries = store
                .vault_list_entries()
                .map_err(|e| format!("Failed to list entries: {e}"))?;
            let matching_keys = collect_rotation_entries(all_entries, &rotation.prefix);

            if matching_keys.is_empty() {
                return Err(format!(
                    "No keys found for rotation prefix '{}'",
                    params.name
                ));
            }

            let selected = match rotation.rotation_strategy.as_str() {
                "round_robin" => {
                    let idx = if rotation.current_index <= 0 {
                        matching_keys.len() - 1
                    } else {
                        (rotation.current_index as usize - 1) % matching_keys.len()
                    };
                    let new_rotation = VaultKeyRotation {
                        current_index: (rotation.current_index % matching_keys.len() as i64) + 1,
                        total_keys: matching_keys.len() as i64,
                        updated_at: Utc::now().to_rfc3339(),
                        ..rotation.clone()
                    };
                    store
                        .vault_set_rotation(&new_rotation)
                        .map_err(|e| format!("Failed to update rotation: {e}"))?;
                    matching_keys.get(idx).cloned()
                }
                "random" => {
                    use rand::Rng;
                    let idx = rand::thread_rng().gen_range(0..matching_keys.len());
                    matching_keys.get(idx).cloned()
                }
                "least_recently_used" => matching_keys
                    .into_iter()
                    .min_by_key(|(_, entry)| (entry.access_count, entry.accessed_at.clone())),
                _ => matching_keys.into_iter().next(),
            }
            .ok_or_else(|| "No key selected".to_string())?;

            Ok((selected.1.name.clone(), selected.1))
        } else {
            let entry = exact_entry.ok_or_else(|| format!("Secret not found: {}", params.name))?;
            Ok((entry.name.clone(), entry))
        }
    } else {
        let entry = exact_entry.ok_or_else(|| format!("Secret not found: {}", params.name))?;
        Ok((entry.name.clone(), entry))
    }
}

pub(super) fn load_unlocked_api_key_secrets(
    server: &MemoryServer,
) -> Result<Vec<(String, String)>, String> {
    let key = get_vault_key(server)?;
    let entries = server
        .with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
        .map_err(|e| format!("Failed to list vault secrets: {e}"))?;

    let mut secrets = Vec::new();
    for entry in entries {
        if entry.secret_type != "api_key" || !entry.name.ends_with("_API_KEY") {
            continue;
        }
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }

        let decrypted = crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
        if !value.trim().is_empty() {
            secrets.push((entry.name, value));
        }
    }

    Ok(secrets)
}

fn attach_provider_refresh_warning(server: &MemoryServer, body: String) -> Result<String, String> {
    match server.refresh_llm_provider_secrets_from_vault() {
        Ok(_) => Ok(body),
        Err(err) => {
            let mut value: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| format!("serialize provider refresh warning: {e}"))?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "provider_secret_refresh_warning".to_string(),
                    json!(format!(
                        "Vault operation succeeded, but provider key cache refresh failed: {err}"
                    )),
                );
            }
            serde_json::to_string(&value).map_err(|e| format!("serialize: {e}"))
        }
    }
}

pub(super) fn read_unlocked_vault_secret(
    server: &MemoryServer,
    name: &str,
    agent_id: Option<&str>,
    auto_rotate: bool,
) -> Result<String, String> {
    let key = get_vault_key(server)?;
    let params = VaultGetParams {
        name: name.to_string(),
        agent_id: agent_id.map(str::to_string),
        auto_rotate,
    };
    let (target_name, entry) =
        server.with_global_store(|store| select_vault_entry(store, &params))?;

    ensure_agent_allowed(&entry, params.agent_id.as_deref())?;

    let decrypted = crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
    let value = String::from_utf8(decrypted)
        .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;

    server
        .with_global_store(|store| {
            store
                .vault_touch_entry(&target_name)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| format!("Failed to update access stats: {e}"))?;

    Ok(value)
}

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
        let key = crypto::derive_key(&params.password, &salt)?;
        let verifier = crypto::create_verifier(&key)?;
        let now = Utc::now().to_rfc3339();
        let config = VaultConfig {
            salt: salt_b64,
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
            cipher: "aes-256-gcm".to_string(),
            created_at: now.clone(),
            updated_at: now,
        };

        server
            .with_global_store(|store| store.vault_set_config(&config).map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to save vault config: {e}"))?;

        *write_or_recover(&server.vault_key, "vault_key") = Some(key);
        *write_or_recover(&server.vault_unlock_time, "vault_unlock_time") = Some(Instant::now());
        reset_vault_unlock_failures(server);

        serde_json::to_string(&json!({
            "initialized": true,
            "locked": false,
            "message": "Vault initialized and unlocked"
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    record_vault_audit(
        server,
        "vault_init",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result
}

pub(crate) async fn handle_vault_unlock(
    server: &MemoryServer,
    params: VaultUnlockParams,
) -> Result<String, String> {
    let result = (|| {
        ensure_vault_unlock_allowed(server)?;

        let config = server
            .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to load vault config: {e}"))?
            .ok_or_else(|| "Vault not initialized. Call vault_init first.".to_string())?;

        let salt = B64
            .decode(&config.salt)
            .map_err(|e| format!("Invalid salt in vault config: {e}"))?;
        let key = crypto::derive_key(&params.password, &salt)?;

        if !crypto::verify_password(&key, &config.verifier)? {
            return record_vault_unlock_failure(server);
        }

        *write_or_recover(&server.vault_key, "vault_key") = Some(key);
        *write_or_recover(&server.vault_unlock_time, "vault_unlock_time") = Some(Instant::now());
        reset_vault_unlock_failures(server);

        serde_json::to_string(&json!({
            "unlocked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    record_vault_audit(
        server,
        "vault_unlock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result
}

pub(crate) async fn handle_vault_lock(server: &MemoryServer) -> Result<String, String> {
    let result = {
        clear_cached_vault_state(server);
        serde_json::to_string(&json!({
            "locked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    };

    record_vault_audit(
        server,
        "vault_lock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result
}

pub(crate) async fn handle_vault_set(
    server: &MemoryServer,
    params: VaultSetParams,
) -> Result<String, String> {
    let secret_name = params.name.clone();
    let result = (|| {
        crypto::validate_secret_name(&params.name)?;
        let key = get_vault_key(server)?;

        let secret_type = match params.secret_type.to_ascii_lowercase().as_str() {
            "api_key" => "api_key",
            "oauth_token" | "oauth" => "oauth_token",
            "json_blob" | "json" => "json_blob",
            "cookie" => "cookie",
            _ => "other",
        };
        let allowed_agents = normalize_allowed_agents(params.allowed_agents.clone());
        let (encrypted_value, nonce) = crypto::encrypt(&key, params.value.as_bytes())?;

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
            .with_global_store(|store| store.vault_upsert_entry(&entry).map_err(|e| e.to_string()))
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
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    record_vault_audit(
        server,
        "vault_set",
        Some(&secret_name),
        result.is_ok(),
        match &result {
            Ok(_) => Some("stored"),
            Err(err) => Some(err.as_str()),
        },
    );
    result
}

pub(crate) async fn handle_vault_get(
    server: &MemoryServer,
    params: VaultGetParams,
) -> Result<String, String> {
    let requested_name = params.name.clone();
    let result = (|| {
        let key = get_vault_key(server)?;
        let (target_name, entry) =
            server.with_global_store(|store| select_vault_entry(store, &params))?;

        ensure_agent_allowed(&entry, params.agent_id.as_deref())?;

        let decrypted = crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted value is not valid UTF-8: {e}"))?;

        server
            .with_global_store(|store| {
                store
                    .vault_touch_entry(&target_name)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("Failed to update access stats: {e}"))?;

        serde_json::to_string(&json!({
            "name": entry.name,
            "value": value,
            "secret_type": entry.secret_type,
            "description": entry.description,
            "allowed_agents": entry.allowed_agents,
            "access_count": entry.access_count + 1,
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    record_vault_audit(
        server,
        "vault_get",
        Some(&requested_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result
}

pub(crate) async fn handle_vault_list(
    server: &MemoryServer,
    params: VaultListParams,
) -> Result<String, String> {
    if !is_vault_initialized(server)? {
        return Err("Vault not initialized. Call vault_init first.".into());
    }

    let entries = if let Some(ref secret_type) = params.secret_type {
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
        let _key = get_vault_key(server)?;
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

    record_vault_audit(
        server,
        "vault_remove",
        Some(&secret_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );

    match result {
        Ok(value) => Ok(value),
        Err(err) if err.starts_with("Secret not found: ") => serde_json::to_string(&json!({
            "removed": false,
            "error": err,
        }))
        .map_err(|e| format!("serialize: {e}")),
        Err(err) => Err(err),
    }
}

pub(crate) async fn handle_vault_status(server: &MemoryServer) -> Result<String, String> {
    let initialized = is_vault_initialized(server)?;
    let _ = maybe_auto_lock_vault(server);
    let locked = read_or_recover(&server.vault_key, "vault_key").is_none();
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
        "auto_lock_after_secs": server.vault_auto_lock_after_secs,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vault_setup_rotation(
    server: &MemoryServer,
    params: VaultSetupRotationParams,
) -> Result<String, String> {
    let _key = get_vault_key(server)?;

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
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}
