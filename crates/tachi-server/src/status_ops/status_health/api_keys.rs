use memcore::vault::VaultKeyHealth;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::rotation::{build_rotation_status, rotation_member_names, RotationSourceStatus};
use super::types::{ProviderProbeCache, ProviderRotationGroupProbe};
use super::vault::load_keychain_vault_api_key_values;
use crate::status_ops::ApiKeyStatus;

/// Registry-wide class of an admitted env-var-name secret. This is the
/// compile-time security boundary #1680/D3 introduces: `ModelApi` keys are
/// the only names eligible for the LLM provider materialization allowlist
/// (`model_provider_env_names()`); every class is eligible for the broader
/// "does this name belong to Tachi's provider vocabulary at all" surfaces
/// (lane env injection, providers-doctor admission, the plaintext secret
/// scanner — `admitted_env_secret_names()`). Widening which names are
/// `ModelApi` is a code-review-gated change, never a DB write (D3: "the
/// env-name admission surface stays compile-time").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyClass {
    /// LLM / embedding / reranking provider credentials — eligible for LLM
    /// provider-secret materialization.
    ModelApi,
    /// Web-search provider credentials (Exa, Tavily, Google Programmable
    /// Search) — never materialized into the LLM provider cache.
    SearchApi,
    /// Reserved for non-provider infrastructure secrets. No current
    /// `API_KEY_DEFS` entry uses this class; kept so the class vocabulary
    /// does not need another compile-time change when one shows up.
    #[allow(dead_code)]
    Infra,
}

/// The documented, anti-SSRF-safe probe target for one provider family.
///
/// #1680/D6: this used to be a second hardcoded copy of the host/endpoint
/// strings that `tachi_llm`'s `auth_probe` already had. It is now the same
/// table — the registry's rows reference `tachi_llm`'s compile-time
/// descriptors. The direction is forced by the crate graph (`tachi-llm` must
/// never depend on `tachi-server`) and is the right one anyway: the strings
/// live next to the code that dials them, and what the registry contributes is
/// the part it owns — *which env-var names* are probeable, and as which family.
/// `status_health::auth_probe_descriptor_for_env_name` is that lookup;
/// `registry_probe_targets_match_the_probe_table` pins the two against drift.
pub(crate) type ProbeDescriptor = tachi_llm::ProviderProbeDescriptor;

pub(crate) struct ApiKeyDef {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) canonical_key: &'static str,
    pub(crate) aliases: &'static [&'static str],
    /// ModelApi vs SearchApi vs Infra — see [`KeyClass`].
    pub(crate) class: KeyClass,
    /// Canonical family id for the underlying account/vendor (e.g.
    /// "deepseek", "anthropic", "google"), independent of which of this
    /// entry's env-var names holds the secret. This is the vocabulary
    /// #1680 D2's `fp1` and D5's account rows are keyed by; reconcile reads
    /// it through `status_health::provider_kind_for_env_name`.
    pub(crate) provider_kind: &'static str,
    /// `Some` only for the families `auth_probe` has an owner-verified,
    /// non-generating probe target for. Read through
    /// `status_health::auth_probe_descriptor_for_env_name`, which is what makes
    /// the probe surface registry-driven rather than a second host list.
    pub(crate) probe: Option<&'static ProbeDescriptor>,
}

pub(crate) const API_KEY_DEFS: &[ApiKeyDef] = &[
    ApiKeyDef {
        key: "VOYAGE_API_KEY",
        label: "Voyage embeddings",
        required: true,
        deprecated: false,
        canonical_key: "VOYAGE_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "voyage",
        probe: None,
    },
    ApiKeyDef {
        key: "VOYAGE_RERANK_API_KEY",
        label: "Voyage rerank",
        required: false,
        deprecated: false,
        canonical_key: "VOYAGE_RERANK_API_KEY",
        aliases: &["VOYAGE_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "voyage",
        probe: None,
    },
    ApiKeyDef {
        key: "SILICONFLOW_API_KEY",
        label: "SiliconFlow/Qwen background LLM",
        required: true,
        deprecated: false,
        canonical_key: "SILICONFLOW_API_KEY",
        aliases: &["EXTRACT_API_KEY", "SUMMARY_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "siliconflow",
        probe: Some(&tachi_llm::SILICONFLOW_AUTH_PROBE),
    },
    // #1680/D2: `REASONING_API_KEY` used to sit in this alias list as well,
    // which made it the one env-var name in the registry that resolved to two
    // provider families — "deepseek" through this row, "zai" through its own
    // row below (whose accepted aliases are `ZAI_API_KEY`/`BIGMODEL_API_KEY`,
    // and which is how the shipped configs actually use the name:
    // `${vault:ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY}` in
    // `builtins::mcp`). `aliases` here means "another name for this account",
    // which is what `provider_kind` and D2's `fp1` are keyed by; it is not the
    // lane resolver's fallback chain. That chain
    // (DEEPSEEK → REASONING → ZAI → …) lives in
    // `tachi_llm::llm::provider_health::config` and is unchanged — a reasoning
    // lane still falls back to a Z.AI secret, it just no longer claims the
    // DeepSeek *account* is configured because a Z.AI key exists.
    // `DISTILL_API_KEY` stays: it is genuinely the same DeepSeek family
    // (its own row also declares `provider_kind: "deepseek"`).
    ApiKeyDef {
        key: "DEEPSEEK_API_KEY",
        label: "DeepSeek OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "DEEPSEEK_API_KEY",
        aliases: &["DISTILL_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "deepseek",
        probe: Some(&tachi_llm::DEEPSEEK_AUTH_PROBE),
    },
    ApiKeyDef {
        key: "DISTILL_API_KEY",
        label: "Distill API lane override before provider fallbacks",
        required: false,
        deprecated: false,
        canonical_key: "DISTILL_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "deepseek",
        probe: None,
    },
    ApiKeyDef {
        key: "ZAI_API_KEY",
        label: "Zhipu/BigModel OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "ZAI_API_KEY",
        aliases: &["BIGMODEL_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "zai",
        // auth_probe recognizes two hosts for this family
        // (open.bigmodel.cn, api.z.ai) with no documented non-generating GET
        // endpoint for either; this names the current primary domain, whose
        // descriptor carries `endpoint: None` — recognized, never probed.
        probe: Some(&tachi_llm::ZAI_AUTH_PROBE),
    },
    // #1355: the grok/xai opencode lane provider must be a recognized
    // provider-key name so an unlocked-vault xAI secret is injected into the
    // lane subprocess env (via `load_unlocked_provider_env_secrets`, gated by
    // `admitted_env_secret_names()`, #1680/D3). That lets `opencode.json`'s `xai`
    // provider use `{env:XAI_API_KEY}` substitution — no literal secret on
    // disk for vault "收权" to blank. `GROK_API_KEY` is carried as an alias
    // (alternate ecosystem name, cf. GOOGLE/GEMINI) so whichever name the
    // owner stored the vault secret under is admitted by the filter; the
    // injected env-var name is always the vault entry's own name, so that
    // name must match the `{env:...}` reference in opencode.json.
    ApiKeyDef {
        key: "XAI_API_KEY",
        label: "xAI/Grok OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "XAI_API_KEY",
        aliases: &["GROK_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "xai",
        probe: None,
    },
    // #1355(b): the opencode `zhipuai-coding-plan` GLM lane provider must
    // also be a recognized provider-key name so an unlocked-vault ZHIPUAI
    // secret is injected into the lane subprocess env (via
    // `load_unlocked_provider_env_secrets`, gated by
    // `admitted_env_secret_names()`, #1680/D3). That lets `opencode.json`'s
    // `zhipuai-coding-plan` provider use `{env:ZHIPUAI_API_KEY}`
    // substitution — no literal secret on disk for vault "收权" to blank.
    // This is deliberately separate from the `ZAI_API_KEY` entry above:
    // the vault stores a distinct `ZHIPUAI_API_KEY` secret (verified by SHA
    // match against the working opencode literal) that is NOT the same
    // value as `ZAI_API_KEY`, so no alias is added either direction.
    ApiKeyDef {
        key: "ZHIPUAI_API_KEY",
        label: "Zhipu AI (GLM coding-plan) OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "ZHIPUAI_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "zhipuai",
        probe: None,
    },
    // #1355 follow-up: the `kimi-for-coding`/K3 opencode lane provider must
    // also be a recognized provider-key name so an unlocked-vault Kimi
    // secret is injected into the lane subprocess env (via
    // `load_unlocked_provider_env_secrets`, gated by
    // `admitted_env_secret_names()`, #1680/D3). That lets the lane's provider config
    // use `{env:KIMI_API_KEY}` substitution (or direct env read) — no
    // literal secret on disk for vault "收权" to blank. `MOONSHOT_API_KEY`
    // is carried as an alias (alternate ecosystem name — Moonshot AI is
    // Kimi's vendor) so whichever name the owner stored the vault secret
    // under is admitted by the filter; the injected env-var name is always
    // the vault entry's own name, so that name must match the lane's
    // `{env:...}` reference.
    ApiKeyDef {
        key: "KIMI_API_KEY",
        label: "Kimi/Moonshot OpenAI-compatible LLM",
        required: false,
        deprecated: false,
        canonical_key: "KIMI_API_KEY",
        aliases: &["MOONSHOT_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "kimi",
        probe: None,
    },
    ApiKeyDef {
        key: "OPENAI_API_KEY",
        label: "OpenAI-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "OPENAI_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "openai",
        probe: None,
    },
    ApiKeyDef {
        key: "ANTHROPIC_API_KEY",
        label: "Anthropic/Claude-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "ANTHROPIC_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "anthropic",
        probe: None,
    },
    ApiKeyDef {
        key: "GOOGLE_API_KEY",
        label: "Google/Gemini-compatible agents",
        required: false,
        deprecated: false,
        canonical_key: "GOOGLE_API_KEY",
        aliases: &["GEMINI_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "google",
        probe: None,
    },
    ApiKeyDef {
        key: "EXA_API_KEY",
        label: "Exa search",
        required: false,
        deprecated: false,
        canonical_key: "EXA_API_KEY",
        aliases: &[],
        class: KeyClass::SearchApi,
        provider_kind: "exa",
        probe: None,
    },
    ApiKeyDef {
        key: "TAVILY_API_KEY",
        label: "Tavily search",
        required: false,
        deprecated: false,
        canonical_key: "TAVILY_API_KEY",
        aliases: &[],
        class: KeyClass::SearchApi,
        provider_kind: "tavily",
        probe: None,
    },
    // #1680/D3: independent SearchApi entry — previously this name was not
    // in `API_KEY_DEFS` at all, but was still manually folded into the
    // "google/gemini" family by `intake::alias_family()` (a live
    // discrimination-2 violation: a search-only credential grouped with
    // model-provider accounts). Giving it its own registry row with no
    // aliases makes it independent by construction: it is admitted (all
    // classes are admitted-set members) but never enters the ModelApi
    // materialization allowlist, and it no longer shares an intake alias
    // family with GOOGLE_API_KEY/GEMINI_API_KEY.
    ApiKeyDef {
        key: "GOOGLE_SEARCH_API_KEY",
        label: "Google Programmable Search",
        required: false,
        deprecated: false,
        canonical_key: "GOOGLE_SEARCH_API_KEY",
        aliases: &[],
        class: KeyClass::SearchApi,
        provider_kind: "google-search",
        probe: None,
    },
    ApiKeyDef {
        key: "MINIMAX_API_KEY",
        label: "MiniMax legacy distill",
        required: false,
        deprecated: true,
        canonical_key: "DEEPSEEK_API_KEY",
        aliases: &[],
        class: KeyClass::ModelApi,
        provider_kind: "deepseek",
        probe: None,
    },
    ApiKeyDef {
        key: "REASONING_API_KEY",
        label: "Reasoning API lane after DeepSeek and before provider fallbacks",
        required: false,
        deprecated: false,
        canonical_key: "REASONING_API_KEY",
        aliases: &["ZAI_API_KEY", "BIGMODEL_API_KEY"],
        class: KeyClass::ModelApi,
        provider_kind: "zai",
        probe: None,
    },
];

pub(super) fn collect_api_key_status(global_db_path: &Path) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, false, None, None)
}

pub(super) fn collect_api_key_status_with_value_compare(
    global_db_path: &Path,
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, true, None, None)
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
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(
        global_db_path,
        compare_vault_values,
        probe_cache,
        resolved_home,
    )
}

fn collect_api_key_status_inner(
    global_db_path: &Path,
    compare_vault_values: bool,
    probe_cache: Option<&ProviderProbeCache>,
    resolved_home: Option<&Path>,
) -> Vec<ApiKeyStatus> {
    let mut vault_names = HashSet::new();
    let mut rotation_rows = Vec::new();
    let mut key_health_rows = Vec::new();
    if let Some(path) = global_db_path.to_str() {
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
