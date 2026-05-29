use std::collections::HashSet;
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

fn load_keychain_vault_api_key_values(
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
    let env_file_names = collect_config_env_key_names();

    API_KEY_DEFS
        .iter()
        .map(|def| {
            let env_configured = std::env::var(def.key)
                .ok()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
            let vault_configured = vault_names.contains(def.key);
            let config_present = env_file_names.contains(def.key);
            let alias_configured = def.aliases.iter().any(|alias| {
                vault_names.contains(*alias)
                    || env_file_names.contains(*alias)
                    || std::env::var(alias)
                        .ok()
                        .is_some_and(|value| !value.trim().is_empty())
            });
            let file_configured = config_present;
            let (status, source) = if vault_configured && (env_configured || file_configured) {
                (
                    "drift",
                    if env_configured {
                        "vault+env"
                    } else {
                        "vault+config.env"
                    },
                )
            } else if vault_configured {
                ("configured", "vault")
            } else if env_configured || file_configured || alias_configured {
                (
                    "configured",
                    if env_configured {
                        "env"
                    } else if file_configured {
                        "config.env"
                    } else {
                        "alias"
                    },
                )
            } else if def.deprecated {
                ("deprecated-unset", "none")
            } else {
                ("missing", "none")
            };
            let drift_warning = if vault_configured && (env_configured || file_configured) {
                Some("duplicate: key present in Vault and env/config.env (informational; runtime prefers Vault when unlocked)".to_string())
            } else if vault_configured && config_present {
                Some("duplicate: key name in config.env and Vault; remove config.env copy after Vault is confirmed".to_string())
            } else {
                None
            };
            ApiKeyStatus {
                name: def.key.to_string(),
                label: def.label.to_string(),
                required: def.required,
                deprecated: def.deprecated,
                status: status.to_string(),
                source: source.to_string(),
                env_configured,
                vault_configured,
                drift_warning,
                inferred_invalid_provider: None,
            }
        })
        .collect()
}

fn collect_config_env_key_names() -> HashSet<String> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".tachi").join("config.env"));
        paths.push(home.join(".sigil").join("config.env"));
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        paths.push(std::path::PathBuf::from(home).join("config.env"));
    }
    paths.push(std::path::PathBuf::from(".tachi/config.env"));
    paths.push(std::path::PathBuf::from(".sigil/config.env"));

    let mut names = HashSet::new();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if !value.trim().is_empty() {
                    names.insert(key.trim().to_string());
                }
            }
        }
    }
    names
}

pub(crate) fn calculate_health_score(
    daemon: &crate::status_ops::DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&crate::status_ops::DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
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
    score -= ((dim_mismatch_dbs as i32) * 10).min(20);
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
