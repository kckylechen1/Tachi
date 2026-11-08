use memcore::vault::VaultKeyHealth;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::rotation::{build_rotation_status, rotation_member_names, RotationSourceStatus};
use super::types::{ProviderProbeCache, ProviderRotationGroupProbe};
use crate::provider_config::key_registry::{ApiKeyDef, API_KEY_DEFS};
use crate::provider_config::keychain_scan::load_keychain_vault_api_key_values;
use crate::status_ops::ApiKeyStatus;

pub(super) fn collect_api_key_status(global_db_path: &Path) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, false, None, None, false, None)
}

pub(super) fn collect_api_key_status_with_value_compare(
    global_db_path: &Path,
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, true, None, None, false, None)
}

pub(super) fn collect_api_key_status_immutable(global_db_path: &Path) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, false, None, None, true, None)
}

/// `resolved_home`: the caller's server-bound home (`MemoryServer::tachi_home_dir()`),
/// unioned into the `config.env` scan set (#1096 leaf-2a round-2, codex
/// B4-status) — see `provider_config::collect_config_env_values`'s doc for
/// why this closes the manifest-side/provider-side split-home bug. The only
/// caller of this function (`status_ops::snapshot::collect_snapshot_inner`)
/// always has an `app_home` in scope, so it is passed as `Some`.
pub(crate) fn collect_api_key_status_with_probe_cache(
    global_db_path: &Path,
    probe_cache: Option<&ProviderProbeCache>,
    compare_vault_values: bool,
    resolved_home: Option<&Path>,
    llm: Option<&tachi_llm::LlmClient>,
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(
        global_db_path,
        compare_vault_values,
        probe_cache,
        resolved_home,
        false,
        llm,
    )
}

fn collect_api_key_status_inner(
    global_db_path: &Path,
    compare_vault_values: bool,
    probe_cache: Option<&ProviderProbeCache>,
    resolved_home: Option<&Path>,
    immutable: bool,
    llm: Option<&tachi_llm::LlmClient>,
) -> Vec<ApiKeyStatus> {
    let mut vault_names = HashSet::new();
    let mut rotation_rows = Vec::new();
    let mut key_health_rows = Vec::new();
    if immutable {
        let encoded = global_db_path
            .to_string_lossy()
            .replace('?', "%3f")
            .replace('#', "%23")
            .replace(' ', "%20");
        let uri = format!("file:{encoded}?mode=ro&immutable=1");
        if let Ok(conn) = memcore::db::open_immutable_readonly(&uri) {
            if let Ok(entries) = memcore::db::vault_list_entries(&conn) {
                vault_names.extend(
                    entries
                        .into_iter()
                        .filter(|entry| entry.secret_type == "api_key")
                        .map(|entry| entry.name),
                );
            }
            if let Ok(rows) = memcore::db::vault_list_rotations(&conn) {
                rotation_rows = rows;
            }
            if let Ok(rows) = memcore::db::vault_list_key_health(&conn, None) {
                key_health_rows = rows;
            }
        }
    } else if let Some(path) = global_db_path.to_str() {
        if let Ok(store) = memcore::MemoryStore::open_read_only(path) {
            if let Ok(entries) = store.vault_list_entries() {
                vault_names.extend(
                    entries
                        .into_iter()
                        .filter(|entry| entry.secret_type == "api_key")
                        .map(|entry| entry.name),
                );
            }
            if let Ok(rows) = store.vault_list_rotations() {
                rotation_rows = rows;
            }
            if let Ok(rows) = store.vault_list_key_health(None) {
                key_health_rows = rows;
            }
        }
    }
    let mut key_health: HashMap<String, HashMap<String, VaultKeyHealth>> = HashMap::new();
    for row in key_health_rows {
        key_health
            .entry(row.logical_name.clone())
            .or_default()
            .insert(row.key_id.clone(), row);
    }
    let rotations = rotation_rows
        .into_iter()
        .map(|rotation| {
            let members = rotation_member_names(&vault_names, &rotation.prefix);
            (
                rotation.prefix,
                RotationSourceStatus {
                    total_keys: rotation.total_keys,
                    current_index: rotation.current_index,
                    strategy: rotation.rotation_strategy,
                    members,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let config_env = crate::provider_config::collect_config_env_values(resolved_home);
    let vault_values: HashMap<String, String> = if compare_vault_values {
        match load_keychain_vault_api_key_values(global_db_path) {
            Ok(values) => values.into_iter().collect(),
            Err(err) => {
                tracing::warn!(
                    "[vault] keychain vault read failed during status health check: {err}"
                );
                HashMap::new()
            }
        }
    } else {
        HashMap::new()
    };

    let rotation_probes = probe_cache
        .map(|cache| {
            cache
                .rotation_groups
                .iter()
                .map(|group| (group.logical_name.clone(), group))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    collect_api_key_status_from_sources(
        vault_names,
        vault_values,
        config_env,
        rotations,
        &key_health,
        &rotation_probes,
        llm,
    )
}

fn lane_key_requirements(
    config: &tachi_llm::ProviderRuntimeConfig,
    fallbacks: &tachi_llm::llm::LaneFallbackConfig,
) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut requirements = vec![
        ("embedding", vec!["VOYAGE_API_KEY"]),
        ("extract", config.extract.api_key_envs.clone()),
        ("summary", config.summary.api_key_envs.clone()),
        ("distill", config.distill.api_key_envs.clone()),
        ("reasoning", config.reasoning.api_key_envs.clone()),
    ];
    if config.rerank.provider == tachi_llm::RerankProviderKind::Voyage {
        requirements.push(("rerank", vec!["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"]));
    }
    for (lane, config) in [
        ("extract fallback", &fallbacks.extract),
        ("summary fallback", &fallbacks.summary),
        ("distill fallback", &fallbacks.distill),
        ("reasoning fallback", &fallbacks.reasoning),
    ] {
        if let Some(config) = config {
            requirements.push((lane, config.api_key_envs.clone()));
        }
    }
    requirements
}

type LaneKeySource = (&'static str, bool, bool);

enum OfflineLaneKey {
    Plaintext(LaneKeySource),
    Unverified,
    Missing,
}

fn offline_lane_key(
    name: &str,
    vault_names: &HashSet<String>,
    config_env: &HashMap<String, String>,
    key_health: &HashMap<String, HashMap<String, VaultKeyHealth>>,
) -> OfflineLaneKey {
    let vault_has = |name: &str| {
        vault_names.contains(name)
            || vault_names.iter().any(|member| {
                crate::provider_config::parse_rotation_member_name(member)
                    .is_some_and(|(prefix, _)| prefix == name)
            })
    };
    let env_value = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let file_value = config_env
        .get(name)
        .filter(|value| !value.trim().is_empty());
    let effective = env_value
        .as_deref()
        .or_else(|| file_value.map(String::as_str));
    if let Some(value) = effective.filter(|value| crate::provider_config::is_vault_alias(value)) {
        // Listing is not materialization: ACL, health, type, binding, decryption
        // and pool admission have not been established by this offline view.
        return if crate::provider_config::parse_vault_alias(value).is_some_and(|target| {
            tachi_llm::provider_names::validate_vault_alias_name(target).is_ok()
                && crate::provider_config::is_model_provider_pool_name(target)
                && vault_has(target)
        }) {
            OfflineLaneKey::Unverified
        } else {
            OfflineLaneKey::Missing
        };
    }
    if effective.is_some() {
        if key_health
            .get(name)
            .and_then(|members| members.get(name))
            .and_then(|health| crate::vault_ops::unusable_skip_class(health, chrono::Utc::now()))
            .is_some()
        {
            return OfflineLaneKey::Missing;
        }
        return OfflineLaneKey::Plaintext((
            if env_value.is_some() {
                "env"
            } else {
                "config.env"
            },
            env_value.is_some(),
            false,
        ));
    }
    if crate::provider_config::is_model_provider_pool_name(name) && vault_has(name) {
        OfflineLaneKey::Unverified
    } else {
        OfflineLaneKey::Missing
    }
}

fn apply_lane_requirements(
    rows: &mut Vec<ApiKeyStatus>,
    requirements: &[(&str, Vec<&'static str>)],
    configured: &HashMap<&str, LaneKeySource>,
    unverified: &HashSet<&str>,
) {
    for row in rows.iter_mut() {
        row.required = false;
        row.required_for.clear();
        row.missing_required_for.clear();
        row.unverified_required_for.clear();
    }
    for (lane, keys) in requirements {
        let selected = keys
            .iter()
            .find(|name| configured.contains_key(**name))
            .copied();
        let uncertain = keys
            .iter()
            .find(|name| unverified.contains(**name))
            .copied();
        let Some(name) = selected.or(uncertain).or_else(|| keys.first().copied()) else {
            continue;
        };
        let index = rows
            .iter()
            .position(|row| row.name == name)
            .or_else(|| {
                rows.iter()
                    .position(|row| row.alias_names.iter().any(|alias| alias == name))
            })
            .unwrap_or_else(|| {
                let (source, env_configured, vault_configured) = configured
                    .get(name)
                    .copied()
                    .unwrap_or(("none", false, false));
                rows.push(ApiKeyStatus {
                    name: name.to_string(),
                    label: "Configured model lane credential".into(),
                    required: false,
                    required_for: Vec::new(),
                    missing_required_for: Vec::new(),
                    unverified_required_for: Vec::new(),
                    deprecated: false,
                    canonical_name: name.to_string(),
                    alias_names: Vec::new(),
                    status: if selected.is_some() {
                        "configured"
                    } else if uncertain.is_some() {
                        "unverified"
                    } else {
                        "missing"
                    }
                    .into(),
                    source: source.into(),
                    env_configured,
                    vault_configured,
                    cleanup_hint: None,
                    drift_warning: None,
                    inferred_invalid_provider: None,
                    rotation: None,
                });
                rows.len() - 1
            });
        rows[index].required = true;
        rows[index].required_for.push((*lane).to_string());
        if selected.is_none() {
            if uncertain.is_some() {
                rows[index]
                    .unverified_required_for
                    .push((*lane).to_string());
            } else {
                rows[index].missing_required_for.push((*lane).to_string());
            }
        }
    }
}

pub(super) fn collect_api_key_status_from_sources(
    vault_names: HashSet<String>,
    vault_values: HashMap<String, String>,
    config_env: HashMap<String, String>,
    rotations: HashMap<String, RotationSourceStatus>,
    key_health: &HashMap<String, HashMap<String, VaultKeyHealth>>,
    rotation_probes: &HashMap<String, &ProviderRotationGroupProbe>,
    llm: Option<&tachi_llm::LlmClient>,
) -> Vec<ApiKeyStatus> {
    let config = llm
        .map(tachi_llm::LlmClient::runtime_config)
        .or_else(|| tachi_llm::ProviderRuntimeConfig::from_env().ok());
    let fallbacks = llm
        .map(tachi_llm::LlmClient::runtime_fallback_config)
        .unwrap_or_else(tachi_llm::llm::LaneFallbackConfig::from_env);
    let requirements = config
        .as_ref()
        .map(|config| lane_key_requirements(config, &fallbacks));
    let mut configured = HashMap::new();
    let mut unverified = HashSet::new();
    if let Some(requirements) = &requirements {
        for name in requirements
            .iter()
            .flat_map(|(_, keys)| keys.iter().copied())
        {
            if let Some(llm) = llm {
                if llm.has_usable_secret_readonly(&[name]) {
                    configured.insert(name, ("runtime", false, false));
                }
            } else {
                match offline_lane_key(name, &vault_names, &config_env, key_health) {
                    OfflineLaneKey::Plaintext(source) => {
                        configured.insert(name, source);
                    }
                    OfflineLaneKey::Unverified => {
                        unverified.insert(name);
                    }
                    OfflineLaneKey::Missing => {}
                }
            }
        }
    }
    let now = chrono::Utc::now();
    let mut rows = API_KEY_DEFS
        .iter()
        .map(|def| {
            let env_value = std::env::var(def.key).ok();
            let env_configured = env_value
                .as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
            let env_is_vault_alias = env_value
                .as_deref()
                .map(crate::provider_config::is_vault_alias)
                .unwrap_or(false);
            let rotation_member_configured = vault_names.iter().any(|name| {
                crate::provider_config::parse_rotation_member_name(name)
                    .is_some_and(|(prefix, _)| prefix == def.key)
            });
            let rotation = rotations.get(def.key);
            let vault_configured =
                vault_names.contains(def.key) || rotation.is_some() || rotation_member_configured;
            let config_value = config_env.get(def.key);
            let config_present = config_value.is_some();
            let config_is_vault_alias = config_value
                .map(|value| crate::provider_config::is_vault_alias(value))
                .unwrap_or(false);
            let config_alias_resolves = config_value
                .and_then(|value| crate::provider_config::parse_vault_alias(value))
                .map(|target| vault_names.contains(target))
                .unwrap_or(false);
            let env_plaintext = plaintext_provider_value(env_value.as_deref());
            let config_plaintext = plaintext_provider_value(config_value.map(String::as_str));
            let duplicate_plaintext = env_plaintext
                .map(|value| ("env", value))
                .or_else(|| config_plaintext.map(|value| ("config.env", value)));
            let duplicate_matches_vault = duplicate_plaintext.and_then(|(_, plaintext)| {
                vault_values
                    .get(def.key)
                    .map(|vault_value| plaintext == vault_value.trim())
            });
            let alias_configured = def.aliases.iter().any(|alias| {
                vault_names.contains(*alias)
                    || config_env.contains_key(*alias)
                    || std::env::var(alias)
                        .ok()
                        .is_some_and(|value| !value.trim().is_empty())
            });
            let file_configured = config_present;
            let (status, source) = if vault_configured
                && (config_is_vault_alias && config_alias_resolves)
            {
                ("configured", "vault(config.env)".to_string())
            } else if vault_configured
                && (env_is_vault_alias || (file_configured && config_is_vault_alias))
            {
                if config_alias_resolves || env_is_vault_alias {
                    ("configured", "vault(config.env)".to_string())
                } else {
                    ("missing", "vault-alias-unresolved".to_string())
                }
            } else if vault_configured
                && ((env_configured && !env_is_vault_alias)
                    || (file_configured && !config_is_vault_alias))
            {
                let duplicate_source = duplicate_plaintext
                    .map(|(source, _)| source)
                    .unwrap_or("plaintext");
                match duplicate_matches_vault {
                    Some(false) => ("drift", format!("vault+{duplicate_source}")),
                    Some(true) => ("configured", format!("vault+{duplicate_source}(same)")),
                    None => (
                        "configured",
                        format!("vault+{duplicate_source}(unverified)"),
                    ),
                }
            } else if vault_configured {
                ("configured", "vault".to_string())
            } else if env_configured || file_configured || alias_configured {
                (
                    "configured",
                    if env_configured {
                        if env_is_vault_alias {
                            "vault(config.env)".to_string()
                        } else {
                            "env".to_string()
                        }
                    } else if file_configured {
                        if config_is_vault_alias {
                            "vault(config.env)".to_string()
                        } else {
                            "config.env".to_string()
                        }
                    } else {
                        "alias".to_string()
                    },
                )
            } else if def.deprecated {
                ("deprecated-unset", "none".to_string())
            } else {
                ("missing", "none".to_string())
            };
            let drift_warning = if vault_configured
                && ((env_configured && !env_is_vault_alias)
                    || (file_configured && !config_is_vault_alias))
            {
                let duplicate_source = duplicate_plaintext
                    .map(|(source, _)| source)
                    .unwrap_or("env/config.env");
                let message = match duplicate_matches_vault {
                    Some(false) => format!(
                        "drift: plaintext key in {duplicate_source} differs from Vault; Vault remains the provider source, but remove or replace the plaintext line with vault:{}",
                        def.key
                    ),
                    Some(true) => format!(
                        "redundant: plaintext key in {duplicate_source} duplicates Vault; replace it with vault:{} or remove it",
                        def.key
                    ),
                    None => format!(
                        "duplicate-unverified: plaintext key in {duplicate_source} exists while Vault also holds this key; unlock Vault via Keychain to compare, then use vault:{} or remove the plaintext line",
                        def.key
                    ),
                };
                Some(message)
            } else {
                None
            };
            let cleanup_hint =
                cleanup_hint_for_key(def, vault_configured || env_configured || file_configured);
            ApiKeyStatus {
                name: def.key.to_string(),
                label: def.label.to_string(),
                required: def.required,
                required_for: Vec::new(),
                missing_required_for: Vec::new(),
                unverified_required_for: Vec::new(),
                deprecated: def.deprecated,
                canonical_name: def.canonical_key.to_string(),
                alias_names: def.aliases.iter().map(|alias| alias.to_string()).collect(),
                status: status.to_string(),
                source,
                env_configured,
                vault_configured,
                cleanup_hint,
                drift_warning,
                inferred_invalid_provider: None,
                rotation: rotation.map(|rotation| {
                    build_rotation_status(
                        rotation,
                        rotation_probes.get(def.key).copied(),
                        key_health.get(def.key),
                        now,
                    )
                }),
            }
        })
        .collect();
    if let Some(requirements) = requirements {
        apply_lane_requirements(&mut rows, &requirements, &configured, &unverified);
    } else {
        for row in rows.iter_mut().filter(|row| row.required) {
            row.required_for.push("configuration unresolved".into());
            row.unverified_required_for
                .push("configuration unresolved".into());
        }
    }
    rows
}

fn cleanup_hint_for_key(def: &ApiKeyDef, configured: bool) -> Option<String> {
    if def.deprecated {
        if configured {
            Some(format!(
                "{} is deprecated; migrate this secret to {} and remove {} from Vault/env/config.env after confirming the canonical key probes OK.",
                def.key, def.canonical_key, def.key
            ))
        } else {
            None
        }
    } else if !def.aliases.is_empty() {
        Some(format!(
            "canonical key: {}; accepted aliases/fallbacks: {}",
            def.canonical_key,
            def.aliases.join(", ")
        ))
    } else {
        None
    }
}

fn plaintext_provider_value(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty() || crate::provider_config::is_vault_alias(value) {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;
    use crate::test_support::EnvRestore;
    use tachi_llm::llm::LaneFallbackConfig;
    use tachi_llm::{ChatLaneConfig, ProviderRuntimeConfig, RerankConfig, RerankProviderKind};

    fn lane(keys: Vec<&'static str>) -> ChatLaneConfig {
        ChatLaneConfig {
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key_envs: keys,
        }
    }

    fn deepseek_config() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            extract: lane(vec!["DEEPSEEK_API_KEY"]),
            summary: lane(vec!["DEEPSEEK_API_KEY"]),
            distill: lane(vec!["DEEPSEEK_API_KEY"]),
            reasoning: lane(vec!["DEEPSEEK_API_KEY"]),
            rerank: RerankConfig {
                provider: RerankProviderKind::Local,
                local_endpoint: Some("http://127.0.0.1:9000/rerank".into()),
            },
        }
    }

    fn clear_keys() -> Vec<EnvRestore> {
        [
            "DEEPSEEK_API_KEY",
            "EXTRACT_API_KEY",
            "SUMMARY_API_KEY",
            "SILICONFLOW_API_KEY",
            "VOYAGE_API_KEY",
            "VOYAGE_RERANK_API_KEY",
            "EXTRACT_FALLBACK_API_KEY",
            "SUMMARY_FALLBACK_API_KEY",
            "DISTILL_FALLBACK_API_KEY",
            "REASONING_FALLBACK_API_KEY",
            "DISTILL_API_KEY",
            "REASONING_API_KEY",
            "ZAI_API_KEY",
            "BIGMODEL_API_KEY",
        ]
        .into_iter()
        .map(EnvRestore::remove)
        .collect()
    }

    #[test]
    fn runtime_lanes_require_their_own_keys_and_only_configured_fallbacks() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _keys = clear_keys();
        let _ambient = EnvRestore::set("EXTRACT_PROVIDER", "siliconflow");
        let _ambient_rerank = EnvRestore::set("TACHI_RERANK_PROVIDER", "voyage");
        let config = deepseek_config();
        let fallback = LaneFallbackConfig {
            extract: Some(lane(vec!["EXTRACT_FALLBACK_API_KEY"])),
            ..Default::default()
        };
        let client =
            tachi_llm::LlmClient::new_with_config_and_fallbacks(config, fallback, None).unwrap();
        client.set_provider_secret_pool(
            "DEEPSEEK_API_KEY",
            (1..=3)
                .map(|index| tachi_llm::ProviderSecret {
                    key_id: format!("DEEPSEEK_API_KEY_{index}"),
                    value: format!("fixture-{index}"),
                })
                .collect(),
        );
        client.set_provider_secret("VOYAGE_API_KEY", "fixture-voyage");
        let before_index = client
            .provider_pool_statuses()
            .into_iter()
            .find(|pool| pool.logical_name == "DEEPSEEK_API_KEY")
            .unwrap()
            .current_index;
        let rows = collect_api_key_status_from_sources(
            HashSet::from(["DEEPSEEK_API_KEY".into(), "VOYAGE_API_KEY".into()]),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            Some(&client),
        );
        assert_eq!(
            client
                .provider_pool_statuses()
                .into_iter()
                .find(|pool| pool.logical_name == "DEEPSEEK_API_KEY")
                .unwrap()
                .current_index,
            before_index
        );
        let row = |name: &str| rows.iter().find(|row| row.name == name).unwrap();
        assert!(!row("SILICONFLOW_API_KEY").required);
        assert!(!row("VOYAGE_RERANK_API_KEY").required);
        assert_eq!(
            row("DEEPSEEK_API_KEY").required_for,
            ["extract", "summary", "distill", "reasoning"]
        );
        assert!(!row("DEEPSEEK_API_KEY").missing_required());
        assert_eq!(
            row("EXTRACT_FALLBACK_API_KEY").missing_required_for,
            ["extract fallback"]
        );
        assert!(row("EXTRACT_FALLBACK_API_KEY").missing_required());
        let lanes = crate::status_ops::status_health::model_lanes_json_for_running_client(
            &client.runtime_config(),
        );
        assert_eq!(
            lanes["recall_rerank_cache"]["query_generation_keys"],
            serde_json::json!(["DEEPSEEK_API_KEY"])
        );
        assert_eq!(lanes["rerank"]["provider"], "local");
        assert!(!lanes["recall_rerank_cache"]["auth_failure_hint"]
            .as_str()
            .unwrap()
            .contains("SILICONFLOW"));
    }

    #[test]
    fn summary_alias_cannot_hide_missing_extract_and_rotation_can_satisfy_chain() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _keys = clear_keys();
        let mut config = deepseek_config();
        config.extract = lane(vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]);
        config.summary = lane(vec![
            "SUMMARY_API_KEY",
            "EXTRACT_API_KEY",
            "SILICONFLOW_API_KEY",
        ]);
        config.rerank.provider = RerankProviderKind::Voyage;
        config.rerank.local_endpoint = None;
        let client = tachi_llm::LlmClient::new_with_config(config, None).unwrap();
        for key in ["DEEPSEEK_API_KEY", "VOYAGE_API_KEY", "SUMMARY_API_KEY"] {
            client.set_provider_secret(key, "fixture-key");
        }
        let load = |vault| {
            collect_api_key_status_from_sources(
                vault,
                HashMap::new(),
                HashMap::from([("SUMMARY_API_KEY".into(), "summary-only".into())]),
                HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                Some(&client),
            )
        };
        let mut vault = HashSet::from(["DEEPSEEK_API_KEY".into(), "VOYAGE_API_KEY".into()]);
        let rows = load(vault.clone());
        let sf = rows
            .iter()
            .find(|row| row.name == "SILICONFLOW_API_KEY")
            .unwrap();
        assert_eq!(
            sf.status, "configured",
            "inventory still observes the summary alias"
        );
        assert_eq!(sf.missing_required_for, ["extract"]);
        assert!(sf.missing_required());
        assert!(!rows
            .iter()
            .any(|row| row.missing_required_for.iter().any(|lane| lane == "rerank")));

        vault.insert("SILICONFLOW_API_KEY_1".into());
        client.set_provider_secret_pool(
            "SILICONFLOW_API_KEY",
            vec![tachi_llm::ProviderSecret {
                key_id: "SILICONFLOW_API_KEY_1".into(),
                value: "fixture-siliconflow".into(),
            }],
        );
        assert!(!load(vault).iter().any(ApiKeyStatus::missing_required));
    }

    #[test]
    fn offline_vault_inventory_does_not_prove_lane_availability() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _keys = clear_keys();
        let _extract = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
        let _summary = EnvRestore::set("SUMMARY_PROVIDER", "deepseek");
        let _alias = EnvRestore::set("DEEPSEEK_API_KEY", "vault:DEEPSEEK_API_KEY_1");
        let load = |vault| {
            collect_api_key_status_from_sources(
                vault,
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                None,
            )
        };
        let rows = load(HashSet::from([
            "VOYAGE_API_KEY".into(),
            "DEEPSEEK_API_KEY".into(),
        ]));
        let mut missing_lanes: Vec<_> = rows
            .iter()
            .flat_map(|row| row.missing_required_for.iter().map(String::as_str))
            .collect();
        missing_lanes.sort_unstable();
        assert_eq!(
            missing_lanes,
            ["distill", "extract", "reasoning", "summary"]
        );
        let rows = load(HashSet::from([
            "VOYAGE_API_KEY".into(),
            "DEEPSEEK_API_KEY_1".into(),
        ]));
        let unverified = rows
            .iter()
            .find(|row| row.name == "DEEPSEEK_API_KEY")
            .unwrap();
        assert!(!unverified.missing_required());
        assert_eq!(unverified.unverified_required_for.len(), 4);
    }

    #[test]
    fn live_snapshot_follows_materialization_name_and_acl_admission() {
        let (mut server, _home) = crate::tests::make_server_with_temp_home();
        let _keys = clear_keys();
        let _keychain = EnvRestore::set("TACHI_TEST_FORCE_KEYCHAIN_MISSING", "1");
        let _alias = EnvRestore::set("DEEPSEEK_API_KEY", "vault:provider-secret");
        let _voyage = EnvRestore::set("VOYAGE_API_KEY", "fixture-voyage");
        server.llm = std::sync::Arc::new(
            tachi_llm::LlmClient::new_with_config(
                deepseek_config(),
                Some(&server.global_db_path_buf()),
            )
            .unwrap(),
        );
        use base64::Engine as _;
        let salt = crate::vault_crypto::generate_salt();
        let key = crate::vault_crypto::derive_cheap("health-snapshot-fixture", &salt).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let config = memcore::vault::VaultConfig {
            salt: base64::engine::general_purpose::STANDARD.encode(salt),
            verifier: crate::vault_crypto::create_verifier(key.bytes()).unwrap(),
            kdf_algorithm: "argon2id".into(),
            kdf_params: crate::vault_crypto::cheap_kdf_params_json().to_string(),
            cipher: memcore::vault::VaultCipher::Aes256Gcm,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        // Prepare ciphertext only; admission and publication below use the
        // production scanner/materializer, not retired Vault test handlers.
        server
            .with_global_store(|store| {
                store
                    .vault_set_config(&config)
                    .map_err(|error| error.to_string())?;
                for name in ["provider-secret", "DEEPSEEK_API_KEY_1"] {
                    let (encrypted_value, nonce) = crate::vault_crypto::encrypt(
                        key.bytes(),
                        format!("fixture-{name}").as_bytes(),
                    )?;
                    store
                        .vault_upsert_entry(&memcore::vault::VaultEntry {
                            name: name.into(),
                            encrypted_value,
                            nonce,
                            secret_type: "api_key".into(),
                            created_at: now.clone(),
                            updated_at: now.clone(),
                            ..Default::default()
                        })
                        .map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(())
            })
            .unwrap();
        {
            let mut state = server.vault_write();
            state.key = Some(memory_server_runtime::CachedVaultKey::copy_from(
                key.bytes(),
            ));
            state.unlock_time = Some(std::time::Instant::now());
        }
        let snapshot = || {
            crate::status_ops::collect_snapshot(
                &server.tachi_home_dir(),
                &server.global_db_path_buf(),
                None,
                Some(&server.llm),
            )
        };
        let assert_missing = |snapshot: &crate::status_ops::StatusSnapshot| {
            let key = snapshot
                .api_keys
                .iter()
                .find(|key| key.name == "DEEPSEEK_API_KEY")
                .unwrap();
            assert_eq!(
                key.missing_required_for,
                ["extract", "summary", "distill", "reasoning"]
            );
            assert!(key.unverified_required_for.is_empty());
            assert!(
                crate::status_ops::build_status_warnings(snapshot, &serde_json::json!({}))
                    .iter()
                    .any(|line| line.contains("DEEPSEEK_API_KEY") && line.contains("extract"))
            );
        };
        let rejected = crate::provider_config::materialize_for_server(&server).unwrap();
        assert_eq!(
            rejected.skip_class_for("DEEPSEEK_API_KEY"),
            tachi_llm::AliasSkipClass::ListedNotModelProvider
        );
        assert_missing(&snapshot());

        // Use a name the real scanner and ModelApi registry can actually load.
        let _valid_alias = EnvRestore::set("DEEPSEEK_API_KEY", "vault:DEEPSEEK_API_KEY_1");
        crate::provider_config::materialize_for_server(&server).unwrap();
        let access = || {
            server
                .with_global_store_read(|store| {
                    let entry = store
                        .vault_get_entry("DEEPSEEK_API_KEY_1")
                        .map_err(|error| error.to_string())?
                        .unwrap();
                    Ok::<_, String>((entry.accessed_at, entry.access_count))
                })
                .unwrap()
        };
        let before = access();
        let accepted = snapshot();
        let key = accepted
            .api_keys
            .iter()
            .find(|key| key.name == "DEEPSEEK_API_KEY")
            .unwrap();
        assert!(!key.missing_required());
        assert!(key.unverified_required_for.is_empty());
        assert_eq!(
            access(),
            before,
            "status must not touch Vault access accounting"
        );

        server
            .with_global_store(|store| {
                let mut entry = store
                    .vault_get_entry("DEEPSEEK_API_KEY_1")
                    .map_err(|error| error.to_string())?
                    .unwrap();
                entry.allowed_agents = Some(vec!["isolated-agent".into()]);
                store
                    .vault_upsert_entry(&entry)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let fenced = crate::provider_config::materialize_for_server(&server).unwrap();
        assert_eq!(
            fenced.skip_class_for("DEEPSEEK_API_KEY"),
            tachi_llm::AliasSkipClass::ListedFenced
        );
        assert_missing(&snapshot());
    }
}
