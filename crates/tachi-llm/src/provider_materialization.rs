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
    /// Env/config key names whose different plaintext values were ignored because
    /// Vault won. Names only — never secret values.
    pub bypassed_names: Vec<String>,
    pub skipped_aliases: Vec<(String, String)>,
    /// Logical config key names whose missing/locked alias kept an existing
    /// provider pool. Names only; retained pools are included in `loaded`.
    pub retained_from_last_known_good: Vec<String>,
    /// Whether the durable source backing this refresh was readable. Operator
    /// output derives its wording from this instead of re-deriving intent from
    /// a message string.
    pub source_availability: VaultSourceAvailability,
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

/// Whether the durable Vault source could actually be read for this refresh.
///
/// The missing-alias branch cannot tell "the aliased secret was deleted" from
/// "the Vault is locked" on its own — both arrive as the same lookup miss. Only
/// the caller that opened the source knows which, and the two must NOT degrade
/// the same way:
///
/// * a locked Vault is transient, and dropping the pool would take providers
///   down until the next unlock, so the last-known-good pool is retained;
/// * a deleted secret is a revocation, and retaining it would keep serving a
///   credential the operator removed — for as long as the process lives.
///
/// tachi#1393 originally retained on both. Retaining on `Readable` silently
/// reverses main's `c90143881` ("Remove stale pools when Vault aliases are
/// absent"), which neither side's tests catch, because their fixtures start
/// with an empty provider cache and so never reach the retention path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VaultSourceAvailability {
    /// The Vault secret set was read. A missing alias target means the secret
    /// is genuinely absent: drop the pool.
    Readable,
    /// The Vault could not be read (locked, auto-locked, uninitialized). A
    /// missing alias target proves nothing about the secret: retain LKG.
    ///
    /// Also the `Default`: an unproven source must never license retention by
    /// omission, and every constructor that forgets to set this lands here.
    #[default]
    LockedOrUnavailable,
}

/// Apply caller-owned, already-resolved pools plus config.env aliases into
/// `LlmClient` without mutating process env.
///
/// Durable Vault/Keychain readers must use
/// [`materialize_provider_secrets_from_durable_source`] so source loading is
/// covered by the same transaction as cache replacement.
///
/// Pre-resolved pools carry no [`VaultSourceAvailability`] signal, so this entry
/// assumes [`VaultSourceAvailability::LockedOrUnavailable`] and retains
/// last-known-good pools. That assumption is only safe for callers that are not
/// deciding revocation, so the entry is compiled for tests only — production
/// must go through the durable-source entry and pass the real availability.
#[cfg(any(test, feature = "test-support"))]
pub fn materialize_provider_secrets<I, S>(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
    provider_keys: I,
) -> Result<MaterializeReport, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    materialize_provider_secrets_inner(
        llm,
        vault_pools,
        provider_keys,
        VaultSourceAvailability::LockedOrUnavailable,
        None,
    )
}

/// Materialize provider secrets while holding one transaction boundary across
/// durable source loading and provider-cache replacement.
///
/// `load_vault_pools` runs after the transaction lock is acquired. Callers that
/// decrypt Vault/Keychain material or update access accounting must use this
/// entry point so an explicit cache clear cannot be overtaken by pre-resolved
/// secret pools.
pub fn materialize_provider_secrets_from_durable_source<I, S, F>(
    llm: &LlmClient,
    provider_keys: I,
    load_vault_pools: F,
) -> Result<MaterializeReport, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    F: FnOnce() -> Result<
        (
            HashMap<String, Vec<ProviderSecret>>,
            VaultSourceAvailability,
        ),
        String,
    >,
{
    let _materialization_guard = llm.provider_materialization_guard()?;
    let (vault_pools, availability) = load_vault_pools()?;
    materialize_provider_secrets_under_guard(llm, &vault_pools, provider_keys, availability, None)
}

fn materialize_provider_secrets_inner<I, S>(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
    provider_keys: I,
    availability: VaultSourceAvailability,
    after_missing_alias_snapshot: Option<Box<dyn FnOnce() + Send>>,
) -> Result<MaterializeReport, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    // Snapshot, env/alias resolution, and the final atomic map replacement are
    // one transaction across every LlmClient clone. This mutex is independent
    // from provider_state, so env/Vault work never nests under its lock.
    let _materialization_guard = llm.provider_materialization_guard()?;
    materialize_provider_secrets_under_guard(
        llm,
        vault_pools,
        provider_keys,
        availability,
        after_missing_alias_snapshot,
    )
}

fn materialize_provider_secrets_under_guard<I, S>(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
    provider_keys: I,
    availability: VaultSourceAvailability,
    mut after_missing_alias_snapshot: Option<Box<dyn FnOnce() + Send>>,
) -> Result<MaterializeReport, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let vault_map = flatten_pools(vault_pools);
    let mut resolved_pools: HashMap<String, Vec<ProviderSecret>> = vault_pools.clone();
    let mut report = MaterializeReport {
        from_vault: vault_pools.len(),
        source_availability: availability,
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
            // An explicit alias is the configured source for this logical key.
            // Remove any same-key canonical Vault pool before resolving it so a
            // missing target cannot silently fall back to a different secret.
            resolved_pools.remove(&key);
            // tachi#1287: syntactically-malformed alias names (space, stray
            // punctuation, oversized) are a config typo, not a "secret not yet
            // provisioned" condition — those must stay fatal instead of being
            // folded into the tolerable `skipped_aliases` degrade below.
            if validate_vault_alias_name(vault_name).is_err() {
                return Err(format!(
                    "Config key '{key}' references a Vault alias that is not a valid Vault secret name; provider refresh refused and prior provider cache left unchanged"
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
                resolved_pools.remove(&key);
                report.skipped_aliases.push((
                    key.clone(),
                    match availability {
                        VaultSourceAvailability::Readable => format!(
                            "Config key '{key}' references a Vault alias whose secret is absent from a readable Vault."
                        ),
                        VaultSourceAvailability::LockedOrUnavailable => format!(
                            "Config key '{key}' references a Vault alias, but the Vault could not be read."
                        ),
                    },
                ));
                // Retention is a statement about the SOURCE, not about the key.
                // A readable Vault that does not contain the alias target has
                // answered the question: the secret is gone, and continuing to
                // serve the cached copy would defeat its revocation.
                let retained_pool = match availability {
                    VaultSourceAvailability::LockedOrUnavailable => {
                        llm.provider_secret_pool_snapshot(&key)
                    }
                    VaultSourceAvailability::Readable => None,
                };
                if let Some(hook) = after_missing_alias_snapshot.take() {
                    hook();
                }
                if let Some(pool) = retained_pool {
                    resolved_pools.insert(key.clone(), pool);
                    report.retained_from_last_known_good.push(key);
                }
                continue;
            };
            resolved_pools.insert(key.clone(), pool);
            report.from_alias += 1;
            report.env_fallbacks_bypassed += 1;
            continue;
        }

        if let Some(vault_value) = vault_map.get(&key) {
            // Vault wins over duplicate plaintext in env/config.env.
            report.env_fallbacks_bypassed += 1;
            // The report is logged by daemon callers, so retain only an
            // actionable key name and never retain either plaintext value.
            if vault_value != trimmed {
                report.bypassed_names.push(key.clone());
            }
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

    let retained_logical_names = report
        .retained_from_last_known_good
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    report.loaded = llm.replace_provider_secret_pools(resolved_pools, &retained_logical_names)?;
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

        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            std::env::remove_var(key);
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
        assert!(
            report.bypassed_names.is_empty(),
            "a vault: alias is an explicit Vault reference, not a conflicting plaintext value"
        );
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

    /// A duplicate with the same value is not a custody conflict. The daemon
    /// still prefers the Vault copy, but must not send an operator a false
    /// "value ignored" warning.
    #[test]
    fn materialize_provider_secrets_does_not_report_same_value_as_bypassed() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("TACHI_TEST_SAME_VALUE_API_KEY", "same-fixture-value");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "TACHI_TEST_SAME_VALUE_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "TACHI_TEST_SAME_VALUE_API_KEY".to_string(),
                value: "same-fixture-value".to_string(),
            }],
        )]);

        let report =
            materialize_provider_secrets(&llm, &vault_pools, ["TACHI_TEST_SAME_VALUE_API_KEY"])
                .expect("materialize");

        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert!(
            report.bypassed_names.is_empty(),
            "same-value env and Vault copies must not produce a false conflict warning: {report:?}"
        );
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
        assert!(
            reason.contains("Config key 'TACHI_TEST_PROVIDER_ALIAS_KEY' references a Vault alias")
        );
        assert!(!reason.contains("MISSING_ALIAS"));
        assert!(!reason.contains("TACHI_TEST_PROVIDER_ALIAS_KEY=vault:MISSING_ALIAS"));
        assert!(!reason.contains("config.env"));
        assert!(!reason.contains("vault_unlock"));
        assert!(!reason.contains("vault_set"));
        assert!(llm
            .provider_secret_for_tests(&["TACHI_TEST_PROVIDER_ALIAS_KEY"])
            .is_none());
    }

    // Discrimination test: before tachi#1287's missing-alias path removed the
    // same-named cloned base pool, this assertion observed `stale-secret`.
    #[test]
    fn materialize_provider_secrets_removes_same_named_stale_pool_for_missing_alias() {
        let _guard = crate::test_support::global_test_lock().lock();
        let key = "TACHI_TEST_MISSING_ALIAS_STALE_POOL_KEY";
        let _env = EnvGuard::set(key, "vault:MISSING_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            key.to_string(),
            vec![ProviderSecret {
                key_id: key.to_string(),
                value: "stale-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools, [key])
            .expect("missing alias should remain a tolerable skip");

        assert_eq!(report.skipped_aliases.len(), 1);
        assert!(llm.provider_secret_for_tests(&[key]).is_none());
    }

    // Discrimination test: the missing alias must remove only its same-named
    // stale pool. Before the fix, the first assertion below observed
    // `stale-secret`; the second protects unrelated Vault pools from removal.
    #[test]
    fn materialize_provider_secrets_preserves_unrelated_pools_when_removing_stale_alias_pool() {
        let _guard = crate::test_support::global_test_lock().lock();
        let key = "TACHI_TEST_MISSING_ALIAS_WITH_UNRELATED_POOL_KEY";
        let unrelated_key = "TACHI_TEST_UNRELATED_VAULT_POOL_KEY";
        let _env = EnvGuard::set(key, "vault:MISSING_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([
            (
                key.to_string(),
                vec![ProviderSecret {
                    key_id: key.to_string(),
                    value: "stale-secret".to_string(),
                }],
            ),
            (
                unrelated_key.to_string(),
                vec![ProviderSecret {
                    key_id: unrelated_key.to_string(),
                    value: "unrelated-secret".to_string(),
                }],
            ),
        ]);

        materialize_provider_secrets(&llm, &vault_pools, [key])
            .expect("missing alias should remain a tolerable skip");

        assert!(llm.provider_secret_for_tests(&[key]).is_none());
        assert_eq!(
            llm.provider_secret_for_tests(&[unrelated_key]).as_deref(),
            Some("unrelated-secret")
        );
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
        assert!(!err.contains("invalid key with spaces"));
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
        assert!(!err.contains(&oversized_name));
    }

    #[test]
    fn malformed_alias_refresh_retains_last_known_good_cache_and_sanitizes_error() {
        let _guard = crate::test_support::global_test_lock().lock();
        let alias_sentinel = "MALFORMED ALIAS SENTINEL MUST NOT LEAK";
        let _env = EnvGuard::set(
            "TACHI_TEST_LKG_ALIAS_API_KEY",
            &format!("vault:{alias_sentinel}"),
        );
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret("TACHI_TEST_LKG_API_KEY", "last-known-good-secret"));

        let err =
            materialize_provider_secrets(&llm, &HashMap::new(), ["TACHI_TEST_LKG_ALIAS_API_KEY"])
                .expect_err("malformed alias must refuse refresh");

        assert!(err.contains("TACHI_TEST_LKG_ALIAS_API_KEY"), "{err}");
        assert!(err.contains("refresh refused"), "{err}");
        assert!(
            !err.contains(alias_sentinel),
            "error leaked alias target: {err}"
        );
        assert!(
            !err.contains("last-known-good-secret"),
            "error leaked cached value: {err}"
        );
        assert_eq!(
            llm.provider_secret_for_tests(&["TACHI_TEST_LKG_API_KEY"])
                .as_deref(),
            Some("last-known-good-secret")
        );
        assert_eq!(llm.provider_secret_count(), 1);
    }

    #[test]
    fn later_alias_failure_exposes_no_partial_pool_and_retains_old_cache() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _good_env = EnvGuard::set("TACHI_TEST_PENDING_GOOD_API_KEY", "pending-good-secret");
        let alias_sentinel = "LATER MALFORMED ALIAS SENTINEL";
        let _bad_env = EnvGuard::set(
            "TACHI_TEST_PENDING_BAD_API_KEY",
            &format!("vault:{alias_sentinel}"),
        );
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret("TACHI_TEST_OLD_API_KEY", "old-cache-secret"));

        let err = materialize_provider_secrets(
            &llm,
            &HashMap::new(),
            [
                "TACHI_TEST_PENDING_GOOD_API_KEY",
                "TACHI_TEST_PENDING_BAD_API_KEY",
            ],
        )
        .expect_err("later malformed alias must refuse the complete refresh");

        assert!(err.contains("TACHI_TEST_PENDING_BAD_API_KEY"), "{err}");
        assert!(
            !err.contains(alias_sentinel),
            "error leaked alias target: {err}"
        );
        assert_eq!(
            llm.provider_secret_for_tests(&["TACHI_TEST_OLD_API_KEY"])
                .as_deref(),
            Some("old-cache-secret")
        );
        assert!(
            llm.provider_pool_statuses()
                .iter()
                .all(|pool| pool.logical_name != "TACHI_TEST_PENDING_GOOD_API_KEY"),
            "a failed refresh must not expose an earlier partially-built pool"
        );
        assert_eq!(llm.provider_secret_count(), 1);
    }

    #[test]
    fn successful_refresh_atomically_replaces_complete_provider_cache() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _new_env = EnvGuard::remove("TACHI_TEST_NEW_API_KEY");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret("TACHI_TEST_OLD_API_KEY", "old-cache-secret"));
        let vault_pools = HashMap::from([(
            "TACHI_TEST_NEW_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "TACHI_TEST_NEW_API_KEY".to_string(),
                value: "new-cache-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools, ["TACHI_TEST_NEW_API_KEY"])
            .expect("valid complete refresh");

        assert_eq!(report.loaded, 1);
        assert!(llm
            .provider_secret_for_tests(&["TACHI_TEST_OLD_API_KEY"])
            .is_none());
        assert_eq!(
            llm.provider_secret_for_tests(&["TACHI_TEST_NEW_API_KEY"])
                .as_deref(),
            Some("new-cache-secret")
        );
        assert_eq!(llm.provider_secret_count(), 1);
    }

    #[test]
    fn poisoned_materialization_lock_refuses_refresh_and_clear_loudly() {
        let _guard = crate::test_support::global_test_lock().lock();
        let logical_name = "TACHI_TEST_POISONED_TRANSACTION_API_KEY";
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret(logical_name, "last-known-good"));
        llm.poison_provider_materialization_lock_for_tests();
        let replacement = HashMap::from([(
            logical_name.to_string(),
            vec![ProviderSecret {
                key_id: logical_name.to_string(),
                value: "replacement-secret".to_string(),
            }],
        )]);

        let refresh_err = materialize_provider_secrets(&llm, &replacement, [logical_name])
            .expect_err("poisoned transaction lock must refuse refresh");
        assert!(refresh_err.contains("transaction lock is poisoned"));
        let clear_err = llm
            .clear_provider_secrets()
            .expect_err("poisoned transaction lock must refuse clear");
        assert!(clear_err.contains("transaction lock is poisoned"));
        assert_eq!(
            llm.provider_secret_for_tests(&[logical_name]).as_deref(),
            Some("last-known-good"),
            "refused mutations must leave prior cache unchanged"
        );
    }

    #[test]
    fn missing_alias_retains_existing_logical_pool_and_reports_key_only() {
        let _guard = crate::test_support::global_test_lock().lock();
        let alias_target = "MISSING_VOYAGE";
        let _env = EnvGuard::set("VOYAGE_API_KEY", &format!("vault:{alias_target}"));
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret("VOYAGE_API_KEY", "last-known-good-voyage"));

        let report = materialize_provider_secrets(&llm, &HashMap::new(), ["VOYAGE_API_KEY"])
            .expect("a valid missing alias remains a tolerated degraded refresh");

        assert_eq!(report.loaded, 1, "retained pools count as ready/loaded");
        assert_eq!(report.retained_from_last_known_good, vec!["VOYAGE_API_KEY"]);
        assert_eq!(report.skipped_aliases.len(), 1);
        assert!(!report.skipped_aliases[0].1.contains(alias_target));
        assert_eq!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("last-known-good-voyage")
        );
    }

    #[test]
    fn missing_alias_without_existing_pool_stays_absent_and_loudly_skipped() {
        let _guard = crate::test_support::global_test_lock().lock();
        let alias_target = "MISSING_VOYAGE_NO_CACHE";
        let _env = EnvGuard::set("VOYAGE_API_KEY", &format!("vault:{alias_target}"));
        let llm = LlmClient::new().expect("llm client");

        let report = materialize_provider_secrets(&llm, &HashMap::new(), ["VOYAGE_API_KEY"])
            .expect("a valid missing alias remains a tolerated degraded refresh");

        assert_eq!(report.loaded, 0);
        assert!(report.retained_from_last_known_good.is_empty());
        assert_eq!(report.skipped_aliases.len(), 1);
        assert_eq!(report.skipped_aliases[0].0, "VOYAGE_API_KEY");
        assert!(report.skipped_aliases[0]
            .1
            .contains("missing or Vault is locked"));
        assert!(!report.skipped_aliases[0].1.contains(alias_target));
        assert!(llm
            .provider_pool_statuses()
            .iter()
            .all(|pool| pool.logical_name != "VOYAGE_API_KEY"));
    }

    #[test]
    fn degraded_refresh_retains_only_skipped_configured_key_and_adds_valid_new_key() {
        let _guard = crate::test_support::global_test_lock().lock();
        let alias_target = "MISSING_VOYAGE_MIXED";
        let _voyage_env = EnvGuard::set("VOYAGE_API_KEY", &format!("vault:{alias_target}"));
        let _fresh_env = EnvGuard::set("TACHI_TEST_FRESH_API_KEY", "fresh-provider-secret");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret("VOYAGE_API_KEY", "old-voyage-secret"));
        assert!(llm.set_provider_secret("TACHI_TEST_REMOVED_API_KEY", "stale-secret"));

        let report = materialize_provider_secrets(
            &llm,
            &HashMap::new(),
            ["VOYAGE_API_KEY", "TACHI_TEST_FRESH_API_KEY"],
        )
        .expect("missing alias must not block an otherwise valid refresh");
        std::env::remove_var("TACHI_TEST_FRESH_API_KEY");

        assert_eq!(report.loaded, 2, "retained pools count as ready/loaded");
        assert_eq!(report.retained_from_last_known_good, vec!["VOYAGE_API_KEY"]);
        assert_eq!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("old-voyage-secret")
        );
        assert_eq!(
            llm.provider_secret_for_tests(&["TACHI_TEST_FRESH_API_KEY"])
                .as_deref(),
            Some("fresh-provider-secret")
        );
        assert!(llm
            .provider_pool_statuses()
            .iter()
            .all(|pool| pool.logical_name != "TACHI_TEST_REMOVED_API_KEY"));
        assert_eq!(llm.provider_secret_count(), 2);
    }

    #[test]
    fn missing_alias_outranks_canonical_same_key_and_retains_old_logical_pool() {
        let _guard = crate::test_support::global_test_lock().lock();
        let alias_target = "MISSING_VOYAGE_ALIAS";
        let _env = EnvGuard::set("VOYAGE_API_KEY", &format!("vault:{alias_target}"));
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret("VOYAGE_API_KEY", "old-logical-secret"));
        let vault_pools = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "VOYAGE_API_KEY".to_string(),
                value: "fresh-canonical-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools, ["VOYAGE_API_KEY"])
            .expect("missing alias remains a tolerated degraded refresh");

        assert_eq!(report.retained_from_last_known_good, vec!["VOYAGE_API_KEY"]);
        let materialized = llm.provider_secret_for_tests(&["VOYAGE_API_KEY"]);
        assert_eq!(materialized.as_deref(), Some("old-logical-secret"));
        assert_ne!(materialized.as_deref(), Some("fresh-canonical-secret"));
    }

    #[test]
    fn missing_alias_outranks_canonical_same_key_without_old_pool() {
        let _guard = crate::test_support::global_test_lock().lock();
        let alias_target = "MISSING_VOYAGE_WITHOUT_LKG";
        let _env = EnvGuard::set("VOYAGE_API_KEY", &format!("vault:{alias_target}"));
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "VOYAGE_API_KEY".to_string(),
                value: "fresh-canonical-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools, ["VOYAGE_API_KEY"])
            .expect("missing alias remains a tolerated degraded refresh");

        assert!(report.retained_from_last_known_good.is_empty());
        assert!(llm
            .provider_pool_statuses()
            .iter()
            .all(|pool| pool.logical_name != "VOYAGE_API_KEY"));
        assert!(llm.provider_secret_for_tests(&["VOYAGE_API_KEY"]).is_none());
    }

    #[test]
    fn existing_alias_target_outranks_canonical_same_key_pool() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("VOYAGE_API_KEY", "vault:VOYAGE_ALIAS_TARGET");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([
            (
                "VOYAGE_API_KEY".to_string(),
                vec![ProviderSecret {
                    key_id: "VOYAGE_API_KEY".to_string(),
                    value: "canonical-secret".to_string(),
                }],
            ),
            (
                "VOYAGE_ALIAS_TARGET".to_string(),
                vec![ProviderSecret {
                    key_id: "VOYAGE_ALIAS_TARGET".to_string(),
                    value: "target-secret".to_string(),
                }],
            ),
        ]);

        let report = materialize_provider_secrets(&llm, &vault_pools, ["VOYAGE_API_KEY"])
            .expect("existing explicit alias target must materialize");

        assert_eq!(report.from_alias, 1);
        assert!(report.retained_from_last_known_good.is_empty());
        assert_eq!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("target-secret")
        );
    }

    #[test]
    fn retained_pool_preserves_index_and_cooldown_while_new_pool_resets_state() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("VOYAGE_API_KEY", "vault:MISSING_STATEFUL_VOYAGE");
        let llm = LlmClient::new().expect("llm client");
        let voyage_first = "VOYAGE_API_KEY_1";
        let replacement_first = "TACHI_TEST_REPLACED_API_KEY_1";
        assert!(llm.set_provider_secret_pool(
            "VOYAGE_API_KEY",
            vec![
                ProviderSecret {
                    key_id: voyage_first.to_string(),
                    value: "old-voyage-one".to_string(),
                },
                ProviderSecret {
                    key_id: "VOYAGE_API_KEY_2".to_string(),
                    value: "old-voyage-two".to_string(),
                },
            ],
        ));
        assert!(llm.set_provider_secret_pool(
            "TACHI_TEST_REPLACED_API_KEY",
            vec![ProviderSecret {
                key_id: replacement_first.to_string(),
                value: "old-replaced-secret".to_string(),
            }],
        ));
        llm.seed_provider_operational_state_for_tests("VOYAGE_API_KEY", 1, voyage_first);
        llm.seed_provider_operational_state_for_tests(
            "TACHI_TEST_REPLACED_API_KEY",
            1,
            replacement_first,
        );
        let vault_pools = HashMap::from([(
            "TACHI_TEST_REPLACED_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: replacement_first.to_string(),
                value: "new-replaced-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(
            &llm,
            &vault_pools,
            ["VOYAGE_API_KEY", "TACHI_TEST_REPLACED_API_KEY"],
        )
        .expect("degraded refresh should atomically combine retained and new pools");

        assert_eq!(report.retained_from_last_known_good, vec!["VOYAGE_API_KEY"]);
        let statuses = llm.provider_pool_statuses();
        let retained = statuses
            .iter()
            .find(|pool| pool.logical_name == "VOYAGE_API_KEY")
            .expect("retained pool status");
        assert_eq!(retained.current_index, 1);
        assert_eq!(retained.rate_limited_keys.len(), 1);
        assert_eq!(retained.rate_limited_keys[0].key_id, voyage_first);
        let replaced = statuses
            .iter()
            .find(|pool| pool.logical_name == "TACHI_TEST_REPLACED_API_KEY")
            .expect("new pool status");
        assert_eq!(replaced.current_index, 0);
        assert!(replaced.rate_limited_keys.is_empty());
    }

    #[test]
    fn retained_cooldown_does_not_block_fresh_pool_with_same_member_id() {
        let _guard = crate::test_support::global_test_lock().lock();
        let retained_logical = "VOYAGE_API_KEY";
        let fresh_logical = "TACHI_TEST_FRESH_SHARED_API_KEY";
        let shared_member = "TACHI_TEST_SHARED_COOLDOWN_MEMBER";
        let _env = EnvGuard::set(retained_logical, "vault:MISSING_SHARED_COOLDOWN");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret_pool(
            retained_logical,
            vec![ProviderSecret {
                key_id: shared_member.to_string(),
                value: "retained-old-secret".to_string(),
            }],
        ));
        assert!(llm.set_provider_secret_pool(
            fresh_logical,
            vec![ProviderSecret {
                key_id: shared_member.to_string(),
                value: "stale-fresh-secret".to_string(),
            }],
        ));
        llm.seed_provider_operational_state_for_tests(retained_logical, 0, shared_member);
        let vault_pools = HashMap::from([(
            fresh_logical.to_string(),
            vec![ProviderSecret {
                key_id: shared_member.to_string(),
                value: "fresh-replacement-secret".to_string(),
            }],
        )]);

        let report =
            materialize_provider_secrets(&llm, &vault_pools, [retained_logical, fresh_logical])
                .expect("retained and fresh pools should replace atomically");

        assert_eq!(report.retained_from_last_known_good, vec![retained_logical]);
        let statuses = llm.provider_pool_statuses();
        let retained = statuses
            .iter()
            .find(|pool| pool.logical_name == retained_logical)
            .expect("retained pool status");
        assert_eq!(retained.rate_limited_keys.len(), 1);
        let fresh = statuses
            .iter()
            .find(|pool| pool.logical_name == fresh_logical)
            .expect("fresh pool status");
        assert!(
            fresh.rate_limited_keys.is_empty(),
            "cooldown identity must include the logical pool"
        );
        assert_eq!(
            llm.provider_secret_for_tests(&[fresh_logical]).as_deref(),
            Some("fresh-replacement-secret"),
            "fresh pool sharing a member id must remain selectable"
        );
        assert!(
            llm.provider_secret_for_tests(&[retained_logical]).is_none(),
            "retained pool must keep its own cooldown"
        );
    }

    #[test]
    fn concurrent_refresh_cannot_overwrite_newer_cache_from_stale_alias_snapshot() {
        let _guard = crate::test_support::global_test_lock().lock();
        let logical_name = "TACHI_TEST_CONCURRENT_ALIAS_API_KEY";
        let _env = EnvGuard::set(logical_name, "vault:MISSING_CONCURRENT_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret(logical_name, "old-last-known-good"));

        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let stale_llm = llm.clone();
        let stale = std::thread::spawn(move || {
            materialize_provider_secrets_inner(
                &stale_llm,
                &HashMap::new(),
                [logical_name],
                // These two race tests are about a LOCKED source being
                // overtaken mid-refresh, which is exactly the case that still
                // retains its last-known-good pool.
                VaultSourceAvailability::LockedOrUnavailable,
                Some(Box::new(move || {
                    snapshot_tx.send(()).expect("announce stale snapshot");
                    release_rx.recv().expect("release stale refresh");
                })),
            )
        });
        snapshot_rx.recv().expect("stale refresh reached snapshot");

        let transaction_is_serialized = llm.provider_materialization_lock_is_held_for_tests();
        let (fresh_done_tx, fresh_done_rx) = std::sync::mpsc::channel();
        let fresh_llm = llm.clone();
        let fresh = std::thread::spawn(move || {
            let vault_pools = HashMap::from([(
                logical_name.to_string(),
                vec![ProviderSecret {
                    key_id: logical_name.to_string(),
                    value: "newer-provider-secret".to_string(),
                }],
            )]);
            let result =
                materialize_provider_secrets(&fresh_llm, &vault_pools, std::iter::empty::<&str>());
            fresh_done_tx.send(()).expect("announce fresh refresh");
            result
        });

        if transaction_is_serialized {
            release_tx
                .send(())
                .expect("release serialized stale refresh");
            stale
                .join()
                .expect("stale refresh thread")
                .expect("stale degraded refresh");
            fresh_done_rx.recv().expect("fresh refresh completed");
        } else {
            fresh_done_rx
                .recv()
                .expect("unserialized fresh refresh completed first");
            release_tx.send(()).expect("release stale overwrite");
            stale
                .join()
                .expect("stale refresh thread")
                .expect("stale degraded refresh");
        }
        fresh
            .join()
            .expect("fresh refresh thread")
            .expect("fresh refresh succeeds");

        assert_eq!(
            llm.provider_secret_for_tests(&[logical_name]).as_deref(),
            Some("newer-provider-secret"),
            "a stale missing-alias snapshot must never roll back a newer refresh"
        );
    }

    #[test]
    fn explicit_clear_finishes_after_inflight_stale_materialization() {
        #[derive(Debug, PartialEq, Eq)]
        enum ClearEvent {
            Started,
            WaitingForMaterializationGuard,
            Done,
        }

        let _guard = crate::test_support::global_test_lock().lock();
        let logical_name = "TACHI_TEST_LOCK_RACE_API_KEY";
        let _env = EnvGuard::set(logical_name, "vault:MISSING_LOCK_RACE_ALIAS");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret(logical_name, "last-known-good-before-lock"));

        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let stale_llm = llm.clone();
        let stale = std::thread::spawn(move || {
            materialize_provider_secrets_inner(
                &stale_llm,
                &HashMap::new(),
                [logical_name],
                // These two race tests are about a LOCKED source being
                // overtaken mid-refresh, which is exactly the case that still
                // retains its last-known-good pool.
                VaultSourceAvailability::LockedOrUnavailable,
                Some(Box::new(move || {
                    snapshot_tx.send(()).expect("announce stale snapshot");
                    release_rx.recv().expect("release stale materialization");
                })),
            )
        });
        snapshot_rx.recv().expect("stale refresh reached snapshot");

        let (clear_event_tx, clear_event_rx) = std::sync::mpsc::channel();
        let clear_llm = llm.clone();
        let clear = std::thread::spawn(move || {
            clear_event_tx
                .send(ClearEvent::Started)
                .expect("announce clear call");
            let waiting_tx = clear_event_tx.clone();
            clear_llm
                .clear_provider_secrets_with_hook_for_tests(move || {
                    waiting_tx
                        .send(ClearEvent::WaitingForMaterializationGuard)
                        .expect("announce materialization guard wait");
                })
                .expect("clear provider secrets");
            clear_event_tx
                .send(ClearEvent::Done)
                .expect("announce clear completion");
        });
        assert_eq!(
            clear_event_rx.recv().expect("clear call started"),
            ClearEvent::Started
        );
        let next_event = clear_event_rx.recv().expect("clear ordering event");
        match next_event {
            ClearEvent::WaitingForMaterializationGuard => {
                release_tx
                    .send(())
                    .expect("release stale materialization before serialized clear");
                stale
                    .join()
                    .expect("stale materialization thread")
                    .expect("stale degraded materialization");
                assert_eq!(
                    clear_event_rx.recv().expect("serialized clear completed"),
                    ClearEvent::Done
                );
            }
            ClearEvent::Done => {
                release_tx
                    .send(())
                    .expect("release stale writer after early clear");
                stale
                    .join()
                    .expect("stale materialization thread")
                    .expect("stale degraded materialization");
            }
            ClearEvent::Started => panic!("duplicate clear-start event"),
        }
        clear.join().expect("clear thread");

        assert_eq!(
            llm.provider_secret_count(),
            0,
            "explicit clear must finish last and leave no stale provider secret"
        );
        assert!(llm.provider_secret_for_tests(&[logical_name]).is_none());
    }

    #[test]
    fn replacement_clears_stale_health_but_retained_pool_keeps_composite_health_state() {
        let _guard = crate::test_support::global_test_lock().lock();
        let _env = EnvGuard::set("VOYAGE_API_KEY", "vault:MISSING_HEALTHY_VOYAGE");
        let llm = LlmClient::new().expect("llm client");
        let retained_logical = "VOYAGE_API_KEY";
        let replaced_logical = "TACHI_TEST_REPLACED_HEALTH_API_KEY";
        // Deliberately collide the member id across two logical pools. Health
        // identity is (logical_name, key_id), never key_id alone.
        let retained_member = "TACHI_TEST_SHARED_HEALTH_MEMBER";
        let replaced_member = retained_member;
        assert!(llm.set_provider_secret_pool(
            retained_logical,
            vec![ProviderSecret {
                key_id: retained_member.to_string(),
                value: "retained-old-secret".to_string(),
            }],
        ));
        assert!(llm.set_provider_secret_pool(
            replaced_logical,
            vec![ProviderSecret {
                key_id: replaced_member.to_string(),
                value: "replaced-old-secret".to_string(),
            }],
        ));
        llm.mark_provider_key_auth_failed_for_tests(retained_logical, retained_member);
        llm.mark_provider_key_auth_failed_for_tests(replaced_logical, replaced_member);
        assert_eq!(
            llm.provider_health_state_presence_for_tests(retained_logical, retained_member),
            (true, true)
        );
        assert_eq!(
            llm.provider_health_state_presence_for_tests(replaced_logical, replaced_member),
            (true, true)
        );
        let vault_pools = HashMap::from([(
            replaced_logical.to_string(),
            vec![ProviderSecret {
                key_id: replaced_member.to_string(),
                value: "replacement-new-secret".to_string(),
            }],
        )]);

        let report =
            materialize_provider_secrets(&llm, &vault_pools, [retained_logical, replaced_logical])
                .expect("health-state replacement remains atomic");

        assert_eq!(report.retained_from_last_known_good, vec![retained_logical]);
        assert_eq!(
            llm.provider_health_state_presence_for_tests(retained_logical, retained_member),
            (true, true),
            "retained logical/member identity must preserve health and snapshot"
        );
        assert!(
            llm.provider_secret_for_tests(&[retained_logical]).is_none(),
            "retained auth-failed member must remain blocked"
        );
        assert_eq!(
            llm.provider_health_state_presence_for_tests(replaced_logical, replaced_member),
            (false, false),
            "new credential with reused logical/key id must not inherit old health"
        );
        assert_eq!(
            llm.provider_secret_for_tests(&[replaced_logical])
                .as_deref(),
            Some("replacement-new-secret"),
            "replacement credential must be selectable after stale health is removed"
        );
    }

    /// Discrimination pair for the retention rule. Same key, same env alias,
    /// same empty Vault result, same populated cache — only the SOURCE
    /// availability differs, and the outcomes must be opposite. Flip either
    /// arm of the `match` in the missing-alias branch and exactly one of these
    /// two goes red.
    #[test]
    fn readable_vault_drops_the_cached_pool_when_the_alias_target_is_gone() {
        let _guard = crate::test_support::global_test_lock().lock();
        let key = "TACHI_TEST_REVOKED_ALIAS_API_KEY";
        let _env = EnvGuard::set(key, "vault:TACHI_TEST_REVOKED_TARGET");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret(key, "revoked-but-still-cached"));

        let report = materialize_provider_secrets_from_durable_source(&llm, [key], || {
            Ok((HashMap::new(), VaultSourceAvailability::Readable))
        })
        .expect("a missing alias stays a tolerable skip, not a hard error");

        assert_eq!(report.skipped_aliases.len(), 1);
        assert!(
            report.retained_from_last_known_good.is_empty(),
            "a readable Vault that lacks the alias target has answered: the \
             secret is revoked, so nothing may be retained: {report:?}"
        );
        assert!(
            llm.provider_secret_for_tests(&[key]).is_none(),
            "a revoked credential must stop being served"
        );
    }

    #[test]
    fn locked_vault_retains_the_cached_pool_for_the_same_missing_alias() {
        let _guard = crate::test_support::global_test_lock().lock();
        let key = "TACHI_TEST_LOCKED_ALIAS_API_KEY";
        let _env = EnvGuard::set(key, "vault:TACHI_TEST_LOCKED_TARGET");
        let llm = LlmClient::new().expect("llm client");
        assert!(llm.set_provider_secret(key, "last-known-good"));

        let report = materialize_provider_secrets_from_durable_source(&llm, [key], || {
            Ok((HashMap::new(), VaultSourceAvailability::LockedOrUnavailable))
        })
        .expect("a locked Vault stays a tolerable skip");

        assert_eq!(report.skipped_aliases.len(), 1);
        assert_eq!(report.retained_from_last_known_good, vec![key.to_string()]);
        assert_eq!(
            llm.provider_secret_for_tests(&[key]).as_deref(),
            Some("last-known-good"),
            "a lock says nothing about the secret, so the pool survives it"
        );
    }

    /// The two skip reasons must be distinguishable in operator output, or the
    /// distinction above is invisible to whoever reads the daemon log.
    #[test]
    fn skip_reason_names_which_of_the_two_conditions_occurred() {
        let _guard = crate::test_support::global_test_lock().lock();
        let key = "TACHI_TEST_SKIP_REASON_API_KEY";
        let _env = EnvGuard::set(key, "vault:TACHI_TEST_SKIP_REASON_TARGET");
        let llm = LlmClient::new().expect("llm client");

        let readable = materialize_provider_secrets_from_durable_source(&llm, [key], || {
            Ok((HashMap::new(), VaultSourceAvailability::Readable))
        })
        .expect("skip");
        let locked = materialize_provider_secrets_from_durable_source(&llm, [key], || {
            Ok((HashMap::new(), VaultSourceAvailability::LockedOrUnavailable))
        })
        .expect("skip");

        assert!(
            readable.skipped_aliases[0]
                .1
                .contains("absent from a readable Vault"),
            "{:?}",
            readable.skipped_aliases
        );
        assert!(
            locked.skipped_aliases[0].1.contains("could not be read"),
            "{:?}",
            locked.skipped_aliases
        );
        for report in [&readable, &locked] {
            assert!(
                !report.skipped_aliases[0]
                    .1
                    .contains("TACHI_TEST_SKIP_REASON_TARGET"),
                "the skip reason must not echo the alias target: {:?}",
                report.skipped_aliases
            );
        }
    }
}
