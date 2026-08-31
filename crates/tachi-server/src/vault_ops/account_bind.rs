//! Lane slots bind to provider accounts. They must not store a second copy
//! of account ciphertext (tachi#1855 redesign).
//!
//! `EXTRACT_API_KEY` / `SUMMARY_API_KEY` / `DISTILL_API_KEY` /
//! `REASONING_API_KEY` are slots. `vault set SLOT <bytes>` either writes
//! `vault:ACCOUNT` or refuses. `--rebind` changes the pointer, never copies
//! a new family into the slot row. Account names still rotate in place.

use super::slot_rebind::fingerprint_secret;
use crate::vault_crypto as crypto;
use chrono::Utc;
use memcore::vault::{VaultEntry, VaultKeyHealth, SECRET_TYPE_API_KEY};
use memcore::MemoryStore;
pub(crate) use tachi_llm::is_lane_slot_secret_name;
use tachi_llm::parse_vault_alias;

pub(crate) fn health_row_unusable(health: &VaultKeyHealth) -> bool {
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

pub(crate) fn slot_target_health_unusable(
    slot_health: Option<&VaultKeyHealth>,
    target_health: Option<&VaultKeyHealth>,
) -> bool {
    [slot_health, target_health]
        .into_iter()
        .flatten()
        .any(health_row_unusable)
}

/// Shared usable-secret policy for a pointed-to account.
pub(crate) fn refuse_unusable_account_target(
    slot: &str,
    target: &VaultEntry,
    target_plain: &str,
    agent_id: Option<&str>,
    health_unusable: bool,
) -> Result<(), String> {
    if is_lane_slot_secret_name(&target.name) {
        return Err(format!(
            "Lane slot '{slot}' cannot resolve through another slot '{}'",
            target.name
        ));
    }
    if crate::status_ops::status_health::account_class_for_env_name(&target.name)
        != Some(memcore::AccountClass::ModelApi)
    {
        return Err(format!(
            "Lane slot '{slot}' points at '{}' which is not a registered ModelApi provider account",
            target.name
        ));
    }
    if memcore::effective_vault_secret_type(&target.name, &target.secret_type)
        != SECRET_TYPE_API_KEY
    {
        return Err(format!(
            "Lane slot '{slot}' points at '{}' which is not an API key",
            target.name
        ));
    }
    if target_plain.trim().is_empty() || parse_vault_alias(target_plain).is_some() {
        return Err(format!(
            "Lane slot '{slot}' points at an unusable account '{}'",
            target.name
        ));
    }
    if let Some(allowed) = target.allowed_agents.as_ref() {
        if !allowed.is_empty() {
            match agent_id.map(str::trim).filter(|agent| !agent.is_empty()) {
                None => {
                    return Err(format!(
                        "Lane slot '{slot}' points at account '{}' which is not usable because it is restricted",
                        target.name
                    ));
                }
                Some(agent) if !allowed.iter().any(|allowed_agent| allowed_agent == agent) => {
                    return Err(format!(
                        "Lane slot '{slot}' points at account '{}' which is not usable by this agent",
                        target.name
                    ));
                }
                Some(_) => {}
            }
        }
    }
    if health_unusable {
        return Err(format!(
            "Lane slot '{slot}' points at an unusable account '{}'",
            target.name
        ));
    }
    Ok(())
}

/// Read the slot-target and target-target health rows while holding the same
/// Vault transaction as the binding decision.
pub(crate) fn account_target_health_unusable(
    transaction: &memcore::store::vault::VaultTransaction<'_>,
    slot: &str,
    target: &VaultEntry,
) -> Result<bool, String> {
    let slot_health = transaction
        .vault_get_key_health(slot, &target.name)
        .map_err(|e| format!("vault_get_key_health: {e}"))?;
    let target_health = transaction
        .vault_get_key_health(&target.name, &target.name)
        .map_err(|e| format!("vault_get_key_health: {e}"))?;

    Ok(slot_target_health_unusable(
        slot_health.as_ref(),
        target_health.as_ref(),
    ))
}

pub(crate) fn refuse_lane_slot_pool_prefix(prefix: &str) -> Result<(), String> {
    if is_lane_slot_secret_name(prefix) {
        Err(format!(
            "Lane slot '{prefix}' binds to an account; it cannot be an API-key rotation pool"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn follow_lane_slot_pointers(values: Vec<(String, String)>) -> Vec<(String, String)> {
    let by_name: std::collections::HashMap<&str, &str> = values
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    values
        .iter()
        .filter_map(|(name, value)| {
            if !is_lane_slot_secret_name(name) {
                if parse_vault_alias(value).is_some() {
                    return None;
                }
                return Some((name.clone(), value.clone()));
            }
            let target = parse_vault_alias(value)?;
            if is_lane_slot_secret_name(target) {
                return None;
            }
            let target_value = by_name.get(target)?;
            if parse_vault_alias(target_value).is_some() {
                return None;
            }
            Some((name.clone(), (*target_value).to_string()))
        })
        .collect()
}

/// Rewrite leftover slot ciphertext in a sync bundle to `vault:ACCOUNT`
/// pointers. Already-bound pointers are kept. Unmatched raw slot bytes refuse
/// the import instead of recreating a second copy.
pub(crate) fn rewrite_imported_lane_slots(
    master_key: &[u8; 32],
    entries: &[VaultEntry],
) -> Result<Vec<VaultEntry>, String> {
    let slot_entries = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| is_lane_slot_secret_name(&entry.name));
    let mut slot_plain = Vec::new();
    for (idx, entry) in slot_entries {
        super::slot_rebind::validate_lane_slot_secret_type(&entry.name, &entry.secret_type)?;
        slot_plain.push((
            idx,
            decrypt_secret_value(master_key, entry, "Imported vault")?,
        ));
    }
    if slot_plain.is_empty() {
        return Ok(entries.to_vec());
    }

    let required_accounts: Vec<&str> = slot_plain
        .iter()
        .filter_map(|(_, plain)| parse_vault_alias(plain.trim()))
        .collect();
    let needs_raw_match = slot_plain
        .iter()
        .any(|(_, plain)| parse_vault_alias(plain.trim()).is_none());
    let account_rows = decrypt_candidate_account_rows(
        master_key,
        entries,
        &required_accounts,
        needs_raw_match,
        None,
    )?;
    let accounts = SensitiveAccounts(bindable_accounts(account_rows.0.iter().cloned()));
    let mut out = entries.to_vec();
    for (idx, plain) in slot_plain {
        let decided = decide_lane_slot_write(
            master_key,
            &out[idx].name,
            &plain,
            Some(&*plain),
            &accounts.0,
            false,
        )
        .map_err(|error| enrich_unmatched_account_error(error, &plain, &account_rows))?;
        let target_entry = entries
            .iter()
            .find(|entry| entry.name == decided.account)
            .ok_or_else(|| format!("Imported lane slot target '{}' is missing", decided.account))?;
        let target_value = accounts
            .0
            .iter()
            .find(|(name, _, _)| name == &decided.account)
            .map(|(_, value, _)| value.as_str())
            .ok_or_else(|| format!("Imported lane slot target '{}' is missing", decided.account))?;
        refuse_unusable_account_target(&out[idx].name, target_entry, target_value, None, false)?;
        if decided.store_value != *plain {
            let (encrypted_value, nonce) =
                crypto::encrypt(master_key, decided.store_value.as_bytes())
                    .map_err(|e| format!("encrypt imported slot pointer: {e}"))?;
            out[idx].encrypted_value = encrypted_value;
            out[idx].nonce = nonce;
            out[idx].secret_type = SECRET_TYPE_API_KEY.to_string();
        }
    }
    Ok(out)
}

pub(crate) fn write_lane_slot_binding(
    store: &mut MemoryStore,
    master_key: &[u8; 32],
    name: &str,
    new_value: &str,
    rebind: bool,
    description: &str,
    allowed_agents: Option<Vec<String>>,
    effective_agent_id: Option<&str>,
) -> Result<(LaneSlotDecision, bool), String> {
    let name = name.trim();
    let transaction = store
        .begin_vault_transaction()
        .map_err(|e| format!("begin lane slot transaction: {e}"))?;
    let entries = transaction
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let existing_entry = entries.iter().find(|entry| entry.name == name);
    if let Some(existing) = existing_entry {
        super::slot_rebind::validate_existing_lane_slot_secret_type(name, &existing.secret_type)?;
        super::access::ensure_agent_allowed(existing, effective_agent_id)
            .map_err(|e| e.to_string())?;
    }
    let existing_plain = existing_entry
        .map(|entry| decrypt_secret_value(master_key, entry, "Vault"))
        .transpose()?;
    let mut required_accounts = Vec::new();
    if let Some(account) = parse_vault_alias(new_value.trim()) {
        required_accounts.push(account);
    }
    if let Some(account) = existing_plain
        .as_deref()
        .map(str::trim)
        .and_then(parse_vault_alias)
    {
        required_accounts.push(account);
    }
    let needs_raw_match = parse_vault_alias(new_value.trim()).is_none()
        || existing_plain
            .as_deref()
            .is_some_and(|value| parse_vault_alias(value.trim()).is_none());
    let account_rows = decrypt_candidate_account_rows(
        master_key,
        &entries,
        &required_accounts,
        needs_raw_match,
        effective_agent_id,
    )?;
    let accounts = SensitiveAccounts(bindable_accounts(account_rows.0.iter().cloned()));
    let decided = decide_lane_slot_write(
        master_key,
        name,
        new_value,
        existing_plain.as_deref(),
        &accounts.0,
        rebind,
    )
    .map_err(|error| enrich_unmatched_account_error(error, new_value, &account_rows))?;
    let target_entry = entries
        .iter()
        .find(|entry| entry.name == decided.account)
        .ok_or_else(|| format!("Lane slot target '{}' is missing", decided.account))?;
    let target_value = accounts
        .0
        .iter()
        .find(|(account, _, _)| account == &decided.account)
        .map(|(_, value, _)| value.as_str())
        .ok_or_else(|| format!("Lane slot target '{}' is missing", decided.account))?;
    let target_health_unusable = account_target_health_unusable(&transaction, name, target_entry)?;
    refuse_unusable_account_target(
        name,
        target_entry,
        target_value,
        effective_agent_id,
        target_health_unusable,
    )?;
    let (encrypted_value, nonce) = crypto::encrypt(master_key, decided.store_value.as_bytes())
        .map_err(|e| format!("encrypt slot pointer: {e}"))?;
    let created = existing_entry.is_none();
    let now = Utc::now().to_rfc3339();
    let entry = VaultEntry {
        name: name.to_string(),
        encrypted_value,
        nonce,
        secret_type: SECRET_TYPE_API_KEY.to_string(),
        description: description.to_string(),
        allowed_agents,
        created_at: existing_entry
            .map(|entry| entry.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };
    transaction
        .vault_upsert_entry(&entry)
        .map_err(|e| format!("vault_upsert_entry lane slot: {e}"))?;
    let account = super::account_events::observe_account_entry(
        &transaction,
        master_key,
        &target_entry.name,
        &target_entry.secret_type,
        target_value,
        true,
    )?
    .ok_or("lane slot target has no ModelApi account identity")?;
    transaction
        .vault_record_slot_account_alias(
            &account,
            name,
            decided.old_fingerprint.as_deref(),
            &decided.new_fingerprint,
            decided.rebound,
        )
        .map_err(|error| format!("record slot account event: {error}"))?;
    transaction
        .commit()
        .map_err(|e| format!("commit lane slot transaction: {e}"))?;
    Ok((decided, created))
}

struct SensitiveAccountRows(Vec<(String, String, String)>);

impl Drop for SensitiveAccountRows {
    fn drop(&mut self) {
        for (_, value, _) in &mut self.0 {
            crypto::zero_string(value);
        }
    }
}

struct SensitiveAccounts(Vec<(String, String, &'static str)>);

impl Drop for SensitiveAccounts {
    fn drop(&mut self) {
        for (_, value, _) in &mut self.0 {
            crypto::zero_string(value);
        }
    }
}

fn decrypt_secret_value(
    master_key: &[u8; 32],
    entry: &VaultEntry,
    surface: &str,
) -> Result<crypto::ZeroizingString, String> {
    let decrypted = crypto::decrypt(master_key, &entry.encrypted_value, &entry.nonce)
        .map_err(|e| format!("decrypt {surface} '{}': {e}", entry.name))?;
    let value = crypto::decode_utf8_zeroizing(
        decrypted,
        format!("{surface} secret '{}' is not valid UTF-8", entry.name),
    )?;
    Ok(crypto::ZeroizingString::new(value))
}

fn is_candidate_account_entry(entry: &VaultEntry) -> bool {
    !is_lane_slot_secret_name(&entry.name)
        && crate::provider_config::is_provider_api_key_name(&entry.name)
        && memcore::effective_vault_secret_type(&entry.name, &entry.secret_type)
            == SECRET_TYPE_API_KEY
}

fn is_bindable_account_metadata(entry: &VaultEntry) -> bool {
    is_candidate_account_entry(entry)
        && crate::status_ops::status_health::account_class_for_env_name(&entry.name)
            == Some(memcore::AccountClass::ModelApi)
        && crate::status_ops::status_health::provider_kind_for_env_name(&entry.name).is_some()
}

/// Decrypt candidate accounts needed by the decision. Explicit pointers name
/// required targets and fail loudly when those rows are corrupt, but ACL and
/// structural eligibility are checked first. Raw matching skips unauthorized
/// rows before decrypting them. Authorized provider-shaped rows remain available
/// for the existing unregistered-account refusal diagnostic, never for binding.
fn decrypt_candidate_account_rows(
    master_key: &[u8; 32],
    entries: &[VaultEntry],
    required_accounts: &[&str],
    needs_raw_match: bool,
    effective_agent_id: Option<&str>,
) -> Result<SensitiveAccountRows, String> {
    let required_accounts: std::collections::HashSet<&str> =
        required_accounts.iter().copied().collect();
    let mut rows = SensitiveAccountRows(Vec::new());
    for entry in entries.iter().filter(|entry| {
        required_accounts.contains(entry.name.as_str())
            || (needs_raw_match && is_candidate_account_entry(entry))
    }) {
        let required = required_accounts.contains(entry.name.as_str());
        if let Err(error) = super::access::ensure_agent_allowed(entry, effective_agent_id) {
            if required {
                return Err(error.to_string());
            }
            continue;
        }
        if required && !is_bindable_account_metadata(entry) {
            continue;
        }
        let value = match decrypt_secret_value(master_key, entry, "Vault") {
            Ok(value) => value,
            Err(_) if !required => {
                continue;
            }
            Err(error) => return Err(error),
        };
        rows.0.push((
            entry.name.clone(),
            value.to_string(),
            entry.secret_type.clone(),
        ));
    }
    Ok(rows)
}

fn enrich_unmatched_account_error(
    error: String,
    new_value: &str,
    rows: &SensitiveAccountRows,
) -> String {
    if parse_vault_alias(new_value).is_some() || !error.contains("second copy") {
        return error;
    }
    let new_value = new_value.trim();
    let Some((name, _, _)) = rows
        .0
        .iter()
        .find(|(_, value, _)| value.trim() == new_value)
    else {
        return error;
    };
    format!(
        "Lane slot raw bytes match existing provider-shaped account '{name}', but that account is not a registered ModelApi account; refusing the copied ciphertext. {error}"
    )
}

pub(crate) fn bindable_accounts(
    rows: impl IntoIterator<Item = (String, String, String)>,
) -> Vec<(String, String, &'static str)> {
    rows.into_iter()
        .filter_map(|(name, mut value, secret_type)| {
            if is_lane_slot_secret_name(&name)
                || parse_vault_alias(&value).is_some()
                || memcore::effective_vault_secret_type(&name, &secret_type) != SECRET_TYPE_API_KEY
                || crate::status_ops::status_health::account_class_for_env_name(&name)
                    != Some(memcore::AccountClass::ModelApi)
            {
                crypto::zero_string(&mut value);
                return None;
            }
            let Some(kind) = crate::status_ops::status_health::provider_kind_for_env_name(&name)
            else {
                crypto::zero_string(&mut value);
                return None;
            };
            Some((name, value, kind))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneSlotDecision {
    /// Value stored on the slot row: `vault:ACCOUNT`, never raw key bytes.
    pub store_value: String,
    pub account: String,
    pub old_fingerprint: Option<String>,
    pub new_fingerprint: String,
    /// Compatibility alias for callers that predate the explicit transition
    /// fields. It is always the same value as `new_fingerprint`.
    pub fingerprint: String,
    pub rebound: bool,
    pub noop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccountMatch {
    name: String,
    fingerprint: String,
}

fn find_matching_account(
    master_key: &[u8; 32],
    new_value: &str,
    accounts: &[(String, String, &'static str)],
) -> Option<AccountMatch> {
    for (name, value, kind) in accounts {
        if parse_vault_alias(value).is_some() {
            continue;
        }
        let fp = fingerprint_secret(master_key, kind, value);
        if fp == fingerprint_secret(master_key, kind, new_value) {
            return Some(AccountMatch {
                name: name.clone(),
                fingerprint: fp,
            });
        }
    }
    None
}

fn current_pointer(existing: Option<&str>) -> Option<&str> {
    existing.and_then(parse_vault_alias)
}

fn current_account_match(
    master_key: &[u8; 32],
    existing: Option<&str>,
    accounts: &[(String, String, &'static str)],
) -> Option<AccountMatch> {
    let existing = existing?.trim();
    if let Some(pointer) = current_pointer(Some(existing)) {
        return accounts
            .iter()
            .find(|(name, _, _)| name == pointer)
            .map(|(name, value, kind)| AccountMatch {
                name: name.clone(),
                fingerprint: fingerprint_secret(master_key, kind, value),
            });
    }
    find_matching_account(master_key, existing, accounts)
}

/// Decide what a lane-slot `vault set` may store.
///
/// `accounts` is `(name, plaintext, provider_kind)` for non-slot api_key rows.
pub(crate) fn decide_lane_slot_write(
    master_key: &[u8; 32],
    slot: &str,
    new_value: &str,
    existing_slot_value: Option<&str>,
    accounts: &[(String, String, &'static str)],
    rebind: bool,
) -> Result<LaneSlotDecision, String> {
    let slot = slot.trim();
    let new_value = new_value.trim();
    if !is_lane_slot_secret_name(slot) {
        return Err(format!("'{slot}' is not a lane slot"));
    }

    let target = if let Some(account) = parse_vault_alias(new_value) {
        if is_lane_slot_secret_name(account) {
            return Err(format!(
                "Lane slot '{slot}' cannot bind to another slot '{account}'"
            ));
        }
        if account == slot {
            return Err(format!("Lane slot '{slot}' cannot bind to itself"));
        }
        let Some((_, value, kind)) = accounts.iter().find(|(name, _, _)| name == account) else {
            return Err(format!(
                "Lane slot '{slot}' cannot bind to missing or unusable account '{account}'"
            ));
        };
        if parse_vault_alias(value).is_some() {
            return Err(format!(
                "Lane slot '{slot}' cannot bind to account '{account}' whose value is itself a vault alias"
            ));
        }
        let fp = fingerprint_secret(master_key, kind, value);
        AccountMatch {
            name: account.to_string(),
            fingerprint: fp,
        }
    } else {
        find_matching_account(master_key, new_value, accounts).ok_or_else(|| {
            format!(
                "Lane slot '{slot}' would store a second copy of ciphertext. \
                 A matching provider account may be absent, unusable, or restricted. \
                 Store the key on an accessible registered provider account \
                 then bind with {slot}=vault:ACCOUNT."
            )
        })?
    };

    let pointer = format!("vault:{}", target.name);
    let current = current_pointer(existing_slot_value);
    let old_match = current_account_match(master_key, existing_slot_value, accounts);
    // Legacy bytes still have truthful keyed evidence even if their original
    // account no longer exists. The slot-name domain identifies raw stored
    // bytes, not an inferred provider identity; explicit rebind is still required.
    let old_fingerprint = old_match
        .as_ref()
        .map(|account| account.fingerprint.clone())
        .or_else(|| {
            existing_slot_value
                .filter(|value| parse_vault_alias(value).is_none())
                .map(|value| fingerprint_secret(master_key, slot, value))
        });

    if current == Some(target.name.as_str()) {
        return Ok(LaneSlotDecision {
            store_value: pointer,
            account: target.name,
            old_fingerprint,
            new_fingerprint: target.fingerprint.clone(),
            fingerprint: target.fingerprint,
            rebound: false,
            noop: true,
        });
    }

    // A legacy raw row is already semantically bound when its bytes match the
    // selected registered account. The submitted value may be either the raw
    // bytes or the new pointer, so comparing old bytes to `new_value` is not a
    // valid migration test.
    let leftover_same_account = existing_slot_value.is_some()
        && current.is_none()
        && old_match
            .as_ref()
            .is_some_and(|account| account.name == target.name);
    let is_first_write = existing_slot_value.is_none();
    if is_first_write || leftover_same_account {
        return Ok(LaneSlotDecision {
            store_value: pointer,
            account: target.name,
            old_fingerprint,
            new_fingerprint: target.fingerprint.clone(),
            fingerprint: target.fingerprint,
            rebound: false,
            noop: false,
        });
    }

    if !rebind {
        let old = current
            .map(|name| format!("vault:{name}"))
            .unwrap_or_else(|| "a leftover ciphertext row".to_string());
        let old_fingerprint = old_fingerprint.as_deref().unwrap_or("unavailable");
        return Err(format!(
            "Lane slot '{slot}' is bound to {old} (old_fingerprint={old_fingerprint}); new binding is vault:{} (new_fingerprint={}). \
             Pass rebind=true / --rebind to change account family. \
             This is not a ciphertext overwrite.",
            target.name, target.fingerprint
        ));
    }

    Ok(LaneSlotDecision {
        store_value: pointer,
        account: target.name,
        old_fingerprint,
        new_fingerprint: target.fingerprint.clone(),
        fingerprint: target.fingerprint,
        rebound: true,
        noop: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [7u8; 32];

    fn accounts() -> Vec<(String, String, &'static str)> {
        vec![
            (
                "DEEPSEEK_API_KEY".to_string(),
                "deepseek-secret".to_string(),
                "deepseek",
            ),
            (
                "SILICONFLOW_API_KEY".to_string(),
                "siliconflow-secret".to_string(),
                "siliconflow",
            ),
        ]
    }

    #[test]
    fn lane_slot_names_are_the_four_chat_lanes() {
        assert!(is_lane_slot_secret_name("EXTRACT_API_KEY"));
        assert!(is_lane_slot_secret_name("DISTILL_API_KEY"));
        assert!(!is_lane_slot_secret_name("DEEPSEEK_API_KEY"));
        assert!(!is_lane_slot_secret_name("ZAI_API_KEY"));
    }

    #[test]
    fn matching_account_bytes_store_a_pointer_not_a_copy() {
        let decided = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "deepseek-secret",
            None,
            &accounts(),
            false,
        )
        .expect("bind");
        assert_eq!(decided.store_value, "vault:DEEPSEEK_API_KEY");
        assert!(!decided.store_value.contains("deepseek-secret"));
        assert!(!decided.rebound);
        assert!(!decided.noop);
    }

    #[test]
    fn unmatched_bytes_are_refused_even_with_rebind() {
        let err = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "glm-orphan-secret",
            None,
            &accounts(),
            true,
        )
        .expect_err("must not copy");
        assert!(
            err.contains("second copy") || err.contains("provider account"),
            "{err}"
        );
        assert!(!err.contains("glm-orphan-secret"), "{err}");
    }

    #[test]
    fn changing_pointer_without_rebind_is_refused() {
        let err = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "siliconflow-secret",
            Some("vault:DEEPSEEK_API_KEY"),
            &accounts(),
            false,
        )
        .expect_err("need rebind");
        assert!(
            err.contains("--rebind") || err.contains("rebind=true"),
            "{err}"
        );
        assert!(err.contains("vault:DEEPSEEK_API_KEY"), "{err}");
        assert!(err.contains("SILICONFLOW_API_KEY"), "{err}");
        assert!(err.contains("old_fingerprint=fp1:"), "{err}");
        assert!(err.contains("new_fingerprint=fp1:"), "{err}");
        assert!(!err.contains("siliconflow-secret"), "{err}");
    }

    #[test]
    fn rebind_writes_the_new_pointer() {
        let decided = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "siliconflow-secret",
            Some("vault:DEEPSEEK_API_KEY"),
            &accounts(),
            true,
        )
        .expect("rebind");
        assert_eq!(decided.store_value, "vault:SILICONFLOW_API_KEY");
        assert!(decided.rebound);
    }

    #[test]
    fn identical_pointer_is_noop() {
        let decided = decide_lane_slot_write(
            &MASTER,
            "DISTILL_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            Some("vault:DEEPSEEK_API_KEY"),
            &accounts(),
            false,
        )
        .expect("noop");
        assert!(decided.noop);
        assert_eq!(decided.store_value, "vault:DEEPSEEK_API_KEY");
    }

    #[test]
    fn bindable_accounts_skip_alias_and_non_api_key_rows() {
        let accounts = bindable_accounts(vec![
            (
                "DEEPSEEK_API_KEY".to_string(),
                "vault:SILICONFLOW_API_KEY".to_string(),
                SECRET_TYPE_API_KEY.to_string(),
            ),
            (
                "SILICONFLOW_API_KEY".to_string(),
                "siliconflow-secret".to_string(),
                "password".to_string(),
            ),
            (
                "ZAI_API_KEY".to_string(),
                "zai-secret".to_string(),
                SECRET_TYPE_API_KEY.to_string(),
            ),
        ]);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, "ZAI_API_KEY");
    }

    #[test]
    fn bindable_accounts_accept_only_exact_registered_model_api_names() {
        let accounts = bindable_accounts(vec![
            (
                "TAVILY_API_KEY".to_string(),
                "search-secret".to_string(),
                SECRET_TYPE_API_KEY.to_string(),
            ),
            (
                "MCP_CONTEXT7_API_KEY".to_string(),
                "mcp-secret".to_string(),
                SECRET_TYPE_API_KEY.to_string(),
            ),
            (
                "VOYAGE_API_KEY_1".to_string(),
                "numbered-secret".to_string(),
                SECRET_TYPE_API_KEY.to_string(),
            ),
            (
                "DEEPSEEK_API_KEY".to_string(),
                "model-secret".to_string(),
                SECRET_TYPE_API_KEY.to_string(),
            ),
        ]);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, "DEEPSEEK_API_KEY");
    }

    #[test]
    fn alias_valued_account_is_not_a_bind_target() {
        let accounts = vec![(
            "DEEPSEEK_API_KEY".to_string(),
            "vault:SILICONFLOW_API_KEY".to_string(),
            "deepseek",
        )];
        let err = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            None,
            &accounts,
            false,
        )
        .expect_err("alias-valued account");
        assert!(
            err.contains("vault alias") || err.contains("unusable"),
            "{err}"
        );
    }

    #[test]
    fn leftover_copy_of_same_bytes_upgrades_to_pointer() {
        let decided = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "deepseek-secret",
            Some("deepseek-secret"),
            &accounts(),
            false,
        )
        .expect("upgrade");
        assert_eq!(decided.store_value, "vault:DEEPSEEK_API_KEY");
        assert!(!decided.noop);
    }

    #[test]
    fn leftover_raw_row_migrates_when_submission_is_the_matching_pointer() {
        let decided = decide_lane_slot_write(
            &MASTER,
            "EXTRACT_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            Some("deepseek-secret"),
            &accounts(),
            false,
        )
        .expect("matching legacy bytes migrate to the requested pointer");
        assert_eq!(decided.store_value, "vault:DEEPSEEK_API_KEY");
        assert_eq!(decided.account, "DEEPSEEK_API_KEY");
        assert!(!decided.rebound);
        assert!(!decided.noop);
        assert_eq!(
            decided.old_fingerprint,
            Some(decided.new_fingerprint.clone())
        );
    }

    #[test]
    fn follow_lane_slot_pointers_resolves_vault_account_alias() {
        let followed = follow_lane_slot_pointers(vec![
            (
                "DEEPSEEK_API_KEY".to_string(),
                "deepseek-secret".to_string(),
            ),
            (
                "EXTRACT_API_KEY".to_string(),
                "vault:DEEPSEEK_API_KEY".to_string(),
            ),
        ]);
        let extract = followed
            .iter()
            .find(|(name, _)| name == "EXTRACT_API_KEY")
            .expect("slot must resolve");
        assert_eq!(extract.1, "deepseek-secret");
        assert!(followed
            .iter()
            .any(|(name, value)| name == "DEEPSEEK_API_KEY" && value == "deepseek-secret"));
    }

    #[test]
    fn follow_lane_slot_pointers_drops_slot_to_slot_and_account_aliases() {
        let followed = follow_lane_slot_pointers(vec![
            (
                "DEEPSEEK_API_KEY".to_string(),
                "vault:SILICONFLOW_API_KEY".to_string(),
            ),
            (
                "SILICONFLOW_API_KEY".to_string(),
                "siliconflow-secret".to_string(),
            ),
            (
                "EXTRACT_API_KEY".to_string(),
                "vault:SUMMARY_API_KEY".to_string(),
            ),
            (
                "SUMMARY_API_KEY".to_string(),
                "vault:SILICONFLOW_API_KEY".to_string(),
            ),
        ]);
        assert!(
            followed
                .iter()
                .all(|(name, _)| name != "EXTRACT_API_KEY" && name != "DEEPSEEK_API_KEY"),
            "{followed:?}"
        );
        assert!(followed
            .iter()
            .any(|(name, value)| name == "SILICONFLOW_API_KEY" && value == "siliconflow-secret"));
    }

    fn seed_account(store: &MemoryStore, name: &str, value: &str) {
        let (encrypted_value, nonce) =
            crypto::encrypt(&MASTER, value.as_bytes()).expect("encrypt account");
        let now = Utc::now().to_rfc3339();
        store
            .vault_upsert_entry(&VaultEntry {
                name: name.to_string(),
                encrypted_value,
                nonce,
                secret_type: SECRET_TYPE_API_KEY.to_string(),
                description: String::new(),
                allowed_agents: None,
                created_at: now.clone(),
                updated_at: now,
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("seed account");
    }

    fn decrypt_named(store: &MemoryStore, name: &str) -> String {
        let entry = store
            .vault_get_entry(name)
            .expect("get")
            .unwrap_or_else(|| panic!("{name} missing"));
        let decrypted =
            crypto::decrypt(&MASTER, &entry.encrypted_value, &entry.nonce).expect("decrypt");
        String::from_utf8(decrypted).expect("utf8")
    }

    #[test]
    fn write_lane_slot_binding_persists_metadata_on_same_pointer() {
        let mut store = MemoryStore::open_in_memory().expect("open");
        seed_account(&store, "DEEPSEEK_API_KEY", "deepseek-secret");
        write_lane_slot_binding(
            &mut store,
            &MASTER,
            "EXTRACT_API_KEY",
            "deepseek-secret",
            false,
            "first",
            None,
            None,
        )
        .expect("first bind");
        let (decided, created) = write_lane_slot_binding(
            &mut store,
            &MASTER,
            "EXTRACT_API_KEY",
            "vault:DEEPSEEK_API_KEY",
            false,
            "lane extract",
            Some(vec!["lane-bot".to_string()]),
            None,
        )
        .expect("metadata write");
        assert!(decided.noop);
        assert!(!created);
        let entry = store
            .vault_get_entry("EXTRACT_API_KEY")
            .expect("get")
            .expect("row");
        assert_eq!(entry.description, "lane extract");
        assert_eq!(
            entry.allowed_agents.as_deref(),
            Some(["lane-bot".to_string()].as_slice())
        );
        assert_eq!(
            decrypt_named(&store, "EXTRACT_API_KEY"),
            "vault:DEEPSEEK_API_KEY"
        );
    }

    #[test]
    fn write_lane_slot_binding_refuses_concurrent_first_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let db_str = db_path.to_str().expect("utf8");
        {
            let store = MemoryStore::open(db_str).expect("seed open");
            seed_account(&store, "DEEPSEEK_API_KEY", "deepseek-secret");
            seed_account(&store, "SILICONFLOW_API_KEY", "siliconflow-secret");
        }
        let mut store_a = MemoryStore::open(db_str).expect("open a");
        let mut store_b = MemoryStore::open(db_str).expect("open b");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let barrier_a = std::sync::Arc::clone(&barrier);
        let handle_a = std::thread::spawn(move || {
            barrier_a.wait();
            write_lane_slot_binding(
                &mut store_a,
                &MASTER,
                "EXTRACT_API_KEY",
                "deepseek-secret",
                false,
                "",
                None,
                None,
            )
        });
        let handle_b = std::thread::spawn(move || {
            barrier.wait();
            write_lane_slot_binding(
                &mut store_b,
                &MASTER,
                "EXTRACT_API_KEY",
                "siliconflow-secret",
                false,
                "",
                None,
                None,
            )
        });
        let result_a = handle_a.join().expect("thread a");
        let result_b = handle_b.join().expect("thread b");
        let ok_count = result_a.is_ok() as u8 + result_b.is_ok() as u8;
        assert_eq!(
            ok_count, 1,
            "exactly one first-write may commit: {result_a:?} {result_b:?}"
        );
        let err = if result_a.is_err() {
            result_a.expect_err("a")
        } else {
            result_b.expect_err("b")
        };
        assert!(
            err.contains("concurrently") || err.contains("rebind"),
            "{err}"
        );
        let store = MemoryStore::open(db_str).expect("reopen");
        let stored = decrypt_named(&store, "EXTRACT_API_KEY");
        assert!(
            stored == "vault:DEEPSEEK_API_KEY" || stored == "vault:SILICONFLOW_API_KEY",
            "{stored}"
        );
    }

    fn encrypted_entry(name: &str, value: &str) -> VaultEntry {
        let (encrypted_value, nonce) = crypto::encrypt(&MASTER, value.as_bytes()).expect("encrypt");
        let now = Utc::now().to_rfc3339();
        VaultEntry {
            name: name.to_string(),
            encrypted_value,
            nonce,
            secret_type: SECRET_TYPE_API_KEY.to_string(),
            description: String::new(),
            allowed_agents: None,
            created_at: now.clone(),
            updated_at: now,
            accessed_at: String::new(),
            access_count: 0,
        }
    }

    fn decrypt_entry(entry: &VaultEntry) -> String {
        let decrypted =
            crypto::decrypt(&MASTER, &entry.encrypted_value, &entry.nonce).expect("decrypt");
        String::from_utf8(decrypted).expect("utf8")
    }

    #[test]
    fn rewrite_imported_lane_slots_upgrades_leftover_copy_to_pointer() {
        let rewritten = rewrite_imported_lane_slots(
            &MASTER,
            &[
                encrypted_entry("DEEPSEEK_API_KEY", "deepseek-secret"),
                encrypted_entry("EXTRACT_API_KEY", "deepseek-secret"),
            ],
        )
        .expect("rewrite");
        let slot = rewritten
            .iter()
            .find(|entry| entry.name == "EXTRACT_API_KEY")
            .expect("slot");
        assert_eq!(decrypt_entry(slot), "vault:DEEPSEEK_API_KEY");
        let account = rewritten
            .iter()
            .find(|entry| entry.name == "DEEPSEEK_API_KEY")
            .expect("account");
        assert_eq!(decrypt_entry(account), "deepseek-secret");
    }

    #[test]
    fn rewrite_imported_lane_slots_refuses_unmatched_raw_bytes() {
        let err = rewrite_imported_lane_slots(
            &MASTER,
            &[
                encrypted_entry("DEEPSEEK_API_KEY", "deepseek-secret"),
                encrypted_entry("EXTRACT_API_KEY", "orphan-slot-secret"),
            ],
        )
        .expect_err("unmatched leftover copy");
        assert!(
            err.contains("second copy") || err.contains("provider account"),
            "{err}"
        );
        assert!(!err.contains("orphan-slot-secret"), "{err}");
    }

    #[test]
    fn rewrite_imported_lane_slots_keeps_existing_pointer() {
        let rewritten = rewrite_imported_lane_slots(
            &MASTER,
            &[
                encrypted_entry("DEEPSEEK_API_KEY", "deepseek-secret"),
                encrypted_entry("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY"),
            ],
        )
        .expect("keep pointer");
        let slot = rewritten
            .iter()
            .find(|entry| entry.name == "EXTRACT_API_KEY")
            .expect("slot");
        assert_eq!(decrypt_entry(slot), "vault:DEEPSEEK_API_KEY");
    }

    #[test]
    fn rewrite_imported_lane_slots_skips_unrelated_corrupt_account() {
        let mut unrelated = encrypted_entry("SILICONFLOW_API_KEY", "unrelated-secret");
        unrelated.encrypted_value = "corrupt-ciphertext".into();
        unrelated.nonce = "corrupt-nonce".into();
        let rewritten = rewrite_imported_lane_slots(
            &MASTER,
            &[
                encrypted_entry("DEEPSEEK_API_KEY", "deepseek-secret"),
                unrelated,
                encrypted_entry("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY"),
            ],
        )
        .expect("explicit imported target must ignore unrelated corruption");
        let slot = rewritten
            .iter()
            .find(|entry| entry.name == "EXTRACT_API_KEY")
            .expect("slot");
        assert_eq!(decrypt_entry(slot), "vault:DEEPSEEK_API_KEY");
    }
}
