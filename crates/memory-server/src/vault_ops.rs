// vault_ops.rs — MCP tool handlers for Tachi Vault

use super::*;
use crate::vault_crypto as crypto;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use memory_core::vault::{VaultConfig, VaultEntry, VaultKeyHealth, VaultKeyRotation};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const VAULT_UNLOCK_MAX_FAILED_ATTEMPTS: u32 = 5;
const VAULT_UNLOCK_LOCKOUT_SECS: u64 = 300;

fn default_secret_type() -> String {
    "api_key".to_string()
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultInitParams {
    pub password: String,
}

impl Drop for VaultInitParams {
    fn drop(&mut self) {
        crypto::zero_string(&mut self.password);
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultUnlockParams {
    pub password: String,
}

impl Drop for VaultUnlockParams {
    fn drop(&mut self) {
        crypto::zero_string(&mut self.password);
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultSetParams {
    pub name: String,
    pub value: String,
    #[serde(default = "default_secret_type")]
    pub secret_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allowed_agents: Option<Vec<String>>,
    #[serde(default)]
    pub enable_rotation: bool,
    #[serde(default)]
    pub rotation_strategy: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultGetParams {
    pub name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub auto_rotate: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultListParams {
    #[serde(default)]
    pub secret_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultRemoveParams {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultSetupRotationParams {
    pub prefix: String,
    pub total_keys: i64,
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultSetApiKeyPoolParams {
    /// Logical provider env name, e.g. OPENAI_API_KEY or ROUTER_API_KEY.
    pub prefix: String,
    /// Concrete key values. Stored as PREFIX_1, PREFIX_2, ...
    pub values: Vec<String>,
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allowed_agents: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultLeaseApiKeyParams {
    /// Logical provider env name or standalone API key name.
    pub name: String,
    /// Optional child env var name. Defaults to `name`.
    #[serde(default)]
    pub env_name: Option<String>,
    /// Optional agent id for future restricted-secret checks.
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct VaultRecordKeyResultParams {
    /// Logical provider/env name, e.g. DEEPSEEK_API_KEY.
    pub logical_name: String,
    /// Concrete leased key id, e.g. DEEPSEEK_API_KEY_2.
    pub key_id: String,
    /// HTTP status code observed by the consumer, if available.
    #[serde(default)]
    pub status_code: Option<u16>,
    /// Outcome override: success | rate_limited | auth_failed | exhausted | error.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Retry-After seconds for 429/cooldown responses.
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
    /// Short non-secret reason or provider error class.
    #[serde(default)]
    pub reason: Option<String>,
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

fn clear_cached_vault_state_locked(v: &mut crate::VaultState) {
    let _ = v.key.take();
    v.unlock_time = None;
}

fn clear_cached_vault_state(server: &MemoryServer) {
    {
        let mut v = server.vault_write();
        clear_cached_vault_state_locked(&mut v);
    }
    server.llm.clear_provider_secrets();
}

fn maybe_auto_lock_vault(server: &MemoryServer) -> bool {
    let locked = {
        let mut v = server.vault_write();
        let expired = v.unlock_time.is_some_and(|unlock_time| {
            unlock_time.elapsed() > Duration::from_secs(v.auto_lock_after_secs)
        });
        if expired {
            clear_cached_vault_state_locked(&mut v);
        }
        expired
    };
    if locked {
        server.llm.clear_provider_secrets();
    }
    locked
}

fn record_vault_audit(
    server: &MemoryServer,
    operation: &str,
    secret_name: Option<&str>,
    success: bool,
    detail: Option<&str>,
) -> Result<(), String> {
    let timestamp = Utc::now().to_rfc3339();
    server
        .with_global_store(|store| {
            store
                .vault_insert_audit(&timestamp, operation, secret_name, success, detail)
                .map_err(|e| e.to_string())
        })
        .map_err(|err| {
            tracing::warn!("failed to record vault audit for {operation}: {err}");
            format!("failed to record vault audit for {operation}: {err}")
        })
}

fn attach_vault_audit_warning(body: String, warning: String) -> Result<String, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("serialize vault audit warning: {e}"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "vault_audit_warning".to_string(),
            json!(format!(
                "Vault operation succeeded, but its audit record was not persisted: {warning}"
            )),
        );
    }
    serde_json::to_string(&value).map_err(|e| format!("serialize: {e}"))
}

fn result_with_vault_audit_warning(
    result: Result<String, String>,
    audit_result: Result<(), String>,
) -> Result<String, String> {
    match (result, audit_result) {
        (Ok(body), Err(warning)) => attach_vault_audit_warning(body, warning),
        (result, _) => result,
    }
}

/// Check if vault is unlocked and run work with a borrowed cached key.
fn with_vault_key<T>(
    server: &MemoryServer,
    f: impl FnOnce(&[u8; 32]) -> Result<T, String>,
) -> Result<T, String> {
    loop {
        let key_bytes = {
            let v = server.vault_read();
            let Some(unlock_time) = v.unlock_time else {
                return Err("Vault is locked. Call vault_unlock first.".to_string());
            };

            if unlock_time.elapsed() <= Duration::from_secs(v.auto_lock_after_secs) {
                let key = v
                    .key
                    .as_ref()
                    .ok_or_else(|| "Vault is locked. Call vault_unlock first.".to_string())?;
                Some(*key.bytes())
            } else {
                None
            }
        };

        if let Some(key_bytes) = key_bytes {
            return f(&key_bytes);
        }

        let auto_locked = {
            let mut v = server.vault_write();
            let expired = v.unlock_time.is_some_and(|unlock_time| {
                unlock_time.elapsed() > Duration::from_secs(v.auto_lock_after_secs)
            });
            if expired {
                clear_cached_vault_state_locked(&mut v);
            }
            expired
        };

        if auto_locked {
            server.llm.clear_provider_secrets();
            return Err("Vault auto-locked. Call vault_unlock first.".into());
        }
    }
}

#[cfg(test)]
mod vault_key_tests {
    use super::*;

    #[tokio::test]
    async fn with_vault_key_drops_vault_lock_before_running_work() {
        let db_path = std::env::temp_dir().join(format!(
            "memory-server-vault-lock-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = MemoryServer::new(db_path, None).expect("create test server");
        let key = [9u8; 32];
        {
            let mut v = server.vault_write();
            v.key = Some(crate::CachedVaultKey::copy_from(&key));
            v.unlock_time = Some(Instant::now());
        }

        with_vault_key(&server, |cached_key| {
            assert_eq!(cached_key, &key);
            assert!(
                server.vault.try_write().is_ok(),
                "vault lock should not be held while user work runs"
            );
            Ok(())
        })
        .expect("vault key should be available");
    }
}

fn ensure_vault_unlocked(server: &MemoryServer) -> Result<(), String> {
    with_vault_key(server, |_| Ok(()))
}

/// Check if vault is initialized.
fn is_vault_initialized(server: &MemoryServer) -> Result<bool, String> {
    server
        .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
        .map(|opt| opt.is_some())
}

fn ensure_vault_unlock_allowed(server: &MemoryServer) -> Result<(), String> {
    let mut v = server.vault_write();
    if let Some(until) = v.failed_attempts.1 {
        if Instant::now() < until {
            return Err(format!(
                "Vault unlock temporarily locked. Try again in {} seconds.",
                remaining_lockout_seconds(until)
            ));
        }
        v.failed_attempts = (0, None);
    }
    Ok(())
}

fn record_vault_unlock_failure(server: &MemoryServer) -> Result<String, String> {
    let mut v = server.vault_write();
    v.failed_attempts.0 = v.failed_attempts.0.saturating_add(1);
    if v.failed_attempts.0 >= VAULT_UNLOCK_MAX_FAILED_ATTEMPTS {
        let until = Instant::now() + Duration::from_secs(VAULT_UNLOCK_LOCKOUT_SECS);
        v.failed_attempts.1 = Some(until);
        return Err(format!(
            "Too many failed vault unlock attempts. Try again in {} seconds.",
            remaining_lockout_seconds(until)
        ));
    }
    Err("Wrong password".to_string())
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

fn load_unlocked_vault_secrets(
    server: &MemoryServer,
    include_entry: impl Fn(&VaultEntry) -> bool,
) -> Result<Vec<(String, String)>, String> {
    with_vault_key(server, |key| {
        let entries = server
            .with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to list vault secrets: {e}"))?;

        let mut secrets = Vec::new();
        for entry in entries {
            if !include_entry(&entry) {
                continue;
            }
            if entry
                .allowed_agents
                .as_ref()
                .is_some_and(|agents| !agents.is_empty())
            {
                continue;
            }

            let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
            let value = String::from_utf8(decrypted)
                .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
            if !value.trim().is_empty() {
                secrets.push((entry.name, value));
            }
        }

        Ok(secrets)
    })
}

pub(super) fn load_unlocked_api_key_secret_pools(
    server: &MemoryServer,
) -> Result<HashMap<String, Vec<crate::llm::ProviderSecret>>, String> {
    with_vault_key(server, |key| {
        let (entries, rotations, key_health_rows) = server
            .with_global_store(|store| {
                let entries = store.vault_list_entries().map_err(|e| e.to_string())?;
                let rotations = store.vault_list_rotations().map_err(|e| e.to_string())?;
                let key_health = store
                    .vault_list_key_health(None)
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((entries, rotations, key_health))
            })
            .map_err(|e| format!("Failed to list vault provider secrets: {e}"))?;

        let now = Utc::now();
        let mut key_health_by_logical: HashMap<String, HashMap<String, VaultKeyHealth>> =
            HashMap::new();
        for row in key_health_rows {
            key_health_by_logical
                .entry(row.logical_name.clone())
                .or_default()
                .insert(row.key_id.clone(), row);
        }

        let mut pools: HashMap<String, Vec<crate::llm::ProviderSecret>> = HashMap::new();
        let mut rotation_members: HashSet<String> = HashSet::new();

        let is_unusable =
            |logical_name: &str, key_id: &str, now: &chrono::DateTime<chrono::Utc>| {
                let Some(logical_health) = key_health_by_logical.get(logical_name) else {
                    return false;
                };
                let Some(health) = logical_health.get(key_id) else {
                    return false;
                };
                if health.disabled || health.auth_failed {
                    return true;
                }
                match health.status.as_str() {
                    "exhausted" => true,
                    "rate_limited" | "cooldown" => health
                        .cooldown_until
                        .as_deref()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .is_some_and(|until| until.with_timezone(&Utc) > *now),
                    _ => false,
                }
            };

        for rotation in rotations {
            let mut matching = collect_rotation_entries(entries.clone(), &rotation.prefix);
            if matching.is_empty() {
                continue;
            }
            let selected_idx = match rotation.rotation_strategy.as_str() {
                "round_robin" => {
                    if rotation.current_index <= 0 {
                        0
                    } else {
                        (rotation.current_index as usize - 1) % matching.len()
                    }
                }
                "random" => {
                    use rand::Rng;
                    rand::thread_rng().gen_range(0..matching.len())
                }
                "least_recently_used" => matching
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (_, entry))| (entry.access_count, entry.accessed_at.clone()))
                    .map(|(idx, _)| idx)
                    .unwrap_or(0),
                _ => 0,
            };
            matching.rotate_left(selected_idx);
            let mut pool = Vec::new();
            for (_, entry) in matching {
                if entry.secret_type != "api_key"
                    || entry
                        .allowed_agents
                        .as_ref()
                        .is_some_and(|agents| !agents.is_empty())
                    || is_unusable(&rotation.prefix, &entry.name, &now)
                {
                    continue;
                }
                let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
                let value = String::from_utf8(decrypted).map_err(|e| {
                    format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name)
                })?;
                if value.trim().is_empty() {
                    continue;
                }
                rotation_members.insert(entry.name.clone());
                pool.push(crate::llm::ProviderSecret {
                    key_id: entry.name,
                    value,
                });
            }
            if !pool.is_empty() {
                pools.insert(rotation.prefix, pool);
            }
        }

        for entry in entries {
            if entry.secret_type != "api_key"
                || !entry.name.ends_with("_API_KEY")
                || rotation_members.contains(&entry.name)
                || entry
                    .allowed_agents
                    .as_ref()
                    .is_some_and(|agents| !agents.is_empty())
                || is_unusable(&entry.name, &entry.name, &now)
            {
                continue;
            }
            let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
            let value = String::from_utf8(decrypted)
                .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
            if !value.trim().is_empty() {
                pools.entry(entry.name.clone()).or_insert_with(|| {
                    vec![crate::llm::ProviderSecret {
                        key_id: entry.name,
                        value,
                    }]
                });
            }
        }

        Ok(pools)
    })
}

pub(super) fn load_unlocked_env_secrets(
    server: &MemoryServer,
) -> Result<Vec<(String, String)>, String> {
    let pools = load_unlocked_api_key_secret_pools(server)?;
    let rotation_member_names: HashSet<String> = pools
        .values()
        .flat_map(|entries| entries.iter().map(|entry| entry.key_id.clone()))
        .collect();
    let mut secrets = load_unlocked_vault_secrets(server, |entry| {
        is_shell_env_name(&entry.name) && !rotation_member_names.contains(&entry.name)
    })?;
    for (logical_name, entries) in pools {
        if !is_shell_env_name(&logical_name) {
            continue;
        }
        if let Some(entry) = entries.first() {
            upsert_env_secret(&mut secrets, logical_name, entry.value.clone());
        }
    }
    Ok(secrets)
}

fn load_unlocked_provider_env_secrets(
    server: &MemoryServer,
) -> Result<Vec<(String, String)>, String> {
    let provider_keys = crate::provider_config::provider_env_keys();
    let pools = load_unlocked_api_key_secret_pools(server)?;
    let mut secrets = Vec::new();
    for (logical_name, entries) in pools {
        if !provider_keys.contains(&logical_name) || !is_shell_env_name(&logical_name) {
            continue;
        }
        if let Some(entry) = entries.first() {
            upsert_env_secret(&mut secrets, logical_name, entry.value.clone());
        }
    }
    Ok(secrets)
}

pub(super) fn load_unlocked_env_secrets_for_child_env(
    server: &MemoryServer,
    cwd: Option<&Path>,
) -> Result<Vec<(String, String)>, String> {
    let child_env_mode = std::env::var("TACHI_VAULT_CHILD_ENV")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "project".to_string());
    let include_all_env_secrets = matches!(
        child_env_mode.as_str(),
        "all"
            | "full"
            | "legacy"
            | "legacy_all"
            | "override"
            | "fill_missing"
            | "missing_only"
            | "preserve_env"
    );

    let mut secrets = if include_all_env_secrets {
        load_unlocked_env_secrets(server)?
    } else {
        load_unlocked_provider_env_secrets(server)?
    };
    let Some(cwd) = cwd else {
        return Ok(secrets);
    };
    let Some(bindings_path) = find_project_vault_env_file(cwd) else {
        return Ok(secrets);
    };

    let contents = std::fs::read_to_string(&bindings_path)
        .map_err(|e| format!("Failed to read {}: {e}", bindings_path.display()))?;
    for (env_name, secret_name) in parse_project_vault_env_bindings(&contents) {
        let value = match read_unlocked_vault_secret(server, &secret_name, None, false) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    "[vault] skipped project env binding {}={} from {}: {}",
                    env_name,
                    crate::provider_config::vault_alias_line(&secret_name),
                    bindings_path.display(),
                    err
                );
                continue;
            }
        };
        upsert_env_secret(&mut secrets, env_name, value);
    }

    Ok(secrets)
}

fn find_project_vault_env_file(cwd: &Path) -> Option<PathBuf> {
    let start = if cwd.is_file() {
        cwd.parent().unwrap_or(cwd)
    } else {
        cwd
    };

    for dir in start.ancestors() {
        for rel_path in [".tachi/vault.env", ".tachi/vault-bindings.env"] {
            let candidate = dir.join(rel_path);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn parse_project_vault_env_bindings(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            if !is_shell_env_name(name) {
                return None;
            }
            crate::provider_config::parse_vault_alias(value)
                .map(|secret_name| (name.to_string(), secret_name.to_string()))
        })
        .collect()
}

fn upsert_env_secret(secrets: &mut Vec<(String, String)>, name: String, value: String) {
    if let Some((_, existing_value)) = secrets
        .iter_mut()
        .find(|(existing_name, _)| existing_name == &name)
    {
        *existing_value = value;
    } else {
        secrets.push((name, value));
    }
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

pub(crate) fn read_unlocked_vault_secret(
    server: &MemoryServer,
    name: &str,
    agent_id: Option<&str>,
    auto_rotate: bool,
) -> Result<String, String> {
    with_vault_key(server, |key| {
        let params = VaultGetParams {
            name: name.to_string(),
            agent_id: agent_id.map(str::to_string),
            auto_rotate,
        };
        let (target_name, entry) =
            server.with_global_store(|store| select_vault_entry(store, &params))?;

        ensure_agent_allowed(&entry, params.agent_id.as_deref())?;

        let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
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
    })
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
        let key = crypto::DerivedVaultKey::derive(&params.password, &salt)?;
        let verifier = crypto::create_verifier(key.bytes())?;
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
    let result = (|| {
        ensure_vault_unlock_allowed(server)?;

        let config = server
            .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to load vault config: {e}"))?
            .ok_or_else(|| "Vault not initialized. Call vault_init first.".to_string())?;

        let salt = B64
            .decode(&config.salt)
            .map_err(|e| format!("Invalid salt in vault config: {e}"))?;
        let key = crypto::DerivedVaultKey::derive(&params.password, &salt)?;

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
    })();

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
            let secret_type = match params.secret_type.to_ascii_lowercase().as_str() {
                "api_key" => "api_key",
                "oauth_token" | "oauth" => "oauth_token",
                "json_blob" | "json" => "json_blob",
                "cookie" => "cookie",
                _ => "other",
            };
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
        let (target_name, entry) =
            server.with_global_store(|store| select_vault_entry(store, &params))?;

        ensure_agent_allowed(&entry, params.agent_id.as_deref())?;

        let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted value is not valid UTF-8: {e}"))?;

        let new_access_count = server
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
    let _ = maybe_auto_lock_vault(server);
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
        if !is_shell_env_name(&params.prefix) {
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
                    secret_type: "api_key".to_string(),
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
    let result = (|| {
        let env_name = params
            .env_name
            .clone()
            .unwrap_or_else(|| params.name.clone());
        if !is_shell_env_name(&env_name) {
            return Err(format!(
                "env_name '{}' must be a valid shell env name",
                env_name
            ));
        }

        let pools = load_unlocked_api_key_secret_pools(server)?;
        let selected = pools
            .get(&params.name)
            .and_then(|entries| entries.first())
            .ok_or_else(|| {
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

    let audit_result = record_vault_audit(
        server,
        "vault_lease_api_key",
        Some(&requested_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
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
        "health": health,
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
