use memory_core::vault::VaultKeyHealth;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::rotation::{build_rotation_status, rotation_member_names, RotationSourceStatus};
use super::types::{ProviderProbeCache, ProviderRotationGroupProbe};
use super::vault::load_keychain_vault_api_key_values;
use crate::status_ops::ApiKeyStatus;

pub(crate) struct ApiKeyDef {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) canonical_key: &'static str,
    pub(crate) aliases: &'static [&'static str],
}

pub(crate) const API_KEY_DEFS: &[ApiKeyDef] = &[
    ApiKeyDef {
        key: "VOYAGE_API_KEY",
        label: "Voyage embeddings",
        required: true,
        deprecated: false,
        canonical_key: "VOYAGE_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "VOYAGE_RERANK_API_KEY",
        label: "Voyage rerank",
        required: false,
        deprecated: false,
        canonical_key: "VOYAGE_RERANK_API_KEY",
        aliases: &["VOYAGE_API_KEY"],
    },
    ApiKeyDef {
        key: "SILICONFLOW_API_KEY",
        label: "SiliconFlow/Qwen background LLM",
        required: true,
        deprecated: false,
        canonical_key: "SILICONFLOW_API_KEY",
        aliases: &[
            "EXTRACT_API_KEY",
            "SUMMARY_API_KEY",
            "DISTILL_API_KEY",
            "REASONING_API_KEY",
        ],
    },
    ApiKeyDef {
        key: "DEEPSEEK_API_KEY",
        label: "DeepSeek OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "DEEPSEEK_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "ZAI_API_KEY",
        label: "Zhipu/BigModel OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "ZAI_API_KEY",
        aliases: &["BIGMODEL_API_KEY"],
    },
    ApiKeyDef {
        key: "OPENAI_API_KEY",
        label: "OpenAI-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "OPENAI_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "ANTHROPIC_API_KEY",
        label: "Anthropic/Claude-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "ANTHROPIC_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "GOOGLE_API_KEY",
        label: "Google/Gemini-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "GOOGLE_API_KEY",
        aliases: &["GEMINI_API_KEY"],
    },
    ApiKeyDef {
        key: "EXA_API_KEY",
        label: "Exa search",
        required: false,
        deprecated: false,
        canonical_key: "EXA_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "TAVILY_API_KEY",
        label: "Tavily search",
        required: false,
        deprecated: false,
        canonical_key: "TAVILY_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "MINIMAX_API_KEY",
        label: "MiniMax legacy distill",
        required: false,
        deprecated: true,
        canonical_key: "SILICONFLOW_API_KEY",
        aliases: &[],
    },
    ApiKeyDef {
        key: "REASONING_API_KEY",
        label: "Legacy reasoning lane",
        required: false,
        deprecated: true,
        canonical_key: "SILICONFLOW_API_KEY",
        aliases: &["ZAI_API_KEY", "BIGMODEL_API_KEY"],
    },
];

pub(crate) fn collect_api_key_status(global_db_path: &Path) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, false, None)
}

pub(crate) fn collect_api_key_status_with_value_compare(
    global_db_path: &Path,
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, true, None)
}

pub(crate) fn collect_api_key_status_with_probe_cache(
    global_db_path: &Path,
    probe_cache: Option<&ProviderProbeCache>,
    compare_vault_values: bool,
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, compare_vault_values, probe_cache)
}

fn collect_api_key_status_inner(
    global_db_path: &Path,
    compare_vault_values: bool,
    probe_cache: Option<&ProviderProbeCache>,
) -> Vec<ApiKeyStatus> {
    let mut vault_names = HashSet::new();
    let mut rotation_rows = Vec::new();
    let mut key_health_rows = Vec::new();
    if let Some(path) = global_db_path.to_str() {
        if let Ok(store) = memory_core::MemoryStore::open_read_only(path) {
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
    let config_env = crate::provider_config::collect_config_env_values();
    let vault_values: HashMap<String, String> = if compare_vault_values {
        load_keychain_vault_api_key_values(global_db_path)
            .unwrap_or_default()
            .into_iter()
            .collect()
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
    )
}

pub(super) fn collect_api_key_status_from_sources(
    vault_names: HashSet<String>,
    vault_values: HashMap<String, String>,
    config_env: HashMap<String, String>,
    rotations: HashMap<String, RotationSourceStatus>,
    key_health: &HashMap<String, HashMap<String, VaultKeyHealth>>,
    rotation_probes: &HashMap<String, &ProviderRotationGroupProbe>,
) -> Vec<ApiKeyStatus> {
    let now = chrono::Utc::now();
    API_KEY_DEFS
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
        .collect()
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
