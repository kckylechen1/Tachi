//! Read-only provider-doctor evidence for account-backed lane slots.
//!
//! The lookup key remains the exact env-ref name. A valid slot is represented
//! in the report as that exact key with the pointed-to account's metadata and
//! value held only in memory. The target name, pointer, and plaintext never
//! become report output.

use super::ProviderVaultReportEvidence;
use memcore::vault::{VaultEntry, VaultKeyHealth, SECRET_TYPE_API_KEY};
use std::collections::{HashMap, HashSet};

pub(super) fn collect_lane_slot_report_evidence(
    entries: &[VaultEntry],
    health_by_identity: &HashMap<(String, String), VaultKeyHealth>,
    names: &HashSet<String>,
    key: Option<&[u8; 32]>,
    evidence: &mut ProviderVaultReportEvidence,
) {
    let entries_by_name: HashMap<&str, &VaultEntry> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();

    for slot in entries.iter().filter(|entry| {
        names.contains(&entry.name)
            && crate::vault_ops::is_lane_slot_secret_name(&entry.name)
            && memcore::effective_vault_secret_type(&entry.name, &entry.secret_type)
                == SECRET_TYPE_API_KEY
            && entry
                .allowed_agents
                .as_ref()
                .is_none_or(|agents| agents.is_empty())
    }) {
        let Some(key) = key else {
            record_metadata(evidence, slot);
            continue;
        };

        let Some(mut slot_value) = decrypt_value(slot, key) else {
            record_metadata(evidence, slot);
            continue;
        };
        let Some(target_name) = tachi_llm::parse_vault_alias(&slot_value).map(str::to_string)
        else {
            record_metadata(evidence, slot);
            continue;
        };
        crate::vault_crypto::zero_string(slot_value.as_mut_string());

        let Some(target) = entries_by_name.get(target_name.as_str()).copied() else {
            record_metadata(evidence, slot);
            continue;
        };
        let Some(mut target_value) = decrypt_value(target, key) else {
            record_metadata(evidence, slot);
            continue;
        };

        let slot_health = health_by_identity.get(&(slot.name.clone(), target.name.clone()));
        let target_health = health_by_identity.get(&(target.name.clone(), target.name.clone()));
        let health_unusable =
            crate::vault_ops::account_bind::slot_target_health_unusable(slot_health, target_health);
        if crate::vault_ops::account_bind::refuse_unusable_account_target(
            &slot.name,
            target,
            &target_value,
            None,
            health_unusable,
        )
        .is_err()
        {
            record_metadata(evidence, slot);
            continue;
        }

        let target_value = std::mem::take(target_value.as_mut_string());
        record_metadata_at(evidence, &slot.name, &target.updated_at);
        insert_value(evidence, &slot.name, target_value);
    }
}

fn decrypt_value(
    entry: &VaultEntry,
    key: &[u8; 32],
) -> Option<crate::vault_crypto::ZeroizingString> {
    let decrypted = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce).ok()?;
    let value = crate::vault_crypto::decode_utf8_zeroizing(
        decrypted,
        "provider doctor lane-slot value is not valid UTF-8",
    )
    .ok()?;
    Some(crate::vault_crypto::ZeroizingString::new(value))
}

fn record_metadata(evidence: &mut ProviderVaultReportEvidence, slot: &VaultEntry) {
    record_metadata_at(evidence, &slot.name, &slot.updated_at);
}

fn record_metadata_at(evidence: &mut ProviderVaultReportEvidence, name: &str, updated_at: &str) {
    evidence
        .exact_updated_at
        .insert(name.to_string(), updated_at.to_string());
}

fn insert_value(evidence: &mut ProviderVaultReportEvidence, name: &str, value: String) {
    if let Some(mut previous) = evidence.exact_values.insert(name.to_string(), value) {
        crate::vault_crypto::zero_string(&mut previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::vault_cli::providers_doctor::{
        build_provider_rows_with_evidence, render_provider_report, ApiKeyShape, ProviderVaultAccess,
    };
    use chrono::Utc;
    use memcore::vault::{VaultEntry, VaultKeyHealth};
    use memcore::MemoryStore;
    use std::collections::BTreeMap;

    fn entry(
        key: &[u8; 32],
        name: &str,
        value: &str,
        allowed_agents: Option<Vec<String>>,
    ) -> VaultEntry {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key, value.as_bytes()).expect("encrypt fixture");
        VaultEntry {
            name: name.to_string(),
            encrypted_value,
            nonce,
            secret_type: SECRET_TYPE_API_KEY.to_string(),
            description: "doctor slot fixture".to_string(),
            allowed_agents,
            created_at: "2026-08-01T00:00:00Z".to_string(),
            updated_at: "2026-08-02T00:00:00Z".to_string(),
            accessed_at: "2026-08-01T00:00:00Z".to_string(),
            access_count: 0,
        }
    }

    #[test]
    fn account_backed_slot_report_uses_exact_slot_metadata_and_never_leaks_material() {
        let slot_secret = "legacy-slot-secret-MUST-NOT-LEAK";
        let account_secret = "deepseek-account-secret-MUST-NOT-LEAK";
        let restricted_secret = "restricted-account-secret-MUST-NOT-LEAK";
        let invalid_secret = "invalid-account-secret-MUST-NOT-LEAK";
        let health_secret = "health-account-secret-MUST-NOT-LEAK";
        let key = [7_u8; 32];
        let entries = vec![
            entry(&key, "EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY", None),
            entry(&key, "DEEPSEEK_API_KEY", account_secret, None),
            entry(&key, "SUMMARY_API_KEY", "vault:VOYAGE_API_KEY", None),
            entry(
                &key,
                "VOYAGE_API_KEY",
                restricted_secret,
                Some(vec!["other-agent".to_string()]),
            ),
            entry(&key, "DISTILL_API_KEY", "vault:SILICONFLOW_API_KEY", None),
            {
                let mut invalid = entry(&key, "SILICONFLOW_API_KEY", invalid_secret, None);
                invalid.secret_type = "password".to_string();
                invalid
            },
            entry(&key, "REASONING_API_KEY", "vault:OPENAI_API_KEY", None),
            entry(&key, "OPENAI_API_KEY", health_secret, None),
        ];
        let health = VaultKeyHealth {
            logical_name: "REASONING_API_KEY".to_string(),
            key_id: "OPENAI_API_KEY".to_string(),
            disabled: true,
            ..VaultKeyHealth::default()
        };
        let names = HashSet::from([
            "EXTRACT_API_KEY".to_string(),
            "SUMMARY_API_KEY".to_string(),
            "DISTILL_API_KEY".to_string(),
            "REASONING_API_KEY".to_string(),
        ]);
        let dir = tempfile::tempdir().expect("fixture tempdir");
        let store = MemoryStore::open(dir.path().join("memory.db").to_str().unwrap())
            .expect("open fixture store");
        for entry in &entries {
            store.vault_upsert_entry(entry).expect("seed fixture entry");
        }
        store
            .vault_upsert_key_health(&health)
            .expect("seed fixture health");
        let evidence =
            super::super::collect_provider_vault_evidence(&store, Some(&key), &names, Utc::now())
                .expect("collect production doctor evidence");

        assert_eq!(
            evidence.exact_values.get("EXTRACT_API_KEY"),
            Some(&account_secret.to_string())
        );
        assert!(evidence.exact_updated_at.contains_key("EXTRACT_API_KEY"));
        assert!(!evidence.exact_values.contains_key("SUMMARY_API_KEY"));
        assert!(!evidence.exact_values.contains_key("DISTILL_API_KEY"));
        assert!(!evidence.exact_values.contains_key("REASONING_API_KEY"));
        for slot_name in ["SUMMARY_API_KEY", "DISTILL_API_KEY", "REASONING_API_KEY"] {
            assert!(evidence.exact_updated_at.contains_key(slot_name));
        }

        let providers = BTreeMap::from([
            (
                "valid".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:EXTRACT_API_KEY}"}}),
            ),
            (
                "restricted".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:SUMMARY_API_KEY}"}}),
            ),
            (
                "invalid".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:DISTILL_API_KEY}"}}),
            ),
            (
                "health".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:REASONING_API_KEY}"}}),
            ),
        ]);
        let env_values =
            HashMap::from([("EXTRACT_API_KEY".to_string(), account_secret.to_string())]);
        let rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::new(),
            &env_values,
            &evidence,
            ProviderVaultAccess::Unlocked,
            Utc::now(),
        )
        .expect("build doctor rows");
        let rendered = render_provider_report(&rows);
        let valid = rows
            .iter()
            .find(|row| row.provider == "valid")
            .expect("valid doctor row");
        assert_eq!(
            valid.shape,
            ApiKeyShape::EnvRef {
                name: "EXTRACT_API_KEY".to_string()
            }
        );
        assert_eq!(valid.value_match, "MATCH");
        assert_eq!(valid.vault, Some(true));
        assert!(rendered.contains("valid"));
        for forbidden in [
            slot_secret,
            account_secret,
            restricted_secret,
            invalid_secret,
            health_secret,
            "DEEPSEEK_API_KEY",
            "VOYAGE_API_KEY",
            "SILICONFLOW_API_KEY",
            "OPENAI_API_KEY",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "doctor report leaked {forbidden}"
            );
        }

        let locked_evidence =
            super::super::collect_provider_vault_evidence(&store, None, &names, Utc::now())
                .expect("collect locked doctor evidence");
        assert!(locked_evidence.exact_values.is_empty());
        assert!(locked_evidence
            .exact_updated_at
            .contains_key("EXTRACT_API_KEY"));
        let locked_rows = build_provider_rows_with_evidence(
            &BTreeMap::from([(
                "locked".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:EXTRACT_API_KEY}"}}),
            )]),
            &HashSet::new(),
            &HashMap::new(),
            &locked_evidence,
            ProviderVaultAccess::LockedOrUnavailable,
            Utc::now(),
        )
        .expect("build locked doctor row");
        assert_eq!(locked_rows[0].vault, Some(true));
        assert!(locked_rows[0].value_match.starts_with("UNKNOWN("));
        let locked_rendered = render_provider_report(&locked_rows);
        for forbidden in [account_secret, "DEEPSEEK_API_KEY"] {
            assert!(!locked_rendered.contains(forbidden));
        }

        let legacy_secret = "raw-legacy-slot-secret-MUST-NOT-LEAK";
        let legacy_dir = tempfile::tempdir().expect("legacy fixture tempdir");
        let legacy_store = MemoryStore::open(legacy_dir.path().join("memory.db").to_str().unwrap())
            .expect("open legacy fixture store");
        legacy_store
            .vault_upsert_entry(&entry(&key, "EXTRACT_API_KEY", legacy_secret, None))
            .expect("seed raw legacy slot");
        let legacy_evidence = super::super::collect_provider_vault_evidence(
            &legacy_store,
            Some(&key),
            &HashSet::from(["EXTRACT_API_KEY".to_string()]),
            Utc::now(),
        )
        .expect("collect legacy doctor evidence");
        assert!(legacy_evidence
            .exact_updated_at
            .contains_key("EXTRACT_API_KEY"));
        assert!(!legacy_evidence.exact_values.contains_key("EXTRACT_API_KEY"));
        let legacy_rows = build_provider_rows_with_evidence(
            &BTreeMap::from([(
                "legacy".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:EXTRACT_API_KEY}"}}),
            )]),
            &HashSet::new(),
            &HashMap::new(),
            &legacy_evidence,
            ProviderVaultAccess::Unlocked,
            Utc::now(),
        )
        .expect("build legacy doctor row");
        assert_eq!(legacy_rows[0].vault, Some(true));
        assert!(legacy_rows[0].value_match.starts_with("UNKNOWN("));
        let legacy_rendered = render_provider_report(&legacy_rows);
        assert!(!legacy_rendered.contains(legacy_secret));
    }
}
