use std::collections::{HashMap, HashSet};

use crate::provider_names::{
    parse_rotation_member_name, parse_vault_alias, validate_vault_alias_name,
};
use crate::{LlmClient, ProviderSecret};

#[derive(Debug, Clone, Default)]
pub struct MaterializeReport {
    pub loaded: usize,
    pub from_vault: usize,
    pub from_alias: usize,
    pub env_fallbacks_bypassed: usize,
    /// Env/config key names whose plaintext values were ignored because vault won.
    /// Names only — never secret values.
    pub bypassed_names: Vec<String>,
    pub skipped_aliases: Vec<(String, String)>,
}

fn flatten_pools(pools: &HashMap<String, Vec<ProviderSecret>>) -> HashMap<String, String> {
    pools
        .iter()
        .filter_map(|(name, entries)| {
            entries
                .first()
                .map(|entry| (name.clone(), entry.value.clone()))
        })
        .collect()
}

pub fn group_api_key_values_by_configured_rotations(
    values: Vec<(String, String)>,
    rotation_prefixes: &HashSet<String>,
) -> HashMap<String, Vec<ProviderSecret>> {
    values
        .into_iter()
        .fold(HashMap::new(), |mut acc, (name, value)| {
            if let Some((prefix, _)) = parse_rotation_member_name(&name) {
                if rotation_prefixes.contains(prefix) {
                    acc.entry(prefix.to_string())
                        .or_insert_with(Vec::new)
                        .push(ProviderSecret {
                            key_id: name,
                            value,
                        });
                    return acc;
                }
            }

            acc.entry(name.clone())
                .or_insert_with(Vec::new)
                .push(ProviderSecret {
                    key_id: name,
                    value,
                });
            acc
        })
}

/// Apply Vault + config.env aliases into `LlmClient` without mutating process env.
pub fn materialize_provider_secrets<I, S>(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
    provider_keys: I,
) -> Result<MaterializeReport, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    llm.clear_provider_secrets();
    let vault_map = flatten_pools(vault_pools);
    let mut resolved_pools: HashMap<String, Vec<ProviderSecret>> = vault_pools.clone();
    let mut report = MaterializeReport {
        from_vault: vault_pools.len(),
        ..Default::default()
    };

    for key in provider_keys {
        let key = key.as_ref().trim();
        if key.is_empty() {
            continue;
        }
        let key = key.to_string();
        let Ok(env_val) = std::env::var(&key) else {
            continue;
        };
        let trimmed = env_val.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(vault_name) = parse_vault_alias(trimmed) {
            // tachi#1287: syntactically-malformed alias names (space, stray
            // punctuation, oversized) are a config typo, not a "secret not yet
            // provisioned" condition — those must stay fatal instead of being
            // folded into the tolerable `skipped_aliases` degrade below.
            if let Err(reason) = validate_vault_alias_name(vault_name) {
                return Err(format!(
                    "Config key '{key}' references Vault alias '{vault_name}' which is not a valid Vault secret name: {reason}"
                ));
            }
            let Some(pool) = vault_pools.get(vault_name).cloned().or_else(|| {
                vault_map.get(vault_name).cloned().map(|secret| {
                    vec![ProviderSecret {
                        key_id: vault_name.to_string(),
                        value: secret,
                    }]
                })
            }) else {
                report.skipped_aliases.push((
                    key.clone(),
                    format!(
                        "Config key '{key}' references Vault alias '{vault_name}' but the secret is missing or Vault is locked."
                    ),
                ));
                continue;
            };
            resolved_pools.insert(key.clone(), pool);
            report.from_alias += 1;
            report.env_fallbacks_bypassed += 1;
            report.bypassed_names.push(key.clone());
            continue;
        }

        if vault_map.contains_key(&key) {
            // Vault wins over duplicate plaintext in env/config.env.
            report.env_fallbacks_bypassed += 1;
            report.bypassed_names.push(key.clone());
            continue;
        }

        resolved_pools.insert(
            key.clone(),
            vec![ProviderSecret {
                key_id: key,
                value: trimmed.to_string(),
            }],
        );
    }

    report.loaded = llm.set_provider_secret_pools(resolved_pools);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn keychain_loader_only_groups_configured_rotation_members() {
        let mut rotations = HashSet::new();
        rotations.insert("VOYAGE_API_KEY".to_string());

        let grouped = group_api_key_values_by_configured_rotations(
            vec![
                ("VOYAGE_API_KEY_1".to_string(), "voyage-a".to_string()),
                ("VOYAGE_API_KEY_2".to_string(), "voyage-b".to_string()),
                ("SOME_API_KEY_2".to_string(), "standalone".to_string()),
            ],
            &rotations,
        );

        let voyage = grouped
            .get("VOYAGE_API_KEY")
            .expect("configured rotation members should be grouped");
        assert_eq!(voyage.len(), 2);
        assert_eq!(voyage[0].key_id, "VOYAGE_API_KEY_1");
        assert_eq!(voyage[1].key_id, "VOYAGE_API_KEY_2");
        assert!(!grouped.contains_key("SOME_API_KEY"));
        assert_eq!(
            grouped
                .get("SOME_API_KEY_2")
                .and_then(|entries| entries.first())
                .map(|entry| entry.value.as_str()),
            Some("standalone")
        );
    }

    #[test]
    fn materialize_provider_secrets_preserves_vault_alias_env() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("VOYAGE_API_KEY", "vault:VOYAGE_API_KEY");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "VOYAGE_API_KEY".to_string(),
                value: "vault-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools, ["VOYAGE_API_KEY"])
            .expect("materialize");

        assert_eq!(report.from_alias, 1);
        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert_eq!(report.bypassed_names, vec!["VOYAGE_API_KEY".to_string()]);
        assert_eq!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(
            std::env::var("VOYAGE_API_KEY").as_deref(),
            Ok("vault:VOYAGE_API_KEY")
        );
    }

    #[test]
    fn materialize_provider_secrets_preserves_duplicate_plaintext_env_when_vault_wins() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("OPENAI_API_KEY", "env-secret");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "OPENAI_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "OPENAI_API_KEY".to_string(),
                value: "vault-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools, ["OPENAI_API_KEY"])
            .expect("materialize");

        assert_eq!(report.from_alias, 0);
        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert_eq!(report.bypassed_names, vec!["OPENAI_API_KEY".to_string()]);
        assert_eq!(
            llm.provider_secret_for_tests(&["OPENAI_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(std::env::var("OPENAI_API_KEY").as_deref(), Ok("env-secret"));
    }

    // Semantics changed (tachi#1279): a single missing Vault alias used to abort the
    // entire batch with an `Err`. It now degrades to a per-alias skip recorded in
    // `report.skipped_aliases`, so the rest of the provider keys still materialize.
    #[test]
    fn materialize_provider_secrets_skips_missing_alias_without_aborting() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("TACHI_TEST_PROVIDER_ALIAS_KEY", "vault:MISSING_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        let report =
            materialize_provider_secrets(&llm, &HashMap::new(), ["TACHI_TEST_PROVIDER_ALIAS_KEY"])
                .expect("missing alias should no longer abort the batch");

        assert_eq!(report.skipped_aliases.len(), 1);
        let (key, reason) = &report.skipped_aliases[0];
        assert_eq!(key, "TACHI_TEST_PROVIDER_ALIAS_KEY");
        assert!(reason.contains(
            "Config key 'TACHI_TEST_PROVIDER_ALIAS_KEY' references Vault alias 'MISSING_ALIAS'"
        ));
        assert!(!reason.contains("TACHI_TEST_PROVIDER_ALIAS_KEY=vault:MISSING_ALIAS"));
        assert!(!reason.contains("config.env"));
        assert!(!reason.contains("vault_unlock"));
        assert!(!reason.contains("vault_set"));
        assert!(llm
            .provider_secret_for_tests(&["TACHI_TEST_PROVIDER_ALIAS_KEY"])
            .is_none());
    }

    // Discrimination test for tachi#1279: on the old batch-abort implementation, the
    // single missing alias below would short-circuit with `Err` before the loop ever
    // reached the two good aliases, so `report.from_alias` would never be observed and
    // this test fails at the `expect("materialize")` call (RED on old code). On the
    // fixed per-alias-tolerant implementation, the bad alias is recorded in
    // `skipped_aliases` and the two good aliases still materialize (GREEN).
    #[test]
    fn materialize_provider_secrets_isolates_one_bad_alias_from_good_ones() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env_good_1 = EnvGuard::set("TACHI_TEST_GOOD_ALIAS_KEY_1", "vault:GOOD_ALIAS_1");
        let _env_good_2 = EnvGuard::set("TACHI_TEST_GOOD_ALIAS_KEY_2", "vault:GOOD_ALIAS_2");
        let _env_bad = EnvGuard::set("TACHI_TEST_BAD_ALIAS_KEY", "vault:MISSING_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([
            (
                "GOOD_ALIAS_1".to_string(),
                vec![ProviderSecret {
                    key_id: "GOOD_ALIAS_1".to_string(),
                    value: "good-secret-1".to_string(),
                }],
            ),
            (
                "GOOD_ALIAS_2".to_string(),
                vec![ProviderSecret {
                    key_id: "GOOD_ALIAS_2".to_string(),
                    value: "good-secret-2".to_string(),
                }],
            ),
        ]);

        let report = materialize_provider_secrets(
            &llm,
            &vault_pools,
            [
                "TACHI_TEST_GOOD_ALIAS_KEY_1",
                "TACHI_TEST_GOOD_ALIAS_KEY_2",
                "TACHI_TEST_BAD_ALIAS_KEY",
            ],
        )
        .expect("one bad alias must not abort the whole batch");

        assert!(report.from_alias >= 2);
        assert_eq!(
            llm.provider_secret_for_tests(&["TACHI_TEST_GOOD_ALIAS_KEY_1"])
                .as_deref(),
            Some("good-secret-1")
        );
        assert_eq!(
            llm.provider_secret_for_tests(&["TACHI_TEST_GOOD_ALIAS_KEY_2"])
                .as_deref(),
            Some("good-secret-2")
        );

        assert!(report
            .skipped_aliases
            .iter()
            .any(|(key, _reason)| key == "TACHI_TEST_BAD_ALIAS_KEY"));
    }

    // tachi#1287 fix 2: a missing-but-syntactically-valid alias is a tolerable
    // skip (covered above); a syntactically malformed alias name (space here)
    // is a config typo and must stay fatal, not be folded into
    // `skipped_aliases`.
    #[test]
    fn materialize_provider_secrets_rejects_malformed_alias_name() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set(
            "TACHI_TEST_MALFORMED_ALIAS_KEY",
            "vault:invalid key with spaces",
        );
        let llm = LlmClient::new().expect("llm client");

        let err =
            materialize_provider_secrets(&llm, &HashMap::new(), ["TACHI_TEST_MALFORMED_ALIAS_KEY"])
                .expect_err("malformed alias name must fail loudly, not degrade to a skip");

        assert!(err.contains("TACHI_TEST_MALFORMED_ALIAS_KEY"));
        assert!(err.contains("not a valid Vault secret name"));
    }

    #[test]
    fn materialize_provider_secrets_rejects_oversized_alias_name() {
        let _guard = crate::test_support::global_test_lock().lock();
        let oversized_name = "A".repeat(129);
        let _env = EnvGuard::set(
            "TACHI_TEST_OVERSIZED_ALIAS_KEY",
            &format!("vault:{oversized_name}"),
        );
        let llm = LlmClient::new().expect("llm client");

        let err =
            materialize_provider_secrets(&llm, &HashMap::new(), ["TACHI_TEST_OVERSIZED_ALIAS_KEY"])
                .expect_err("oversized alias name must fail loudly, not degrade to a skip");

        assert!(err.contains("not a valid Vault secret name"));
    }
}
