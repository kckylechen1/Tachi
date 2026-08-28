//! Lane slots bind to provider accounts. They must not store a second copy
//! of account ciphertext (tachi#1855 redesign).
//!
//! `EXTRACT_API_KEY` / `SUMMARY_API_KEY` / `DISTILL_API_KEY` /
//! `REASONING_API_KEY` are slots. `vault set SLOT <bytes>` either writes
//! `vault:ACCOUNT` or refuses. `--rebind` changes the pointer, never copies
//! a new family into the slot row. Account names still rotate in place.

use memcore::vault::fingerprint::FingerprintKey;
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

pub(crate) fn bindable_accounts(
    rows: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String, &'static str)> {
    rows.into_iter()
        .filter_map(|(name, value)| {
            if is_lane_slot_secret_name(&name) {
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
        let Some((_, _, kind)) = accounts.iter().find(|(name, _, _)| name == account) else {
            return Err(format!(
                "Lane slot '{slot}' cannot bind to missing account '{account}'"
            ));
        };
        let fp = accounts
            .iter()
            .find(|(name, _, _)| name == account)
            .and_then(|(_, value, _)| {
                if parse_vault_alias(value).is_some() {
                    None
                } else {
                    Some(fingerprint_secret(master_key, kind, value))
                }
            })
            .unwrap_or_else(|| "fp1:unresolved".to_string());
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
}
