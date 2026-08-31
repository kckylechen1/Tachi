use crate::server_state::MemoryServer;
use crate::vault_crypto as crypto;
use chrono::Utc;
use memcore::vault::{
    api_key_pool_member_index, VaultEntry, VaultKeyHealth, VaultKeyRotation, SECRET_TYPE_API_KEY,
};
use memcore::MemoryStore;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use tachi_llm::AliasSkipClass;

use super::alias_integrity::unusable_skip_class;
use super::params::VaultGetParams;
use super::rotation::collect_rotation_entries;
use super::session::{ensure_vault_unlocked, with_vault_key, with_vault_key_for_provider_refresh};

#[derive(Debug)]
pub(super) enum VaultOpsError {
    VaultLocked,
    AgentRequired,
    AgentDenied,
    AgentIdentityMismatch(String),
    Internal(String),
}

impl std::fmt::Display for VaultOpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VaultLocked => write!(f, "Vault is locked"),
            Self::AgentRequired => write!(
                f,
                "Access denied: agent_id is required for this restricted secret"
            ),
            Self::AgentDenied => write!(
                f,
                "Access denied: agent is not in the allowed list for this secret"
            ),
            Self::AgentIdentityMismatch(msg) => write!(f, "{msg}"),
            Self::Internal(msg) => write!(f, "Vault error: {msg}"),
        }
    }
}

impl std::error::Error for VaultOpsError {}

fn normalize_agent_id(agent_id: Option<&str>) -> Option<String> {
    agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn resolve_vault_acl_agent_id(
    server: &MemoryServer,
    caller_agent_id: Option<&str>,
) -> Result<Option<String>, String> {
    let caller_agent_id = normalize_agent_id(caller_agent_id);
    let Some(bound_agent_id) = server.bound_agent_id() else {
        return Ok(caller_agent_id);
    };

    if caller_agent_id
        .as_deref()
        .is_some_and(|caller| caller != bound_agent_id)
    {
        return Err(
            "Access denied: caller agent_id does not match server-bound TACHI_AGENT_ID."
                .to_string(),
        );
    }

    Ok(Some(bound_agent_id))
}

pub(super) fn ensure_agent_allowed(
    entry: &VaultEntry,
    agent_id: Option<&str>,
) -> Result<(), VaultOpsError> {
    let Some(allowed_agents) = entry.allowed_agents.as_ref() else {
        return Ok(());
    };

    let Some(agent_id) = agent_id.map(str::trim).filter(|agent| !agent.is_empty()) else {
        return Err(VaultOpsError::AgentRequired);
    };

    if allowed_agents.iter().any(|allowed| allowed == agent_id) {
        Ok(())
    } else {
        Err(VaultOpsError::AgentDenied)
    }
}

/// The single authorization gate every Vault mutation must pass before writing.
/// Order: unlock gate -> resolve existing entry -> agent ACL gate.
pub(super) fn authorize_vault_mutation(
    server: &MemoryServer,
    target_name: &str,
    caller_agent_id: Option<&str>,
) -> Result<(), VaultOpsError> {
    ensure_vault_unlocked(server).map_err(|_| VaultOpsError::VaultLocked)?;
    let effective_agent_id = resolve_vault_acl_agent_id(server, caller_agent_id)
        .map_err(VaultOpsError::AgentIdentityMismatch)?;
    let existing = server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(target_name)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| VaultOpsError::Internal(format!("Failed to resolve secret: {e}")))?;
    if let Some(entry) = existing {
        ensure_agent_allowed(&entry, effective_agent_id.as_deref())?;
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
    caller_agent_id: Option<&str>,
) -> Result<(), VaultOpsError> {
    ensure_vault_unlocked(server).map_err(|_| VaultOpsError::VaultLocked)?;
    let effective_agent_id = resolve_vault_acl_agent_id(server, caller_agent_id)
        .map_err(VaultOpsError::AgentIdentityMismatch)?;
    let entries = server
        .with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
        .map_err(|e| VaultOpsError::Internal(format!("Failed to list entries: {e}")))?;
    for entry in &entries {
        if api_key_pool_member_index(&entry.name, prefix).is_some() {
            ensure_agent_allowed(&entry, effective_agent_id.as_deref())?;
        }
    }
    Ok(())
}

pub(super) struct SelectedVaultEntry {
    pub(super) target_name: String,
    pub(super) entry: VaultEntry,
    pub(super) pending_rotation: Option<VaultKeyRotation>,
}

fn select_vault_entry_from_transaction(
    store: &memcore::store::vault::VaultTransaction<'_>,
    params: &VaultGetParams,
) -> Result<SelectedVaultEntry, String> {
    let exact_entry = store
        .vault_get_entry(&params.name)
        .map_err(|e| format!("Failed to get secret: {e}"))?;
    if let Some((prefix, _)) = crate::provider_config::parse_rotation_member_name(&params.name) {
        if let Some(member_rotation) = store
            .vault_get_rotation(prefix)
            .map_err(|e| format!("Failed to check member rotation: {e}"))?
        {
            let all_entries = store
                .vault_list_entries()
                .map_err(|e| format!("Failed to list member rotation: {e}"))?;
            memcore::validate_api_key_rotation(&all_entries, &member_rotation)
                .map_err(|error| format!("{error}; refusing configured-member Vault get"))?;
        }
    }
    let rotation = store
        .vault_get_rotation(&params.name)
        .map_err(|e| format!("Failed to check rotation: {e}"))?;

    if super::is_lane_slot_secret_name(&params.name) {
        let entry = exact_entry.ok_or_else(|| format!("Secret not found: {}", params.name))?;
        return Ok(SelectedVaultEntry {
            target_name: entry.name.clone(),
            entry,
            pending_rotation: None,
        });
    }

    if let Some(rotation) = rotation {
        if params.auto_rotate || exact_entry.is_none() {
            let all_entries = store
                .vault_list_entries()
                .map_err(|e| format!("Failed to list entries: {e}"))?;
            memcore::validate_api_key_rotation(&all_entries, &rotation)
                .map_err(|error| format!("{error}; refusing rotated Vault get"))?;
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

fn record_successful_vault_access_in_transaction(
    transaction: &memcore::store::vault::VaultTransaction<'_>,
    target_name: &str,
    pending_rotation: Option<&VaultKeyRotation>,
) -> Result<i64, String> {
    if let Some(rotation) = pending_rotation {
        let current = transaction
            .vault_get_rotation(&rotation.prefix)
            .map_err(|e| format!("Failed to read current rotation: {e}"))?
            .ok_or_else(|| format!("Vault rotation '{}' disappeared", rotation.prefix))?;
        let entries = transaction
            .vault_list_entries()
            .map_err(|e| format!("Failed to list current rotation members: {e}"))?;
        memcore::validate_api_key_rotation(&entries, &current)
            .map_err(|error| format!("{error}; refusing rotation advance"))?;
        let member_index = memcore::api_key_pool_member_index(target_name, &current.prefix)
            .ok_or_else(|| {
                format!(
                    "Vault entry '{target_name}' is not a member of rotation '{}'",
                    current.prefix
                )
            })?;
        let mut updated = current;
        updated.current_index = (member_index as i64 % updated.total_keys) + 1;
        updated.updated_at = Utc::now().to_rfc3339();
        transaction
            .vault_set_rotation(&updated)
            .map_err(|e| format!("Failed to update rotation: {e}"))?;
    }
    transaction
        .vault_touch_entry(target_name)
        .map_err(|e| e.to_string())
}

fn select_authorized_vault_entry_and_record_access_with_hook(
    store: &mut MemoryStore,
    params: &VaultGetParams,
    effective_agent_id: Option<&str>,
    key: &[u8; 32],
    after_select: impl FnOnce(),
) -> Result<(SelectedVaultEntry, String, i64), String> {
    let transaction = store
        .begin_vault_transaction()
        .map_err(|e| format!("Failed to begin authorized Vault read transaction: {e}"))?;
    let selected = select_vault_entry_from_transaction(&transaction, params)?;
    after_select();
    ensure_agent_allowed(&selected.entry, effective_agent_id).map_err(|e| e.to_string())?;
    let decrypted = crypto::decrypt(key, &selected.entry.encrypted_value, &selected.entry.nonce)?;
    let value = crypto::decode_utf8_zeroizing(
        decrypted,
        format!("Vault secret '{}' is not valid UTF-8", selected.entry.name),
    )?;
    let access_count = record_successful_vault_access_in_transaction(
        &transaction,
        &selected.target_name,
        selected.pending_rotation.as_ref(),
    )?;
    transaction
        .commit()
        .map_err(|e| format!("Failed to commit authorized Vault read transaction: {e}"))?;
    Ok((selected, value, access_count))
}

pub(super) fn select_authorized_vault_entry_and_record_access(
    store: &mut MemoryStore,
    params: &VaultGetParams,
    effective_agent_id: Option<&str>,
    key: &[u8; 32],
) -> Result<(SelectedVaultEntry, String, i64), String> {
    select_authorized_vault_entry_and_record_access_with_hook(
        store,
        params,
        effective_agent_id,
        key,
        || {},
    )
}

fn read_usable_vault_secret_from_store(
    store: &mut MemoryStore,
    key: &[u8; 32],
    params: &VaultGetParams,
    effective_agent_id: Option<&str>,
) -> Result<String, String> {
    let transaction = store
        .begin_vault_transaction()
        .map_err(|e| format!("Failed to begin usable Vault read transaction: {e}"))?;
    let selected = select_vault_entry_from_transaction(&transaction, params)?;
    ensure_agent_allowed(&selected.entry, effective_agent_id).map_err(|e| e.to_string())?;
    let decrypted = crypto::decrypt(key, &selected.entry.encrypted_value, &selected.entry.nonce)?;
    let value = crypto::decode_utf8_zeroizing(
        decrypted,
        format!("Vault secret '{}' is not valid UTF-8", selected.entry.name),
    )?;

    let (touch_name, value) = if super::is_lane_slot_secret_name(&selected.entry.name) {
        let target = tachi_llm::parse_vault_alias(&value).ok_or_else(|| {
            format!(
                "Lane slot '{}' is not bound to a usable account",
                selected.entry.name
            )
        })?;
        if super::is_lane_slot_secret_name(target) {
            return Err(format!(
                "Lane slot '{}' cannot resolve through another slot '{target}'",
                selected.entry.name
            ));
        }
        let target_entry = transaction
            .vault_get_entry(target)
            .map_err(|e| format!("Failed to read lane slot target: {e}"))?
            .ok_or_else(|| format!("Lane slot target '{target}' is missing"))?;
        ensure_agent_allowed(&target_entry, effective_agent_id).map_err(|e| e.to_string())?;
        if memcore::effective_vault_secret_type(&target_entry.name, &target_entry.secret_type)
            != SECRET_TYPE_API_KEY
        {
            return Err(format!(
                "Lane slot '{}' points at '{}' which is not an API key",
                selected.entry.name, target_entry.name
            ));
        }
        let decrypted = crypto::decrypt(key, &target_entry.encrypted_value, &target_entry.nonce)?;
        let target_value = crypto::decode_utf8_zeroizing(
            decrypted,
            format!("Vault secret '{}' is not valid UTF-8", target_entry.name),
        )?;
        let slot_health = transaction
            .vault_get_key_health(&selected.entry.name, &target_entry.name)
            .map_err(|e| format!("Failed to read lane slot health: {e}"))?;
        let target_health = transaction
            .vault_get_key_health(&target_entry.name, &target_entry.name)
            .map_err(|e| format!("Failed to read target account health: {e}"))?;
        super::account_bind::refuse_unusable_account_target(
            &selected.entry.name,
            &target_entry,
            &target_value,
            effective_agent_id,
            super::account_bind::slot_target_health_unusable(
                slot_health.as_ref(),
                target_health.as_ref(),
            ),
        )?;
        (target_entry.name, target_value)
    } else {
        (selected.target_name.clone(), value)
    };

    record_successful_vault_access_in_transaction(
        &transaction,
        &touch_name,
        selected.pending_rotation.as_ref(),
    )?;
    transaction
        .commit()
        .map_err(|e| format!("Failed to commit usable Vault read transaction: {e}"))?;
    Ok(value)
}

pub(crate) fn read_usable_vault_secret_from_store_direct(
    store: &mut MemoryStore,
    key: &[u8; 32],
    name: &str,
    auto_rotate: bool,
) -> Result<String, String> {
    let params = VaultGetParams {
        name: name.to_string(),
        agent_id: None,
        auto_rotate,
    };
    read_usable_vault_secret_from_store(store, key, &params, None)
}

/// Materialize unrestricted entries for an identity-less CLI consumer in one
/// transaction. Rotation validation, ACL filtering, decrypt, and access
/// accounting all observe the same database state.
pub(crate) fn materialize_unrestricted_vault_entries_from_store(
    store: &mut MemoryStore,
    key: &[u8; 32],
    include_entry: impl Fn(&VaultEntry) -> bool,
) -> Result<Vec<(String, String)>, String> {
    materialize_unrestricted_vault_entries_from_store_with_hook(store, key, include_entry, || {})
}

fn materialize_unrestricted_vault_entries_from_store_with_hook(
    store: &mut MemoryStore,
    key: &[u8; 32],
    include_entry: impl Fn(&VaultEntry) -> bool,
    after_snapshot: impl FnOnce(),
) -> Result<Vec<(String, String)>, String> {
    let transaction = store
        .begin_vault_transaction()
        .map_err(|e| format!("Failed to begin Vault materialization transaction: {e}"))?;
    let entries = transaction
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault secrets: {e}"))?;
    for rotation in transaction
        .vault_list_rotations()
        .map_err(|e| format!("Failed to list Vault rotations: {e}"))?
    {
        memcore::validate_api_key_rotation(&entries, &rotation)
            .map_err(|error| format!("{error}; refusing Vault materialization"))?;
    }
    after_snapshot();

    let mut secrets = Vec::new();
    for entry in &entries {
        if !include_entry(&entry)
            || entry
                .allowed_agents
                .as_ref()
                .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }
        let decrypted = match crypto::decrypt(key, &entry.encrypted_value, &entry.nonce) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("WARNING: failed to decrypt '{}': {}", entry.name, error);
                continue;
            }
        };
        let value = match crypto::decode_utf8_zeroizing(
            decrypted,
            format!("Vault secret '{}' is not valid UTF-8", entry.name),
        ) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("WARNING: failed to decrypt '{}': {}", entry.name, error);
                continue;
            }
        };
        if value.trim().is_empty() {
            eprintln!(
                "WARNING: failed to decrypt '{}': secret is empty",
                entry.name
            );
            continue;
        }
        let (touched_name, value) = if let Some(target) = tachi_llm::parse_vault_alias(&value) {
            if !super::is_lane_slot_secret_name(&entry.name) {
                continue;
            }
            let Some(target_entry) = entries.iter().find(|candidate| candidate.name == target)
            else {
                continue;
            };
            if super::is_lane_slot_secret_name(&target_entry.name)
                || memcore::effective_vault_secret_type(
                    &target_entry.name,
                    &target_entry.secret_type,
                ) != SECRET_TYPE_API_KEY
                || target_entry
                    .allowed_agents
                    .as_ref()
                    .is_some_and(|agents| !agents.is_empty())
            {
                continue;
            }
            let decrypted =
                crypto::decrypt(key, &target_entry.encrypted_value, &target_entry.nonce)?;
            let target_value = crypto::decode_utf8_zeroizing(
                decrypted,
                format!("Vault secret '{}' is not valid UTF-8", target_entry.name),
            )?;
            let slot_health = transaction
                .vault_get_key_health(&entry.name, &target_entry.name)
                .map_err(|e| format!("Failed to read lane slot health: {e}"))?;
            let target_health = transaction
                .vault_get_key_health(&target_entry.name, &target_entry.name)
                .map_err(|e| format!("Failed to read target account health: {e}"))?;
            if super::account_bind::refuse_unusable_account_target(
                &entry.name,
                target_entry,
                &target_value,
                None,
                super::account_bind::slot_target_health_unusable(
                    slot_health.as_ref(),
                    target_health.as_ref(),
                ),
            )
            .is_err()
            {
                continue;
            }
            (target_entry.name.clone(), target_value)
        } else {
            if super::is_lane_slot_secret_name(&entry.name) {
                continue;
            }
            (entry.name.clone(), value)
        };
        transaction
            .vault_touch_entry(&touched_name)
            .map_err(|e| format!("Failed to record Vault access: {e}"))?;
        secrets.push((entry.name.clone(), value));
    }
    transaction
        .commit()
        .map_err(|e| format!("Failed to commit Vault materialization transaction: {e}"))?;
    Ok(secrets)
}

#[cfg(test)]
pub(super) fn materialize_unrestricted_vault_entries_from_store_with_hook_for_tests(
    store: &mut MemoryStore,
    key: &[u8; 32],
    include_entry: impl Fn(&VaultEntry) -> bool,
    after_snapshot: impl FnOnce(),
) -> Result<Vec<(String, String)>, String> {
    materialize_unrestricted_vault_entries_from_store_with_hook(
        store,
        key,
        include_entry,
        after_snapshot,
    )
}

#[cfg(test)]
pub(super) fn select_authorized_vault_entry_and_record_access_with_hook_for_tests(
    store: &mut MemoryStore,
    params: &VaultGetParams,
    effective_agent_id: Option<&str>,
    key: &[u8; 32],
    after_select: impl FnOnce(),
) -> Result<(SelectedVaultEntry, String, i64), String> {
    select_authorized_vault_entry_and_record_access_with_hook(
        store,
        params,
        effective_agent_id,
        key,
        after_select,
    )
}

#[cfg(test)]
pub(super) fn record_successful_vault_access(
    store: &mut MemoryStore,
    target_name: &str,
    pending_rotation: Option<&VaultKeyRotation>,
) -> Result<i64, String> {
    if let Some(rotation) = pending_rotation {
        let transaction = store
            .begin_vault_transaction()
            .map_err(|e| format!("Failed to begin access transaction: {e}"))?;
        let access_count = record_successful_vault_access_in_transaction(
            &transaction,
            target_name,
            Some(rotation),
        )?;
        transaction
            .commit()
            .map_err(|e| format!("Failed to commit access transaction: {e}"))?;
        return Ok(access_count);
    }
    store
        .vault_touch_entry(target_name)
        .map_err(|e| e.to_string())
}

pub(crate) fn is_lane_config_name(name: &str) -> bool {
    matches!(
        name,
        "EXTRACT_BASE_URL"
            | "EXTRACT_MODEL"
            | "SUMMARY_BASE_URL"
            | "SUMMARY_MODEL"
            | "DISTILL_BASE_URL"
            | "DISTILL_MODEL"
            | "REASONING_BASE_URL"
            | "REASONING_MODEL"
    )
}

pub(super) fn load_unlocked_vault_secrets(
    server: &MemoryServer,
    include_entry: impl Fn(&VaultEntry) -> bool,
) -> Result<Vec<(String, String)>, String> {
    with_vault_key(server, |key| {
        load_unlocked_vault_secrets_with_key(server, key, include_entry)
    })
}

fn load_unlocked_vault_secrets_with_key(
    server: &MemoryServer,
    key: &[u8; 32],
    include_entry: impl Fn(&VaultEntry) -> bool,
) -> Result<Vec<(String, String)>, String> {
    server.with_global_store(|store| {
        let transaction = store
            .begin_vault_transaction()
            .map_err(|e| format!("Failed to begin Vault materialization transaction: {e}"))?;
        let entries = transaction
            .vault_list_entries()
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
            let value = crypto::decode_utf8_zeroizing(
                decrypted,
                super::VAULT_MATERIALIZATION_INVALID_UTF8,
            )?;
            if !value.trim().is_empty() {
                secrets.push((entry.name, value));
            }
        }
        transaction
            .commit()
            .map_err(|e| format!("Failed to commit Vault materialization transaction: {e}"))?;
        Ok(secrets)
    })
}

pub(crate) fn load_unlocked_api_key_secret_pools(
    server: &MemoryServer,
) -> Result<HashMap<String, Vec<tachi_llm::ProviderSecret>>, String> {
    load_unlocked_api_key_secret_pools_filtered(server, None, true, || {}).map(|scan| scan.pools)
}

pub(crate) fn canonical_api_key_health_logical_name(
    store: &MemoryStore,
    logical_name: &str,
) -> Result<String, String> {
    if store
        .vault_get_rotation(logical_name)
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Ok(logical_name.to_string());
    }
    let Some((prefix, _)) = crate::provider_config::parse_rotation_member_name(logical_name) else {
        return Ok(logical_name.to_string());
    };
    if store
        .vault_get_rotation(prefix)
        .map_err(|error| error.to_string())?
        .is_some()
    {
        Ok(prefix.to_string())
    } else {
        Ok(logical_name.to_string())
    }
}

/// Admitted pools plus drop reasons from the same scan (tachi#1860).
pub(crate) struct ProviderSecretScan {
    pub pools: HashMap<String, Vec<tachi_llm::ProviderSecret>>,
    pub dropped: HashMap<String, AliasSkipClass>,
    pub lane_config_values: crate::provider_config::LaneConfigValues,
    pub acl_revision: u64,
}

pub(crate) fn vault_materialization_acl_revision_from_rows(
    entries: &[VaultEntry],
    rotations: &[VaultKeyRotation],
) -> u64 {
    let mut entries = entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let mut rotations = rotations.iter().collect::<Vec<_>>();
    rotations.sort_by(|left, right| left.prefix.cmp(&right.prefix));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for entry in entries {
        entry.name.hash(&mut hasher);
        entry.secret_type.hash(&mut hasher);
        entry.allowed_agents.hash(&mut hasher);
        entry.updated_at.hash(&mut hasher);
    }
    for rotation in rotations {
        rotation.prefix.hash(&mut hasher);
        rotation.current_index.hash(&mut hasher);
        rotation.total_keys.hash(&mut hasher);
        rotation.rotation_strategy.hash(&mut hasher);
    }
    hasher.finish()
}

/// Same scan as [`load_unlocked_api_key_secret_pools`], plus the drop reason
/// recorded at the moment each listed row was skipped (tachi#1860).
#[cfg(test)]
pub(crate) fn load_unlocked_api_key_secret_pools_with_drops(
    server: &MemoryServer,
) -> Result<ProviderSecretScan, String> {
    load_unlocked_api_key_secret_pools_filtered(server, None, false, || {})
}

pub(crate) fn load_validated_unlocked_api_key_secret_pools_with_drops(
    server: &MemoryServer,
) -> Result<ProviderSecretScan, String> {
    load_unlocked_api_key_secret_pools_filtered(server, None, true, || {})
}

fn record_listed_drop(
    dropped: &mut HashMap<String, AliasSkipClass>,
    name: &str,
    class: AliasSkipClass,
) {
    dropped.entry(name.to_string()).or_insert(class);
}

fn record_rotation_member_drop(
    dropped: &mut HashMap<String, AliasSkipClass>,
    prefix_drop: &mut Option<(usize, AliasSkipClass)>,
    member_index: usize,
    member_name: &str,
    class: AliasSkipClass,
) {
    record_listed_drop(dropped, member_name, class);
    if prefix_drop.is_none_or(|(lowest_index, _)| member_index < lowest_index) {
        *prefix_drop = Some((member_index, class));
    }
}

fn load_unlocked_api_key_secret_pools_filtered(
    server: &MemoryServer,
    only_logical_name: Option<&str>,
    validate_rotations: bool,
    after_snapshot: impl FnOnce(),
) -> Result<ProviderSecretScan, String> {
    with_vault_key_for_provider_refresh(server, |key| {
        server.with_global_store(|store| {
            let transaction = store
                .begin_vault_transaction()
                .map_err(|e| format!("Failed to begin Vault provider transaction: {e}"))?;
            let entries = transaction
                .vault_list_entries()
                .map_err(|e| format!("Failed to list vault provider secrets: {e}"))?;
            let rotations = transaction
                .vault_list_rotations()
                .map_err(|e| format!("Failed to list Vault rotations: {e}"))?;
            let key_health_rows = transaction
                .vault_list_key_health(None)
                .map_err(|e| format!("Failed to list Vault key health: {e}"))?;
            let acl_revision = vault_materialization_acl_revision_from_rows(&entries, &rotations);
            after_snapshot();

            let now = Utc::now();
            let requested_rotation_prefix = only_logical_name.and_then(|logical_name| {
                crate::provider_config::parse_rotation_member_name(logical_name)
                    .map(|(prefix, _)| prefix)
            });
            for rotation in &rotations {
                if !validate_rotations {
                    break;
                }
                if only_logical_name.is_none()
                    || only_logical_name == Some(rotation.prefix.as_str())
                    || requested_rotation_prefix == Some(rotation.prefix.as_str())
                {
                    memcore::validate_api_key_rotation(&entries, rotation).map_err(|error| {
                        format!("{error}; refusing to materialize API-key rotation")
                    })?;
                }
            }
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
                            let db_updated =
                                chrono::DateTime::parse_from_rfc3339(&db_row.updated_at)
                                    .ok()?
                                    .with_timezone(&Utc);
                            let mem_updated =
                                chrono::DateTime::parse_from_rfc3339(&health.updated_at)
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
            let mut dropped: HashMap<String, AliasSkipClass> = HashMap::new();
            let mut lane_config_values = crate::provider_config::LaneConfigValues::default();
            if only_logical_name.is_none() {
                for entry in &entries {
                    if !is_lane_config_name(&entry.name)
                        || entry
                            .allowed_agents
                            .as_ref()
                            .is_some_and(|agents| !agents.is_empty())
                    {
                        continue;
                    }
                    let decrypted =
                        crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
                    let value = crate::vault_crypto::decode_utf8_zeroizing(
                        decrypted,
                        super::VAULT_MATERIALIZATION_INVALID_UTF8,
                    )?;
                    if !value.trim().is_empty() {
                        lane_config_values.push((entry.name.clone(), value));
                    }
                }
            }
            let mut rotation_members: HashSet<String> = HashSet::new();
            let mut materialized_key_ids: BTreeSet<String> = BTreeSet::new();

            let unusable_class = |logical_name: &str, key_id: &str| -> Option<AliasSkipClass> {
                let health = key_health_by_logical.get(logical_name)?.get(key_id)?;
                unusable_skip_class(health, now)
            };

            // Record configured membership before applying the optional pool
            // filter. A concrete-member lease must not fall through to raw-name
            // health identity merely because its prefix pass was filtered out.
            for rotation in &rotations {
                if super::is_lane_slot_secret_name(&rotation.prefix) {
                    continue;
                }
                for (_, entry) in collect_rotation_entries(entries.clone(), &rotation.prefix) {
                    rotation_members.insert(entry.name);
                }
            }

            for rotation in rotations {
                if super::is_lane_slot_secret_name(&rotation.prefix) {
                    continue;
                }
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
                        .min_by_key(|(_, (_, entry))| {
                            (entry.access_count, entry.accessed_at.clone())
                        })
                        .map(|(idx, _)| idx)
                        .unwrap_or(0),
                    _ => 0,
                };
                matching.rotate_left(selected_idx);
                let mut pool = Vec::new();
                let mut prefix_drop = None;
                for (member_index, entry) in matching {
                    // Membership is structural, not conditional on admission.
                    // A configured member rejected below must never fall through
                    // to the standalone raw-name pass and bypass prefix health.
                    rotation_members.insert(entry.name.clone());
                    if memcore::is_lane_config_secret_name(&entry.name) {
                        record_rotation_member_drop(
                            &mut dropped,
                            &mut prefix_drop,
                            member_index,
                            &entry.name,
                            AliasSkipClass::ListedWrongType,
                        );
                        continue;
                    }
                    if memcore::effective_vault_secret_type(&entry.name, &entry.secret_type)
                        != SECRET_TYPE_API_KEY
                    {
                        record_rotation_member_drop(
                            &mut dropped,
                            &mut prefix_drop,
                            member_index,
                            &entry.name,
                            AliasSkipClass::ListedWrongType,
                        );
                        continue;
                    }
                    if entry
                        .allowed_agents
                        .as_ref()
                        .is_some_and(|agents| !agents.is_empty())
                    {
                        record_rotation_member_drop(
                            &mut dropped,
                            &mut prefix_drop,
                            member_index,
                            &entry.name,
                            AliasSkipClass::ListedFenced,
                        );
                        continue;
                    }
                    if let Some(class) = unusable_class(&rotation.prefix, &entry.name) {
                        record_rotation_member_drop(
                            &mut dropped,
                            &mut prefix_drop,
                            member_index,
                            &entry.name,
                            class,
                        );
                        continue;
                    }
                    let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
                    let value = crypto::decode_utf8_zeroizing(
                        decrypted,
                        super::VAULT_MATERIALIZATION_INVALID_UTF8,
                    )?;
                    if value.trim().is_empty() {
                        record_rotation_member_drop(
                            &mut dropped,
                            &mut prefix_drop,
                            member_index,
                            &entry.name,
                            AliasSkipClass::ListedEmpty,
                        );
                        continue;
                    }
                    let key_id = entry.name.clone();
                    pool.push(tachi_llm::ProviderSecret {
                        key_id: key_id.clone(),
                        value,
                    });
                    materialized_key_ids.insert(key_id);
                }
                if !pool.is_empty() {
                    pools.insert(rotation.prefix, pool);
                } else if let Some((_, class)) = prefix_drop {
                    dropped.insert(rotation.prefix, class);
                }
            }

            for entry in &entries {
                if only_logical_name.is_some_and(|logical_name| logical_name != entry.name) {
                    continue;
                }
                if memcore::is_lane_config_secret_name(&entry.name) {
                    record_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedWrongType);
                    continue;
                }
                if memcore::effective_vault_secret_type(&entry.name, &entry.secret_type)
                    != SECRET_TYPE_API_KEY
                {
                    record_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedWrongType);
                    continue;
                }
                if !crate::provider_config::is_provider_api_key_name(&entry.name) {
                    record_listed_drop(
                        &mut dropped,
                        &entry.name,
                        AliasSkipClass::ListedNotModelProvider,
                    );
                    continue;
                }
                if rotation_members.contains(&entry.name) {
                    continue;
                }
                if entry
                    .allowed_agents
                    .as_ref()
                    .is_some_and(|agents| !agents.is_empty())
                {
                    record_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedFenced);
                    continue;
                }
                if let Some(class) = unusable_class(&entry.name, &entry.name) {
                    record_listed_drop(&mut dropped, &entry.name, class);
                    continue;
                }
                let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
                let value = crypto::decode_utf8_zeroizing(
                    decrypted,
                    super::VAULT_MATERIALIZATION_INVALID_UTF8,
                )?;
                if value.trim().is_empty() {
                    record_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedEmpty);
                    continue;
                }
                let (key_id, value) = if let Some(target) = tachi_llm::parse_vault_alias(&value) {
                    if !super::is_lane_slot_secret_name(&entry.name) {
                        continue;
                    }
                    let Some(target_entry) =
                        entries.iter().find(|candidate| candidate.name == target)
                    else {
                        continue;
                    };
                    if super::is_lane_slot_secret_name(&target_entry.name)
                        || memcore::effective_vault_secret_type(
                            &target_entry.name,
                            &target_entry.secret_type,
                        ) != SECRET_TYPE_API_KEY
                        || target_entry
                            .allowed_agents
                            .as_ref()
                            .is_some_and(|agents| !agents.is_empty())
                    {
                        continue;
                    }
                    if let Some(class) = unusable_class(&entry.name, &target_entry.name) {
                        record_listed_drop(&mut dropped, &entry.name, class);
                        continue;
                    }
                    let decrypted =
                        crypto::decrypt(key, &target_entry.encrypted_value, &target_entry.nonce)?;
                    let target_value = crypto::decode_utf8_zeroizing(
                        decrypted,
                        super::VAULT_MATERIALIZATION_INVALID_UTF8,
                    )?;
                    if target_value.trim().is_empty()
                        || tachi_llm::parse_vault_alias(&target_value).is_some()
                    {
                        continue;
                    }
                    (target_entry.name.clone(), target_value)
                } else {
                    (entry.name.clone(), value)
                };
                if let std::collections::hash_map::Entry::Vacant(slot) =
                    pools.entry(entry.name.clone())
                {
                    slot.insert(vec![tachi_llm::ProviderSecret {
                        key_id: key_id.clone(),
                        value,
                    }]);
                    materialized_key_ids.insert(key_id);
                }
            }

            // Decode, UTF-8 validation, filtering, and pool construction must all
            // succeed before access metadata changes. One store call performs one
            // atomic SQLite batch, so any touch failure rolls back every delta.
            if !materialized_key_ids.is_empty() {
                let key_ids = materialized_key_ids.into_iter().collect::<Vec<_>>();
                for key_id in &key_ids {
                    transaction
                        .vault_touch_entry(key_id)
                        .map_err(|e| format!("Failed to record provider key access: {e}"))?;
                }
            }

            let scan = ProviderSecretScan {
                pools,
                dropped,
                lane_config_values,
                acl_revision,
            };
            transaction
                .commit()
                .map_err(|e| format!("Failed to commit Vault provider transaction: {e}"))?;
            Ok(scan)
        })
    })
}

#[cfg(test)]
pub(super) fn load_unlocked_api_key_secret_pools_with_acl_hook_for_tests(
    server: &MemoryServer,
    after_snapshot: impl FnOnce(),
) -> Result<HashMap<String, Vec<tachi_llm::ProviderSecret>>, String> {
    load_unlocked_api_key_secret_pools_filtered(server, None, true, after_snapshot)
        .map(|scan| scan.pools)
}

pub(crate) fn read_unlocked_vault_secret(
    server: &MemoryServer,
    name: &str,
    agent_id: Option<&str>,
    auto_rotate: bool,
) -> Result<String, String> {
    let effective_agent_id = resolve_vault_acl_agent_id(server, agent_id)?;
    with_vault_key(server, |key| {
        let params = VaultGetParams {
            name: name.to_string(),
            agent_id: agent_id.map(str::to_string),
            auto_rotate,
        };
        server.with_global_store(|store| {
            read_usable_vault_secret_from_store(store, key, &params, effective_agent_id.as_deref())
        })
    })
}

#[derive(Debug)]
pub(super) struct AuthorizedApiKeyLease {
    pub logical_name: String,
    pub key_id: String,
    pub value: String,
    pub access_count: i64,
}

fn lease_authorized_api_key_with_hook(
    server: &MemoryServer,
    requested_name: &str,
    effective_agent_id: Option<&str>,
    after_select: impl FnOnce(),
) -> Result<AuthorizedApiKeyLease, String> {
    with_vault_key(server, |key| {
        server.with_global_store(|store| {
            let transaction = store
                .begin_vault_transaction()
                .map_err(|e| format!("Failed to begin API-key lease transaction: {e}"))?;
            let entries = transaction
                .vault_list_entries()
                .map_err(|e| format!("Failed to list Vault entries: {e}"))?;
            let rotations = transaction
                .vault_list_rotations()
                .map_err(|e| format!("Failed to list Vault rotations: {e}"))?;
            let health_rows = transaction
                .vault_list_key_health(None)
                .map_err(|e| format!("Failed to list Vault key health: {e}"))?;

            let requested_is_slot = super::is_lane_slot_secret_name(requested_name);
            let member_rotation = (!requested_is_slot)
                .then(|| crate::provider_config::parse_rotation_member_name(requested_name))
                .flatten()
                .and_then(|(prefix, _)| rotations.iter().find(|row| row.prefix == prefix));
            let rotation = if requested_is_slot {
                None
            } else {
                rotations
                    .iter()
                    .find(|row| row.prefix == requested_name)
                    .or(member_rotation)
            };
            if let Some(rotation) = rotation {
                memcore::validate_api_key_rotation(&entries, rotation)
                    .map_err(|error| format!("{error}; refusing API-key lease"))?;
            }

            let logical_name = rotation
                .map(|row| row.prefix.as_str())
                .unwrap_or(requested_name);
            let mut health_by_key = health_rows
                .iter()
                .filter(|row| row.logical_name == logical_name)
                .map(|row| (row.key_id.clone(), row.clone()))
                .collect::<HashMap<_, _>>();
            if let Some(in_memory) = server.llm.provider_health_memory_snapshot().get(logical_name) {
                for (key_id, health) in in_memory {
                    let keep_in_memory = health_by_key
                        .get(key_id)
                        .and_then(|persisted| {
                            let persisted_at = chrono::DateTime::parse_from_rfc3339(&persisted.updated_at).ok()?;
                            let memory_at = chrono::DateTime::parse_from_rfc3339(&health.updated_at).ok()?;
                            Some(memory_at >= persisted_at)
                        })
                        .unwrap_or(true);
                    if keep_in_memory {
                        health_by_key.insert(key_id.clone(), health.clone());
                    }
                }
            }

            let mut candidates = if let Some(rotation) = rotation {
                let mut matching = collect_rotation_entries(entries.clone(), &rotation.prefix);
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
                        .min_by_key(|(_, (_, entry))| {
                            (entry.access_count, entry.accessed_at.clone())
                        })
                        .map(|(index, _)| index)
                        .unwrap_or(0),
                    _ => 0,
                };
                matching.rotate_left(selected_idx);
                matching
            } else {
                entries
                    .iter()
                    .find(|entry| entry.name == requested_name)
                    .cloned()
                    .map(|entry| vec![(1, entry)])
                    .unwrap_or_default()
            };
            if member_rotation.is_some() {
                candidates.retain(|(_, entry)| entry.name == requested_name);
            }

            let now = Utc::now();
            let mut selected = None;
            let mut materialized_key_ids = BTreeSet::new();
            for (_, entry) in candidates {
                if memcore::effective_vault_secret_type(&entry.name, &entry.secret_type)
                    != SECRET_TYPE_API_KEY
                {
                    continue;
                }
                if ensure_agent_allowed(&entry, effective_agent_id).is_err() {
                    continue;
                }
                if health_by_key
                    .get(&entry.name)
                    .and_then(|health| unusable_skip_class(health, now))
                    .is_some()
                {
                    continue;
                }
                let decrypted = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
                let value = crypto::decode_utf8_zeroizing(
                    decrypted,
                    super::VAULT_MATERIALIZATION_INVALID_UTF8,
                )?;
                let (entry, value) = if super::is_lane_slot_secret_name(&entry.name) {
                    let target = tachi_llm::parse_vault_alias(&value)
                        .map(str::to_string)
                        .ok_or_else(|| {
                            format!(
                                "Lane slot '{}' is not bound to a usable account",
                                entry.name
                            )
                        })?;
                    let target_entry = entries
                        .iter()
                        .find(|candidate| candidate.name == target)
                        .cloned()
                        .ok_or_else(|| format!("Lane slot target '{target}' is missing"))?;
                    let decrypted =
                        crypto::decrypt(key, &target_entry.encrypted_value, &target_entry.nonce)?;
                    let target_value = crypto::decode_utf8_zeroizing(
                        decrypted,
                        super::VAULT_MATERIALIZATION_INVALID_UTF8,
                    )?;
                    let slot_health = health_rows.iter().find(|health| {
                        health.logical_name == entry.name && health.key_id == target_entry.name
                    });
                    let target_health = health_rows.iter().find(|health| {
                        health.logical_name == target_entry.name
                            && health.key_id == target_entry.name
                    });
                    super::account_bind::refuse_unusable_account_target(
                        &entry.name,
                        &target_entry,
                        &target_value,
                        effective_agent_id,
                        super::account_bind::slot_target_health_unusable(
                            slot_health,
                            target_health,
                        ),
                    )?;
                    (target_entry, target_value)
                } else {
                    (entry, value)
                };
                if !value.trim().is_empty() {
                    materialized_key_ids.insert(entry.name.clone());
                    if selected.is_none() {
                        selected = Some((entry, value));
                    }
                }
            }
            let (entry, value) = selected.ok_or_else(|| {
                format!(
                    "No usable API key available for '{requested_name}'. Vault may be locked, missing, restricted, or all keys are disabled/auth-failed/rate-limited."
                )
            })?;
            after_select();
            ensure_agent_allowed(&entry, effective_agent_id).map_err(|e| e.to_string())?;

            if let Some(rotation) = rotation {
                let member_index = memcore::api_key_pool_member_index(&entry.name, &rotation.prefix)
                    .ok_or_else(|| format!("Selected key '{}' is not in rotation '{}'", entry.name, rotation.prefix))?;
                let mut updated = rotation.clone();
                updated.current_index = (member_index as i64 % updated.total_keys) + 1;
                updated.updated_at = Utc::now().to_rfc3339();
                transaction
                    .vault_set_rotation(&updated)
                    .map_err(|e| format!("Failed to advance API-key rotation: {e}"))?;
            }
            let mut access_count = None;
            for key_id in materialized_key_ids {
                let count = transaction
                    .vault_touch_entry(&key_id)
                    .map_err(|e| format!("Failed to record API-key lease access: {e}"))?;
                if key_id == entry.name {
                    access_count = Some(count);
                }
            }
            let access_count = access_count.ok_or_else(|| {
                format!("Selected API key '{}' was not materialized", entry.name)
            })?;
            transaction
                .commit()
                .map_err(|e| format!("Failed to commit API-key lease transaction: {e}"))?;
            Ok(AuthorizedApiKeyLease {
                logical_name: logical_name.to_string(),
                key_id: entry.name,
                value,
                access_count,
            })
        })
    })
}

pub(super) fn lease_authorized_api_key(
    server: &MemoryServer,
    requested_name: &str,
    effective_agent_id: Option<&str>,
) -> Result<AuthorizedApiKeyLease, String> {
    lease_authorized_api_key_with_hook(server, requested_name, effective_agent_id, || {})
}

#[cfg(test)]
pub(super) fn lease_authorized_api_key_with_hook_for_tests(
    server: &MemoryServer,
    requested_name: &str,
    effective_agent_id: Option<&str>,
    after_select: impl FnOnce(),
) -> Result<AuthorizedApiKeyLease, String> {
    lease_authorized_api_key_with_hook(server, requested_name, effective_agent_id, after_select)
}
