use std::collections::{HashMap, HashSet};

use crate::provider_names::{parse_rotation_member_name, parse_vault_alias};
use crate::{LlmClient, ProviderSecret};

#[derive(Debug, Clone, Default)]
pub struct MaterializeReport {
    pub loaded: usize,
    pub from_vault: usize,
    pub from_alias: usize,
    pub env_fallbacks_bypassed: usize,
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
            let pool = vault_pools
                .get(vault_name)
                .cloned()
                .or_else(|| {
                    vault_map.get(vault_name).cloned().map(|secret| {
                        vec![ProviderSecret {
                            key_id: vault_name.to_string(),
                            value: secret,
                        }]
                    })
                })
                .ok_or_else(|| {
                    format!(
                        "provider alias {key}={trimmed} could not be resolved from secret '{vault_name}'"
                    )
                })?;
            resolved_pools.insert(key.clone(), pool);
            report.from_alias += 1;
            report.env_fallbacks_bypassed += 1;
            continue;
        }

        if vault_map.contains_key(&key) {
            // Vault wins over duplicate plaintext in env/config.env.
            report.env_fallbacks_bypassed += 1;
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
        let _guard = crate::test_support::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _guard = crate::test_support::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        assert_eq!(
            llm.provider_secret_for_tests(&["OPENAI_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(std::env::var("OPENAI_API_KEY").as_deref(), Ok("env-secret"));
    }

    #[test]
    fn materialize_provider_secrets_reports_neutral_missing_alias() {
        let _guard = crate::test_support::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvGuard::set("TACHI_TEST_PROVIDER_ALIAS_KEY", "vault:MISSING_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        let err =
            materialize_provider_secrets(&llm, &HashMap::new(), ["TACHI_TEST_PROVIDER_ALIAS_KEY"])
                .expect_err("missing alias should fail");

        assert!(err.contains(
            "provider alias TACHI_TEST_PROVIDER_ALIAS_KEY=vault:MISSING_ALIAS could not be resolved"
        ));
        assert!(!err.contains("config.env"));
        assert!(!err.contains("vault_unlock"));
        assert!(!err.contains("vault_set"));
    }
}
