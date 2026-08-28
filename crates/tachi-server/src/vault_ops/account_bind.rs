//! Lane slots bind to provider accounts. They must not store a second copy
//! of account ciphertext (tachi#1855 redesign).
//!
//! `EXTRACT_API_KEY` / `SUMMARY_API_KEY` / `DISTILL_API_KEY` /
//! `REASONING_API_KEY` are slots. `vault set SLOT <bytes>` either writes
//! `vault:ACCOUNT` or refuses. `--rebind` changes the pointer, never copies
//! a new family into the slot row. Account names still rotate in place.

use crate::vault_crypto as crypto;
use chrono::Utc;
use memcore::vault::fingerprint::FingerprintKey;
use memcore::vault::{VaultEntry, VaultKeyHealth, SECRET_TYPE_API_KEY};
use memcore::MemoryStore;
use tachi_llm::parse_vault_alias;

pub(crate) const LANE_SLOT_SECRET_NAMES: &[&str] = &[
    "EXTRACT_API_KEY",
    "SUMMARY_API_KEY",
    "DISTILL_API_KEY",
    "REASONING_API_KEY",
];

pub(crate) fn is_lane_slot_secret_name(name: &str) -> bool {
    LANE_SLOT_SECRET_NAMES.contains(&name.trim())
}

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
                        "Lane slot '{slot}' points at a restricted account '{}'",
                        target.name
                    ));
                }
                Some(agent) if !allowed.iter().any(|allowed_agent| allowed_agent == agent) => {
                    return Err(format!(
                        "Lane slot '{slot}' points at an account '{}' this agent cannot use",
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

pub(crate) fn refuse_lane_slot_pool_prefix(prefix: &str) -> Result<(), String> {
    if is_lane_slot_secret_name(prefix) {
        Err(format!(
            "Lane slot '{prefix}' binds to an account; it cannot be an API-key rotation pool"
        ))
    } else {
        Ok(())
    }
}

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
    let mut slot_plain = Vec::new();
    let mut account_rows = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        let decrypted = crypto::decrypt(master_key, &entry.encrypted_value, &entry.nonce)
            .map_err(|e| format!("decrypt imported {}: {e}", entry.name))?;
        let value = String::from_utf8(decrypted).map_err(|e| {
            format!(
                "Imported vault secret '{}' is not valid UTF-8: {e}",
                entry.name
            )
        })?;
        if is_lane_slot_secret_name(&entry.name) {
            slot_plain.push((idx, value));
        } else {
            account_rows.push((entry.name.clone(), value, entry.secret_type.clone()));
        }
    }
    let accounts = bindable_accounts(account_rows);
    let mut out = entries.to_vec();
    for (idx, plain) in slot_plain {
        let decided = decide_lane_slot_write(
            master_key,
            &out[idx].name,
            &plain,
            Some(plain.as_str()),
            &accounts,
            false,
        )?;
        if decided.store_value != plain {
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
) -> Result<(LaneSlotDecision, bool), String> {
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let mut existing_plain = None;
    let mut expected_cipher: Option<(String, String)> = None;
    let mut account_rows = Vec::new();
    for entry in entries {
        let decrypted = crypto::decrypt(master_key, &entry.encrypted_value, &entry.nonce)
            .map_err(|e| format!("decrypt {}: {e}", entry.name))?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
        if entry.name == name {
            expected_cipher = Some((entry.encrypted_value, entry.nonce));
            existing_plain = Some(value);
        } else {
            account_rows.push((entry.name, value, entry.secret_type));
        }
    }
    let accounts = bindable_accounts(account_rows);
    let decided = decide_lane_slot_write(
        master_key,
        name,
        new_value,
        existing_plain.as_deref(),
        &accounts,
        rebind,
    )?;
    let (encrypted_value, nonce) = crypto::encrypt(master_key, decided.store_value.as_bytes())
        .map_err(|e| format!("encrypt slot pointer: {e}"))?;
    let created = expected_cipher.is_none();
    let now = Utc::now().to_rfc3339();
    let entry = VaultEntry {
        name: name.to_string(),
        encrypted_value,
        nonce,
        secret_type: SECRET_TYPE_API_KEY.to_string(),
        description: description.to_string(),
        allowed_agents,
        created_at: if created { now.clone() } else { String::new() },
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };
    let expected = expected_cipher
        .as_ref()
        .map(|(encrypted, nonce)| (encrypted.as_str(), nonce.as_str()));
    let wrote = store
        .vault_cas_upsert_entry(&entry, expected)
        .map_err(|e| format!("vault_cas_upsert_entry: {e}"))?;
    if !wrote {
        return Err(format!(
            "Lane slot '{name}' changed concurrently; pass rebind=true / --rebind to change account family"
        ));
    }
    Ok((decided, created))
}

pub(crate) fn bindable_accounts(
    rows: impl IntoIterator<Item = (String, String, String)>,
) -> Vec<(String, String, &'static str)> {
    rows.into_iter()
        .filter_map(|(name, value, secret_type)| {
            if is_lane_slot_secret_name(&name) {
                return None;
            }
            if parse_vault_alias(&value).is_some() {
                return None;
            }
            if memcore::effective_vault_secret_type(&name, &secret_type) != SECRET_TYPE_API_KEY {
                return None;
            }
            let kind = crate::status_ops::status_health::provider_kind_for_env_name(&name)?;
            Some((name, value, kind))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneSlotDecision {
    /// Value stored on the slot row: `vault:ACCOUNT`, never raw key bytes.
    pub store_value: String,
    pub account: String,
    pub fingerprint: String,
    pub rebound: bool,
    pub noop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccountMatch {
    name: String,
    fingerprint: String,
}

fn fingerprint_secret(master_key: &[u8; 32], provider_kind: &str, value: &str) -> String {
    FingerprintKey::derive_from_master_key(master_key).key_fingerprint(provider_kind, value)
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
                 Store the key on a provider account (for example DEEPSEEK_API_KEY) \
                 then bind with {slot}=vault:ACCOUNT."
            )
        })?
    };

    let pointer = format!("vault:{}", target.name);
    let current = current_pointer(existing_slot_value);

    if current == Some(target.name.as_str()) {
        return Ok(LaneSlotDecision {
            store_value: pointer,
            account: target.name,
            fingerprint: target.fingerprint,
            rebound: false,
            noop: true,
        });
    }

    let leftover_same_bytes = existing_slot_value.is_some()
        && current.is_none()
        && existing_slot_value.is_some_and(|value| value.trim() == new_value);
    let is_first_write = existing_slot_value.is_none();
    if is_first_write || leftover_same_bytes {
        return Ok(LaneSlotDecision {
            store_value: pointer,
            account: target.name,
            fingerprint: target.fingerprint,
            rebound: false,
            noop: false,
        });
    }

    if !rebind {
        let old = current
            .map(|name| format!("vault:{name}"))
            .unwrap_or_else(|| "a leftover ciphertext row".to_string());
        return Err(format!(
            "Lane slot '{slot}' is bound to {old}; new binding is vault:{} ({}). \
             Pass rebind=true / --rebind to change account family. \
             This is not a ciphertext overwrite.",
            target.name, target.fingerprint
        ));
    }

    Ok(LaneSlotDecision {
        store_value: pointer,
        account: target.name,
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
}
