use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::json;

use super::{ApiKeyStatus, DbStatus, EXPECTED_EMBEDDING_DIM};

pub(crate) struct ApiKeyDef {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) aliases: &'static [&'static str],
}

pub(crate) const API_KEY_DEFS: &[ApiKeyDef] = &[
    ApiKeyDef {
        key: "VOYAGE_API_KEY",
        label: "Voyage embeddings",
        required: true,
        deprecated: false,
        aliases: &[],
    },
    ApiKeyDef {
        key: "VOYAGE_RERANK_API_KEY",
        label: "Voyage rerank",
        required: false,
        deprecated: false,
        aliases: &["VOYAGE_API_KEY"],
    },
    ApiKeyDef {
        key: "SILICONFLOW_API_KEY",
        label: "SiliconFlow/Qwen background LLM",
        required: true,
        deprecated: false,
        aliases: &[
            "EXTRACT_API_KEY",
            "SUMMARY_API_KEY",
            "DISTILL_API_KEY",
            "REASONING_API_KEY",
        ],
    },
    ApiKeyDef {
        key: "MINIMAX_API_KEY",
        label: "MiniMax legacy distill",
        required: false,
        deprecated: true,
        aliases: &[],
    },
    ApiKeyDef {
        key: "REASONING_API_KEY",
        label: "Legacy reasoning lane",
        required: false,
        deprecated: true,
        aliases: &["ZAI_API_KEY", "BIGMODEL_API_KEY"],
    },
];

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProviderProbeResult {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
}

pub(crate) fn provider_key_status_json(global_db_path: &Path) -> serde_json::Value {
    json!(collect_api_key_status(global_db_path))
}

pub(crate) fn model_lanes_json() -> serde_json::Value {
    json!({
        "embedding": {
            "provider": "voyage",
            "model": "voyage-4",
            "expected_dimension": EXPECTED_EMBEDDING_DIM,
            "key": "VOYAGE_API_KEY",
        },
        "rerank": {
            "provider": "voyage",
            "model": "rerank-2.5",
            "keys": ["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"],
        },
        "recall_rerank_cache": {
            "query_generation_provider": "extract/SiliconFlow",
            "rerank_provider": "voyage",
            "auth_failure_hint": "403 during query generation points to SILICONFLOW_API_KEY; 403 during Voyage rerank points to VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY",
        },
        "extract": {
            "provider": "openai-compatible",
            "default_base_url": "https://api.siliconflow.cn/v1/chat/completions",
            "default_model": "Qwen/Qwen3.5-27B",
            "keys": ["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "summary": {
            "provider": "openai-compatible",
            "inherits": "extract",
            "keys": ["SUMMARY_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "distill": {
            "provider": "raw_api default (FOUNDRY_DISTILL_BACKEND), claude_cli optional",
            "keys": ["DISTILL_API_KEY", "REASONING_API_KEY", "ZAI_API_KEY", "BIGMODEL_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "reasoning": {
            "provider": "claude-cli-first, openai-compatible fallback",
            "keys": ["REASONING_API_KEY", "ZAI_API_KEY", "BIGMODEL_API_KEY", "DISTILL_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        }
    })
}

pub(crate) async fn run_provider_probes(global_db_path: &Path) -> Vec<ProviderProbeResult> {
    let llm = match crate::llm::LlmClient::new() {
        Ok(client) => client,
        Err(err) => {
            return vec![ProviderProbeResult {
                name: "llm_client".to_string(),
                status: "failed".to_string(),
                message: Some(err),
            }];
        }
    };
    if let Ok(secrets) = load_keychain_vault_api_key_values(global_db_path) {
        llm.set_provider_secrets(secrets);
    }

    // Run all three probes concurrently — reduces worst-case latency from
    // 15+15+20 = 50s to max(15,15,20) = 20s.
    let rerank_docs = vec![
        "Tachi stores operational memory".to_string(),
        "Unrelated weather note".to_string(),
    ];
    let llm_embed = llm.clone();
    let llm_rerank = llm.clone();
    let (embed, rerank, chat) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            llm_embed.embed_voyage("tachi provider probe", "document"),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            llm_rerank.rerank_voyage("tachi provider probe", &rerank_docs, 1),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            llm.call_extract_llm(
                "Return exactly OK.",
                "Provider probe. Reply OK only.",
                None,
                0.0,
                8,
            ),
        ),
    );

    let mut out = Vec::new();
    out.push(match embed {
        Ok(Ok(vec)) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "ok".to_string(),
            message: Some(format!("{} dims", vec.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });
    out.push(match rerank {
        Ok(Ok(rows)) => ProviderProbeResult {
            name: "voyage_rerank".to_string(),
            status: "ok".to_string(),
            message: Some(format!("{} result(s)", rows.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "voyage_rerank".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "voyage_rerank".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });
    out.push(match chat {
        Ok(Ok(text)) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "ok".to_string(),
            message: Some(text.chars().take(80).collect()),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 20s".to_string()),
        },
    });
    out
}

pub(crate) fn load_keychain_vault_api_key_values(
    vault_db_path: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Ok(Vec::new());
    }

    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let password = String::from_utf8(output.stdout)?.trim().to_string();
    if password.is_empty() {
        return Ok(Vec::new());
    }

    let vault_db_str = vault_db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Vault DB path contains invalid UTF-8: {}",
                vault_db_path.display()
            ),
        )
    })?;
    let store = memory_core::MemoryStore::open_read_only(vault_db_str)?;
    let Some(config) = store.vault_get_config()? else {
        return Ok(Vec::new());
    };

    let salt = B64.decode(&config.salt)?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;
    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in store.vault_list_entries()? {
        if entry.secret_type != "api_key"
            || !entry.name.ends_with("_API_KEY")
            || entry.allowed_agents.is_some()
        {
            continue;
        }
        let decrypted = crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)?;
        if !value.trim().is_empty() {
            out.push((entry.name, value));
        }
    }
    Ok(out)
}

pub(crate) fn collect_api_key_status(global_db_path: &Path) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, false)
}

pub(crate) fn collect_api_key_status_with_value_compare(
    global_db_path: &Path,
) -> Vec<ApiKeyStatus> {
    collect_api_key_status_inner(global_db_path, true)
}

fn collect_api_key_status_inner(
    global_db_path: &Path,
    compare_vault_values: bool,
) -> Vec<ApiKeyStatus> {
    let mut vault_names = HashSet::new();
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
        }
    }
    let config_env = crate::provider_config::collect_config_env_values();
    let vault_values: HashMap<String, String> = if compare_vault_values {
        load_keychain_vault_api_key_values(global_db_path)
            .unwrap_or_default()
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };

    collect_api_key_status_from_sources(vault_names, vault_values, config_env)
}

fn collect_api_key_status_from_sources(
    vault_names: HashSet<String>,
    vault_values: HashMap<String, String>,
    config_env: HashMap<String, String>,
) -> Vec<ApiKeyStatus> {
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
            let vault_configured = vault_names.contains(def.key);
            let config_value = config_env.get(def.key);
            let config_present = config_value.is_some();
            let config_is_vault_alias = config_value
                .map(|value| crate::provider_config::is_vault_alias(value))
                .unwrap_or(false);
            let config_alias_resolves = config_value
                .and_then(|value| crate::provider_config::parse_vault_alias(value))
                .map(|target| vault_names.contains(target))
                .unwrap_or(false);
            let env_plaintext = env_value.as_deref().and_then(|value| {
                let value = value.trim();
                if value.is_empty() || crate::provider_config::is_vault_alias(value) {
                    None
                } else {
                    Some(value)
                }
            });
            let config_plaintext = config_value.and_then(|value| {
                let value = value.trim();
                if value.is_empty() || crate::provider_config::is_vault_alias(value) {
                    None
                } else {
                    Some(value)
                }
            });
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
            } else if vault_configured && ((env_configured && !env_is_vault_alias) || (file_configured && !config_is_vault_alias))
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
            ApiKeyStatus {
                name: def.key.to_string(),
                label: def.label.to_string(),
                required: def.required,
                deprecated: def.deprecated,
                status: status.to_string(),
                source,
                env_configured,
                vault_configured,
                drift_warning,
                inferred_invalid_provider: None,
            }
        })
        .collect()
}

pub(crate) fn calculate_health_score(
    daemon: &crate::status_ops::DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&crate::status_ops::DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
    probe_results: Option<&[ProviderProbeResult]>,
) -> u8 {
    let mut score = 100i32;
    if !matches!(daemon, crate::status_ops::DaemonStatus::Running { .. }) {
        score -= 20;
    }
    let failed_jobs: usize = dbs.iter().map(|db| db.failed).sum();
    score -= (failed_jobs as i32).min(25);
    let stuck_jobs: usize = dbs.iter().map(|db| db.stuck_in_progress).sum();
    score -= ((stuck_jobs as i32) * 5).min(20);
    let low_vector_dbs = dbs
        .iter()
        .filter(|db| db.memory_total > 0 && db.vector_coverage < 0.9)
        .count();
    score -= ((low_vector_dbs as i32) * 10).min(25);
    let dim_mismatch_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::vector_dimension_mismatch(db))
        .count();
    score -= ((dim_mismatch_dbs as i32) * 15).min(30);
    if distill_marker.map(|m| m.is_stale).unwrap_or(true) {
        score -= 10;
    }
    let missing_required_keys = api_keys
        .iter()
        .filter(|key| key.required && key.status == "missing")
        .count();
    score -= ((missing_required_keys as i32) * 10).min(20);
    // Vault+env duplicate keys are informational (see api_keys.drift), not health defects.
    let inferred_invalid_keys = api_keys
        .iter()
        .filter(|key| key.inferred_invalid_provider.is_some())
        .count();
    score -= ((inferred_invalid_keys as i32) * 10).min(20);
    // Provider probe failures indicate misconfigured or expired API keys.
    if let Some(probes) = probe_results {
        let failed_probes = probes.iter().filter(|p| p.status != "ok").count();
        score -= ((failed_probes as i32) * 8).min(24);
    }
    score.clamp(0, 100) as u8
}

pub(crate) fn format_elapsed(dur: chrono::Duration) -> String {
    let secs = dur.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn is_auth_error(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || (lower.contains("api key") && lower.contains("invalid"))
        || lower.contains("permission denied")
}

pub(crate) fn infer_provider_from_failed_job(
    kind: &str,
    lane: Option<&str>,
    reason: &str,
) -> Option<String> {
    let explicit = infer_provider_from_auth_error(reason);
    if explicit
        .as_deref()
        .is_some_and(|provider| provider != "UNKNOWN")
    {
        return explicit;
    }
    if !is_auth_error(reason) {
        return None;
    }
    if kind == "recall_rerank_cache" || kind == "memory_distill" {
        return Some("SILICONFLOW".to_string());
    }
    match lane.unwrap_or_default() {
        "rerank" | "embedding" => Some("VOYAGE".to_string()),
        "extract" | "extraction" | "summary" | "distill" | "reasoning" => {
            Some("SILICONFLOW".to_string())
        }
        _ => explicit,
    }
}

pub(crate) fn infer_provider_from_auth_error(reason: &str) -> Option<String> {
    if !is_auth_error(reason) {
        return None;
    }
    let lower = reason.to_ascii_lowercase();
    if lower.contains("voyage") || lower.contains("voyageai") {
        Some("VOYAGE".to_string())
    } else if lower.contains("siliconflow") || lower.contains("qwen") {
        Some("SILICONFLOW".to_string())
    } else if lower.contains("minimax") {
        Some("MINIMAX".to_string())
    } else if lower.contains("zai") || lower.contains("bigmodel") || lower.contains("glm") {
        Some("REASONING".to_string())
    } else {
        Some("UNKNOWN".to_string())
    }
}

fn provider_to_key(provider: &str) -> Option<&'static str> {
    match provider {
        "VOYAGE" => Some("VOYAGE_API_KEY"),
        "SILICONFLOW" => Some("SILICONFLOW_API_KEY"),
        "MINIMAX" => Some("MINIMAX_API_KEY"),
        "REASONING" => Some("REASONING_API_KEY"),
        _ => None,
    }
}

pub(crate) fn apply_inferred_provider_failures(api_keys: &mut [ApiKeyStatus], dbs: &[DbStatus]) {
    let mut providers = HashSet::new();
    for db in dbs {
        if let Some(provider) = db
            .latest_failed_job
            .as_ref()
            .and_then(|job| job.inferred_invalid_provider.as_deref())
        {
            providers.insert(provider.to_string());
        }
    }
    for provider in providers {
        if let Some(key_name) = provider_to_key(&provider) {
            if let Some(key) = api_keys.iter_mut().find(|key| key.name == key_name) {
                key.inferred_invalid_provider = Some(provider.clone());
            }
        }
    }
}

pub(crate) fn agent_readiness_json(
    app_home: &std::path::Path,
    snapshot: &crate::status_ops::StatusSnapshot,
) -> serde_json::Value {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let agent_rule_files = vec![
        ("claude", home.join(".claude").join("CLAUDE.md")),
        ("codex", home.join(".codex").join("AGENTS.md")),
        ("gemini", home.join(".gemini").join("GEMINI.md")),
    ];
    let rules: Vec<_> = agent_rule_files
        .into_iter()
        .map(|(agent, path)| {
            let installed = std::fs::read_to_string(&path)
                .ok()
                .is_some_and(|body| body.contains("BEGIN TACHI MEMORY RULES"));
            json!({
                "agent": agent,
                "path": path.display().to_string(),
                "exists": path.exists(),
                "tachi_rules_installed": installed,
            })
        })
        .collect();
    let mcp_configs = vec![
        ("claude", home.join(".claude").join("mcp.json")),
        ("cursor", home.join(".cursor").join("mcp.json")),
        ("gemini", home.join(".gemini").join("mcp.json")),
        (
            "amp",
            home.join("Library/Application Support/Amp/settings.json"),
        ),
    ];
    let mcp: Vec<_> = mcp_configs
        .into_iter()
        .map(|(agent, path)| {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            json!({
                "agent": agent,
                "path": path.display().to_string(),
                "exists": path.exists(),
                "mentions_tachi": raw.to_ascii_lowercase().contains("tachi") || raw.contains("memory-server"),
            })
        })
        .collect();
    json!({
        "rules": rules,
        "mcp_configs": mcp,
        "runs_root": crate::shell_ops::shell_runs_root().display().to_string(),
        "last_distill_marker": snapshot.distill_marker.as_ref().map(|m| m.path.clone()),
        "doctor_hint": format!("tachi doctor --jobs --probe-keys --roots {}", shell_quote(&app_home.display().to_string())),
    })
}

pub(crate) fn format_backfill_command(db: &DbStatus) -> String {
    format!("tachi backfill-vectors --db {}", shell_quote(&db.path))
}

pub(crate) fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_key_row<'a>(rows: &'a [ApiKeyStatus], name: &str) -> &'a ApiKeyStatus {
        rows.iter()
            .find(|row| row.name == name)
            .expect("api key row should exist")
    }

    fn restore_env(name: &str, original: Option<std::ffi::OsString>) {
        if let Some(value) = original {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn vault_plaintext_duplicate_with_same_value_is_not_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("VOYAGE_API_KEY");
        std::env::set_var("VOYAGE_API_KEY", "same-secret");

        let rows = collect_api_key_status_from_sources(
            HashSet::from(["VOYAGE_API_KEY".to_string()]),
            HashMap::from([("VOYAGE_API_KEY".to_string(), "same-secret".to_string())]),
            HashMap::new(),
        );
        let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

        assert_eq!(voyage.status, "configured");
        assert_eq!(voyage.source, "vault+env(same)");
        assert!(voyage
            .drift_warning
            .as_deref()
            .is_some_and(|warning| warning.starts_with("redundant:")));

        restore_env("VOYAGE_API_KEY", original);
    }

    #[test]
    fn vault_plaintext_duplicate_with_different_value_is_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("VOYAGE_API_KEY");
        std::env::set_var("VOYAGE_API_KEY", "env-secret");

        let rows = collect_api_key_status_from_sources(
            HashSet::from(["VOYAGE_API_KEY".to_string()]),
            HashMap::from([("VOYAGE_API_KEY".to_string(), "vault-secret".to_string())]),
            HashMap::new(),
        );
        let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

        assert_eq!(voyage.status, "drift");
        assert_eq!(voyage.source, "vault+env");
        assert!(voyage
            .drift_warning
            .as_deref()
            .is_some_and(|warning| warning.starts_with("drift:")));

        restore_env("VOYAGE_API_KEY", original);
    }

    #[test]
    fn vault_plaintext_duplicate_without_decrypted_value_is_unverified_not_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("VOYAGE_API_KEY");
        std::env::set_var("VOYAGE_API_KEY", "env-secret");

        let rows = collect_api_key_status_from_sources(
            HashSet::from(["VOYAGE_API_KEY".to_string()]),
            HashMap::new(),
            HashMap::new(),
        );
        let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

        assert_eq!(voyage.status, "configured");
        assert_eq!(voyage.source, "vault+env(unverified)");
        assert!(voyage
            .drift_warning
            .as_deref()
            .is_some_and(|warning| warning.starts_with("duplicate-unverified:")));

        restore_env("VOYAGE_API_KEY", original);
    }
}
