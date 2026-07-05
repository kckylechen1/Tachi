use crate::server_state::MemoryServer;
use crate::vault_crypto as crypto;
use chrono::Utc;
use memory_core::vault::{
    api_key_pool_member_index, VaultEntry, VaultKeyHealth, VaultKeyRotation, SECRET_TYPE_API_KEY,
};
use memory_core::MemoryStore;
use std::collections::{HashMap, HashSet};

use super::params::VaultGetParams;
use super::rotation::collect_rotation_entries;
use super::session::{ensure_vault_unlocked, with_vault_key};

pub(super) fn ensure_agent_allowed(
    entry: &VaultEntry,
    agent_id: Option<&str>,
) -> Result<(), String> {
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

/// The single authorization gate every Vault mutation must pass before writing.
/// Order: unlock gate -> resolve existing entry -> agent ACL gate.
pub(super) fn authorize_vault_mutation(
    server: &MemoryServer,
    target_name: &str,
    agent_id: Option<&str>,
) -> Result<(), String> {
    ensure_vault_unlocked(server)?;
    let existing = server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(target_name)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| format!("Failed to resolve secret for authorization: {e}"))?;
    if let Some(entry) = existing {
        ensure_agent_allowed(&entry, agent_id)?;
    }
    Ok(())
}

/// Authorize overwriting an API-key pool by checking every existing pool member.
/// Membership is resolved with `api_key_pool_member_index` — the SAME predicate
/// `vault_replace_api_key_pool` uses to decide which entries it overwrites/deletes —
/// so the gate covers exactly what the write clobbers. (Using the rotation-side
/// `collect_rotation_entries` here instead would parse suffixes as `u32` and let a
/// restricted member with a > u32::MAX suffix escape the gate while still being
/// clobbered by the `usize`-based write path: a fail-open.)
pub(super) fn authorize_vault_pool_mutation(
    server: &MemoryServer,
    prefix: &str,
    agent_id: Option<&str>,
) -> Result<(), String> {
    ensure_vault_unlocked(server)?;
    let entries = server
        .with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
        .map_err(|e| format!("Failed to list entries for authorization: {e}"))?;
    for entry in entries {
        if api_key_pool_member_index(&entry.name, prefix).is_some() {
            ensure_agent_allowed(&entry, agent_id)?;
        }
    }
    Ok(())
}

pub(super) struct SelectedVaultEntry {
    pub(super) target_name: String,
    pub(super) entry: VaultEntry,
    pub(super) pending_rotation: Option<VaultKeyRotation>,
}

pub(super) fn select_vault_entry(
    store: &mut MemoryStore,
    params: &VaultGetParams,
) -> Result<SelectedVaultEntry, String> {
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

            let (selected, pending_rotation) = match rotation.rotation_strategy.as_str() {
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
                    (matching_keys.get(idx).cloned(), Some(new_rotation))
                }
                "random" => {
                    use rand::Rng;
                    let idx = rand::thread_rng().gen_range(0..matching_keys.len());
                    (matching_keys.get(idx).cloned(), None)
                }
                "least_recently_used" => (
                    matching_keys
                        .into_iter()
                        .min_by_key(|(_, entry)| (entry.access_count, entry.accessed_at.clone())),
                    None,
                ),
                _ => (matching_keys.into_iter().next(), None),
            };
            let selected = selected.ok_or_else(|| "No key selected".to_string())?;

            Ok(SelectedVaultEntry {
                target_name: selected.1.name.clone(),
                entry: selected.1,
                pending_rotation,
            })
        } else {
            let entry = exact_entry.ok_or_else(|| format!("Secret not found: {}", params.name))?;
            Ok(SelectedVaultEntry {
                target_name: entry.name.clone(),
                entry,
                pending_rotation: None,
            })
        }
    } else {
        let entry = exact_entry.ok_or_else(|| format!("Secret not found: {}", params.name))?;
        Ok(SelectedVaultEntry {
            target_name: entry.name.clone(),
            entry,
            pending_rotation: None,
        })
    }
}

pub(super) fn record_successful_vault_access(
    store: &mut MemoryStore,
    target_name: &str,
    pending_rotation: Option<&VaultKeyRotation>,
) -> Result<i64, String> {
    if let Some(rotation) = pending_rotation {
        store
            .vault_set_rotation(rotation)
            .map_err(|e| format!("Failed to update rotation: {e}"))?;
    }
    store
        .vault_touch_entry(target_name)
        .map_err(|e| e.to_string())
}

pub(super) fn load_unlocked_vault_secrets(
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

pub(crate) fn load_unlocked_api_key_secret_pools(
    server: &MemoryServer,
) -> Result<HashMap<String, Vec<tachi_llm::ProviderSecret>>, String> {
    load_unlocked_api_key_secret_pools_filtered(server, None)
}

pub(super) fn load_unlocked_api_key_secret_pool(
    server: &MemoryServer,
    logical_name: &str,
) -> Result<Vec<tachi_llm::ProviderSecret>, String> {
    load_unlocked_api_key_secret_pools_filtered(server, Some(logical_name))
        .map(|mut pools| pools.remove(logical_name).unwrap_or_default())
}

fn load_unlocked_api_key_secret_pools_filtered(
    server: &MemoryServer,
    only_logical_name: Option<&str>,
) -> Result<HashMap<String, Vec<tachi_llm::ProviderSecret>>, String> {
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

        // Merge in-memory health so that runtime mutations are visible even when
        // background persistence is disabled (e.g. in tests).
        for (logical_name, members) in server.llm.provider_health_memory_snapshot() {
            let target = key_health_by_logical.entry(logical_name).or_default();
            for (key_id, health) in members {
                let keep_in_memory = target
                    .get(&key_id)
                    .and_then(|db_row| {
                        let db_updated = chrono::DateTime::parse_from_rfc3339(&db_row.updated_at)
                            .ok()?
                            .with_timezone(&Utc);
                        let mem_updated = chrono::DateTime::parse_from_rfc3339(&health.updated_at)
                            .ok()?
                            .with_timezone(&Utc);
                        Some(mem_updated >= db_updated)
                    })
                    .unwrap_or(true);
                if keep_in_memory {
                    target.insert(key_id, health);
                }
            }
        }

        let mut pools: HashMap<String, Vec<tachi_llm::ProviderSecret>> = HashMap::new();
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
            if only_logical_name.is_some_and(|logical_name| logical_name != rotation.prefix) {
                continue;
            }
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
                if entry.secret_type != SECRET_TYPE_API_KEY
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
                pool.push(tachi_llm::ProviderSecret {
                    key_id: entry.name,
                    value,
                });
            }
            if !pool.is_empty() {
                pools.insert(rotation.prefix, pool);
            }
        }

        for entry in entries {
            if only_logical_name.is_some_and(|logical_name| logical_name != entry.name) {
                continue;
            }
            if entry.secret_type != SECRET_TYPE_API_KEY
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
                    vec![tachi_llm::ProviderSecret {
                        key_id: entry.name,
                        value,
                    }]
                });
            }
        }

        Ok(pools)
    })
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
        let selected = server.with_global_store(|store| select_vault_entry(store, &params))?;

        ensure_agent_allowed(&selected.entry, params.agent_id.as_deref())?;

        let decrypted =
            crypto::decrypt(key, &selected.entry.encrypted_value, &selected.entry.nonce)?;
        let value = String::from_utf8(decrypted).map_err(|e| {
            format!(
                "Vault secret '{}' is not valid UTF-8: {e}",
                selected.entry.name
            )
        })?;

        server
            .with_global_store(|store| {
                record_successful_vault_access(
                    store,
                    &selected.target_name,
                    selected.pending_rotation.as_ref(),
                )
            })
            .map_err(|e| format!("Failed to update access stats: {e}"))?;

        Ok(value)
    })
}
