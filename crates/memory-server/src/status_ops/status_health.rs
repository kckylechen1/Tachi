use memory_core::vault::VaultKeyHealth;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::json;

use super::{
    ApiKeyRotationMemberStatus, ApiKeyRotationStatus, ApiKeyStatus, DbStatus,
    EXPECTED_EMBEDDING_DIM,
};

const PROVIDER_PROBE_CACHE_TTL_SECS: i64 = 24 * 60 * 60;

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

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderProbeResult {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderRotationGroupProbe {
    pub(crate) logical_name: String,
    pub(crate) total_keys: i64,
    pub(crate) configured_keys: i64,
    pub(crate) healthy_keys: i64,
    pub(crate) rate_limited_keys: i64,
    pub(crate) auth_failed_keys: i64,
    pub(crate) current_index: i64,
    pub(crate) strategy: String,
    pub(crate) next_retry_at: Option<String>,
    pub(crate) keys: Vec<ApiKeyRotationMemberStatus>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderProbeReport {
    pub(crate) probes: Vec<ProviderProbeResult>,
    pub(crate) rotation_groups: Vec<ProviderRotationGroupProbe>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ProviderProbeCache {
    pub(crate) last_probe_at: String,
    pub(crate) ttl_seconds: i64,
    pub(crate) probes: Vec<ProviderProbeResult>,
    #[serde(default)]
    pub(crate) rotation_groups: Vec<ProviderRotationGroupProbe>,
}

impl ProviderProbeCache {
    pub(crate) fn is_stale(&self) -> bool {
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&self.last_probe_at) else {
            return true;
        };
        let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
        age.num_seconds() > self.ttl_seconds
    }
}

#[derive(Debug, Clone)]
struct RotationSourceStatus {
    total_keys: i64,
    current_index: i64,
    strategy: String,
    members: Vec<String>,
}

fn rotation_member_names(vault_names: &HashSet<String>, prefix: &str) -> Vec<String> {
    let mut members = vault_names
        .iter()
        .filter_map(|name| {
            crate::provider_config::parse_rotation_member_name(name)
                .filter(|(member_prefix, _)| *member_prefix == prefix)
                .map(|(_, idx)| (idx, name.clone()))
        })
        .collect::<Vec<_>>();
    members.sort_by(|(left_idx, left_name), (right_idx, right_name)| {
        left_idx
            .cmp(right_idx)
            .then_with(|| left_name.cmp(right_name))
    });
    members.into_iter().map(|(_, name)| name).collect()
}

fn build_rotation_status(
    source: &RotationSourceStatus,
    probe: Option<&ProviderRotationGroupProbe>,
    health_members: Option<&HashMap<String, VaultKeyHealth>>,
    now: chrono::DateTime<chrono::Utc>,
) -> ApiKeyRotationStatus {
    let probe_present = probe.is_some();
    let probed_members = probe.map(|probe| {
        probe
            .keys
            .iter()
            .map(|member| (member.name.as_str(), member))
            .collect::<HashMap<_, _>>()
    });
    let mut saw_runtime_health = false;
    let mut healthy_keys = 0;
    let mut rate_limited_keys = 0;
    let mut auth_failed_keys = 0;
    let members = source
        .members
        .iter()
        .map(|name| {
            if let Some(member) = probed_members
                .as_ref()
                .and_then(|members| members.get(name.as_str()))
                .map(|member| (*member).clone())
            {
                saw_runtime_health = true;
                match member.status.as_str() {
                    "ok" => healthy_keys += 1,
                    "rate_limited" => rate_limited_keys += 1,
                    "auth_failed" => auth_failed_keys += 1,
                    _ => {}
                }
                return member;
            }

            if let Some(health) = health_members.and_then(|members| members.get(name.as_str())) {
                saw_runtime_health = true;
                let status = if health.disabled {
                    "disabled"
                } else if health.auth_failed {
                    "auth_failed"
                } else {
                    match health.status.as_str() {
                        "exhausted" => "exhausted",
                        "rate_limited" | "cooldown" => {
                            if health
                                .cooldown_until
                                .as_deref()
                                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                                .is_some_and(|until| until.with_timezone(&chrono::Utc) > now)
                            {
                                "rate_limited"
                            } else {
                                "ok"
                            }
                        }
                        _ => "ok",
                    }
                };

                let mut message = health.last_error.clone();
                if message.is_none() && !matches!(status, "ok" | "configured") {
                    message = Some(format!("vault status: {}", health.status));
                }
                let last_probe_at = health
                    .last_attempt
                    .clone()
                    .or_else(|| health.last_success.clone())
                    .or_else(|| Some(health.updated_at.clone()));

                match status {
                    "ok" => healthy_keys += 1,
                    "rate_limited" => rate_limited_keys += 1,
                    "auth_failed" => auth_failed_keys += 1,
                    _ => {}
                }

                return ApiKeyRotationMemberStatus {
                    name: name.clone(),
                    status: status.to_string(),
                    message,
                    last_probe_at,
                };
            }

            ApiKeyRotationMemberStatus {
                name: name.clone(),
                status: "configured".to_string(),
                message: None,
                last_probe_at: None,
            }
        })
        .collect::<Vec<_>>();
    ApiKeyRotationStatus {
        total_keys: source.total_keys,
        configured_keys: members.len() as i64,
        healthy_keys: if probe_present || saw_runtime_health {
            Some(healthy_keys)
        } else {
            None
        },
        rate_limited_keys: probe
            .map(|probe| probe.rate_limited_keys)
            .unwrap_or(rate_limited_keys),
        auth_failed_keys: probe
            .map(|probe| probe.auth_failed_keys)
            .unwrap_or(auth_failed_keys),
        current_index: source.current_index,
        strategy: source.strategy.clone(),
        next_retry_at: probe.and_then(|probe| probe.next_retry_at.clone()),
        members,
    }
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

pub(crate) async fn run_provider_probe_report(global_db_path: &Path) -> ProviderProbeReport {
    let llm = match crate::llm::LlmClient::new() {
        Ok(client) => client,
        Err(err) => {
            return ProviderProbeReport {
                probes: vec![ProviderProbeResult {
                    name: "llm_client".to_string(),
                    status: "failed".to_string(),
                    message: Some(err),
                }],
                rotation_groups: run_rotation_group_probes(global_db_path).await,
            };
        }
    };
    if let Err(err) = crate::provider_config::materialize_standalone(&llm, global_db_path) {
        tracing::warn!("[provider] probe secret materialization failed: {err}");
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
    ProviderProbeReport {
        probes: out,
        rotation_groups: run_rotation_group_probes(global_db_path).await,
    }
}

pub(crate) async fn run_provider_probes(global_db_path: &Path) -> Vec<ProviderProbeResult> {
    run_provider_probe_report(global_db_path).await.probes
}

async fn run_rotation_group_probes(global_db_path: &Path) -> Vec<ProviderRotationGroupProbe> {
    let rotation_sources = collect_rotation_sources(global_db_path);
    if rotation_sources.is_empty() {
        return Vec::new();
    }
    let values = load_keychain_vault_api_key_values(global_db_path)
        .unwrap_or_default()
        .into_iter()
        .collect::<HashMap<_, _>>();
    let probed_at = chrono::Utc::now().to_rfc3339();

    let mut groups = Vec::new();
    for (logical_name, source) in rotation_sources {
        let mut keys = Vec::new();
        for key_id in &source.members {
            let (status, message) = match values.get(key_id) {
                Some(value) => probe_rotation_member(&logical_name, key_id, value.clone()).await,
                None => (
                    "unavailable".to_string(),
                    Some(
                        "Vault key material unavailable; unlock Vault/Keychain for live probe"
                            .to_string(),
                    ),
                ),
            };
            keys.push(ApiKeyRotationMemberStatus {
                name: key_id.clone(),
                status,
                message,
                last_probe_at: Some(probed_at.clone()),
            });
        }
        let healthy_keys = keys.iter().filter(|key| key.status == "ok").count() as i64;
        let rate_limited_keys = keys
            .iter()
            .filter(|key| key.status == "rate_limited")
            .count() as i64;
        let auth_failed_keys = keys
            .iter()
            .filter(|key| key.status == "auth_failed")
            .count() as i64;
        groups.push(ProviderRotationGroupProbe {
            logical_name,
            total_keys: source.total_keys,
            configured_keys: source.members.len() as i64,
            healthy_keys,
            rate_limited_keys,
            auth_failed_keys,
            current_index: source.current_index,
            strategy: source.strategy,
            next_retry_at: None,
            keys,
        });
    }
    groups.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
    groups
}

fn collect_rotation_sources(global_db_path: &Path) -> Vec<(String, RotationSourceStatus)> {
    let Some(path) = global_db_path.to_str() else {
        return Vec::new();
    };
    let Ok(store) = memory_core::MemoryStore::open_read_only(path) else {
        return Vec::new();
    };
    let Ok(entries) = store.vault_list_entries() else {
        return Vec::new();
    };
    let vault_names = entries
        .into_iter()
        .filter(|entry| entry.secret_type == "api_key")
        .map(|entry| entry.name)
        .collect::<HashSet<_>>();
    let Ok(rotations) = store.vault_list_rotations() else {
        return Vec::new();
    };
    let mut out = rotations
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
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

async fn probe_rotation_member(
    logical_name: &str,
    key_id: &str,
    value: String,
) -> (String, Option<String>) {
    let client = match crate::llm::LlmClient::new() {
        Ok(client) => client,
        Err(err) => return ("failed".to_string(), Some(err)),
    };
    client.clear_provider_secrets();
    client.set_provider_secret_pool(
        logical_name,
        vec![crate::llm::ProviderSecret {
            key_id: key_id.to_string(),
            value,
        }],
    );

    let result = match logical_name {
        "VOYAGE_API_KEY" => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                client.embed_voyage("tachi provider rotation probe", "document"),
            )
            .await;
            match result {
                Ok(Ok(vec)) => Ok(format!("{} dims", vec.len())),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 15s".to_string()),
            }
        }
        "VOYAGE_RERANK_API_KEY" => {
            let docs = vec![
                "Tachi stores operational memory".to_string(),
                "Unrelated weather note".to_string(),
            ];
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                client.rerank_voyage("tachi provider rotation probe", &docs, 1),
            )
            .await;
            match result {
                Ok(Ok(rows)) => Ok(format!("{} result(s)", rows.len())),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 15s".to_string()),
            }
        }
        "SILICONFLOW_API_KEY" | "EXTRACT_API_KEY" => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                client.call_extract_llm(
                    "Return exactly OK.",
                    "Provider rotation probe. Reply OK only.",
                    None,
                    0.0,
                    8,
                ),
            )
            .await;
            match result {
                Ok(Ok(text)) => Ok(text.chars().take(80).collect()),
                Ok(Err(err)) => Err(err),
                Err(_) => Err("timed out after 20s".to_string()),
            }
        }
        _ => {
            return (
                "unsupported".to_string(),
                Some("No live probe lane is registered for this logical key".to_string()),
            );
        }
    };

    match result {
        Ok(message) => ("ok".to_string(), Some(message)),
        Err(err) => classify_provider_probe_error(err),
    }
}

fn classify_provider_probe_error(err: String) -> (String, Option<String>) {
    let lower = err.to_ascii_lowercase();
    let status = if lower.contains("429") || lower.contains("rate limit") {
        "rate_limited"
    } else if is_auth_error(&err) {
        "auth_failed"
    } else if lower.contains("timed out") {
        "timeout"
    } else {
        "failed"
    };
    (status.to_string(), Some(err))
}

pub(crate) async fn refresh_provider_probe_cache(
    app_home: &Path,
    global_db_path: &Path,
) -> Result<ProviderProbeCache, String> {
    let report = run_provider_probe_report(global_db_path).await;
    write_provider_probe_cache_report(app_home, report)
}

pub(crate) fn read_provider_probe_cache(app_home: &Path) -> Option<ProviderProbeCache> {
    let raw = std::fs::read_to_string(provider_probe_cache_path(app_home)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn write_provider_probe_cache_report(
    app_home: &Path,
    report: ProviderProbeReport,
) -> Result<ProviderProbeCache, String> {
    let cache = ProviderProbeCache {
        last_probe_at: chrono::Utc::now().to_rfc3339(),
        ttl_seconds: PROVIDER_PROBE_CACHE_TTL_SECS,
        probes: report.probes,
        rotation_groups: report.rotation_groups,
    };
    let path = provider_probe_cache_path(app_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create probe cache dir: {e}"))?;
    }
    let serialized = serde_json::to_string_pretty(&cache)
        .map_err(|e| format!("serialize provider probe cache: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&path, serialized.as_bytes())?;
    Ok(cache)
}

fn provider_probe_cache_path(app_home: &Path) -> std::path::PathBuf {
    app_home.join("status").join("provider-probes.json")
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
    let key = crate::vault_crypto::DerivedVaultKey::derive(&password, &salt)?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in store.vault_list_entries()? {
        let is_provider_key = entry.name.ends_with("_API_KEY")
            || crate::provider_config::parse_rotation_member_name(&entry.name)
                .is_some_and(|(prefix, _)| prefix.ends_with("_API_KEY"));
        if entry.secret_type != "api_key" || !is_provider_key || entry.allowed_agents.is_some() {
            continue;
        }
        let decrypted =
            crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)?;
        if !value.trim().is_empty() {
            out.push((entry.name, value));
        }
    }
    Ok(out)
}

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

fn collect_api_key_status_from_sources(
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

pub(crate) fn calculate_health_score(
    daemon: &crate::status_ops::DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&crate::status_ops::DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
    probe_results: Option<&[ProviderProbeResult]>,
    rotation_group_results: Option<&[ProviderRotationGroupProbe]>,
) -> u8 {
    let mut score = 100i32;
    if !matches!(daemon, crate::status_ops::DaemonStatus::Running { .. }) {
        score -= 20;
    }
    // Only dead-lettered (retry-exhausted) jobs count as genuine, current
    // failures. Transient failures are auto-retried with backoff and self-heal,
    // so they no longer drag the score down for their full GC-retention window.
    let failed_jobs: usize = dbs.iter().map(|db| db.dead_lettered).sum();
    score -= (failed_jobs as i32).min(25);
    let stuck_jobs: usize = dbs.iter().map(|db| db.stuck_in_progress).sum();
    score -= ((stuck_jobs as i32) * 5).min(20);
    let low_vector_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::low_vector_coverage(db))
        .count();
    score -= ((low_vector_dbs as i32) * 10).min(25);
    let dim_mismatch_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::vector_dimension_mismatch(db))
        .count();
    score -= ((dim_mismatch_dbs as i32) * 15).min(30);
    let enrichment_failed_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::has_enrichment_failures(db))
        .count();
    let enrichment_failed_total: usize = dbs.iter().map(|db| db.enrichment_failed_recent).sum();
    score -= (((enrichment_failed_dbs as i32) * 5) + (enrichment_failed_total as i32 / 10)).min(20);
    let vector_orphans: usize = dbs.iter().map(|db| db.vector_orphans).sum();
    score -= ((vector_orphans as i32) * 3).min(10);
    if distill_marker.map(|m| m.is_stale).unwrap_or(true) {
        score -= 10;
    }
    // Hard errors inside the last distill batch are not foundry_jobs rows, so unlike
    // agent-evolution failures (which dead-letter) they would otherwise never reach
    // the health score. `fallback_used`/`groups_skipped` are graceful degradation,
    // not failure, so they stay informational (surfaced in status, not scored).
    if let Some(distill_errors) = distill_marker.and_then(|m| m.errors) {
        score -= ((distill_errors as i32) * 3).min(10);
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
    if let Some(groups) = rotation_group_results {
        let rate_limited_keys: i64 = groups.iter().map(|group| group.rate_limited_keys).sum();
        let auth_failed_keys: i64 = groups.iter().map(|group| group.auth_failed_keys).sum();
        score -= ((rate_limited_keys as i32) * 4).min(16);
        score -= ((auth_failed_keys as i32) * 8).min(24);
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
    } else if lower.contains("deepseek") {
        Some("DEEPSEEK".to_string())
    } else if lower.contains("minimax") {
        Some("MINIMAX".to_string())
    } else if lower.contains("zai")
        || lower.contains("zhipu")
        || lower.contains("bigmodel")
        || lower.contains("glm")
    {
        Some("ZAI".to_string())
    } else {
        Some("UNKNOWN".to_string())
    }
}

fn provider_to_key(provider: &str) -> Option<&'static str> {
    match provider {
        "VOYAGE" => Some("VOYAGE_API_KEY"),
        "SILICONFLOW" => Some("SILICONFLOW_API_KEY"),
        "DEEPSEEK" => Some("DEEPSEEK_API_KEY"),
        "MINIMAX" => Some("MINIMAX_API_KEY"),
        "ZAI" => Some("ZAI_API_KEY"),
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
        "last_distill": snapshot.distill_marker.as_ref().and_then(|m| {
            // Only emit the quality block for new-format (JSON) markers; legacy
            // bare-timestamp markers leave every field None.
            (m.groups_distilled.is_some()
                || m.groups_skipped.is_some()
                || m.fallback_used.is_some()
                || m.errors.is_some())
            .then(|| json!({
                "groups_distilled": m.groups_distilled,
                "groups_skipped": m.groups_skipped,
                "fallback_used": m.fallback_used,
                "errors": m.errors,
            }))
        }),
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
            HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
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
            HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
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
            HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
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

    #[test]
    fn deprecated_configured_key_reports_canonical_cleanup_hint() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("REASONING_API_KEY");
        std::env::set_var("REASONING_API_KEY", "legacy-secret");

        let rows = collect_api_key_status_from_sources(
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let reasoning = api_key_row(&rows, "REASONING_API_KEY");

        assert!(reasoning.deprecated);
        assert_eq!(reasoning.canonical_name, "SILICONFLOW_API_KEY");
        assert_eq!(reasoning.status, "configured");
        assert!(reasoning
            .cleanup_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("migrate this secret to SILICONFLOW_API_KEY")));

        restore_env("REASONING_API_KEY", original);
    }

    #[test]
    fn deprecated_unset_key_has_no_cleanup_hint() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("REASONING_API_KEY");
        std::env::remove_var("REASONING_API_KEY");

        let rows = collect_api_key_status_from_sources(
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let reasoning = api_key_row(&rows, "REASONING_API_KEY");

        assert!(reasoning.deprecated);
        assert_eq!(reasoning.status, "deprecated-unset");
        assert!(reasoning.cleanup_hint.is_none());

        restore_env("REASONING_API_KEY", original);
    }

    #[test]
    fn rotation_members_configure_their_logical_provider_key() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("VOYAGE_API_KEY");
        std::env::remove_var("VOYAGE_API_KEY");

        let rows = collect_api_key_status_from_sources(
            HashSet::from([
                "VOYAGE_API_KEY_1".to_string(),
                "VOYAGE_API_KEY_2".to_string(),
            ]),
            HashMap::new(),
            HashMap::new(),
            HashMap::from([(
                "VOYAGE_API_KEY".to_string(),
                RotationSourceStatus {
                    total_keys: 2,
                    current_index: 1,
                    strategy: "round_robin".to_string(),
                    members: vec![
                        "VOYAGE_API_KEY_1".to_string(),
                        "VOYAGE_API_KEY_2".to_string(),
                    ],
                },
            )]),
            &HashMap::new(),
            &HashMap::new(),
        );
        let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

        assert_eq!(voyage.status, "configured");
        assert_eq!(voyage.source, "vault");
        assert_eq!(
            voyage.rotation.as_ref().map(|rotation| rotation.total_keys),
            Some(2)
        );
        assert_eq!(
            voyage
                .rotation
                .as_ref()
                .map(|rotation| rotation.configured_keys),
            Some(2)
        );
        assert_eq!(
            voyage
                .rotation
                .as_ref()
                .and_then(|rotation| rotation.healthy_keys),
            None
        );
        assert_eq!(
            voyage
                .rotation
                .as_ref()
                .map(|rotation| {
                    rotation
                        .members
                        .iter()
                        .map(|member| member.name.as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            vec!["VOYAGE_API_KEY_1", "VOYAGE_API_KEY_2"]
        );

        let rotation_sources = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            RotationSourceStatus {
                total_keys: 2,
                current_index: 1,
                strategy: "round_robin".to_string(),
                members: vec![
                    "VOYAGE_API_KEY_1".to_string(),
                    "VOYAGE_API_KEY_2".to_string(),
                ],
            },
        )]);
        let probed = ProviderRotationGroupProbe {
            logical_name: "VOYAGE_API_KEY".to_string(),
            total_keys: 2,
            configured_keys: 2,
            healthy_keys: 1,
            rate_limited_keys: 1,
            auth_failed_keys: 0,
            current_index: 1,
            strategy: "round_robin".to_string(),
            next_retry_at: None,
            keys: vec![
                ApiKeyRotationMemberStatus {
                    name: "VOYAGE_API_KEY_1".to_string(),
                    status: "ok".to_string(),
                    message: Some("1024 dims".to_string()),
                    last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
                },
                ApiKeyRotationMemberStatus {
                    name: "VOYAGE_API_KEY_2".to_string(),
                    status: "rate_limited".to_string(),
                    message: Some("429".to_string()),
                    last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
                },
            ],
        };
        let rotation_probes = HashMap::from([("VOYAGE_API_KEY".to_string(), &probed)]);
        let probed_rows = collect_api_key_status_from_sources(
            HashSet::from([
                "VOYAGE_API_KEY_1".to_string(),
                "VOYAGE_API_KEY_2".to_string(),
            ]),
            HashMap::new(),
            HashMap::new(),
            rotation_sources,
            &HashMap::new(),
            &rotation_probes,
        );
        let probed_voyage = api_key_row(&probed_rows, "VOYAGE_API_KEY");
        let rotation = probed_voyage.rotation.as_ref().expect("rotation");
        assert_eq!(rotation.healthy_keys, Some(1));
        assert_eq!(rotation.rate_limited_keys, 1);
        assert_eq!(rotation.members[1].status, "rate_limited");

        restore_env("VOYAGE_API_KEY", original);
    }

    #[test]
    fn provider_probe_cache_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = write_provider_probe_cache_report(
            dir.path(),
            ProviderProbeReport {
                probes: vec![ProviderProbeResult {
                    name: "voyage_embed".to_string(),
                    status: "ok".to_string(),
                    message: Some("1024 dims".to_string()),
                }],
                rotation_groups: vec![ProviderRotationGroupProbe {
                    logical_name: "VOYAGE_API_KEY".to_string(),
                    total_keys: 2,
                    configured_keys: 2,
                    healthy_keys: 1,
                    rate_limited_keys: 1,
                    auth_failed_keys: 0,
                    current_index: 1,
                    strategy: "round_robin".to_string(),
                    next_retry_at: None,
                    keys: vec![
                        ApiKeyRotationMemberStatus {
                            name: "VOYAGE_API_KEY_1".to_string(),
                            status: "ok".to_string(),
                            message: Some("1024 dims".to_string()),
                            last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
                        },
                        ApiKeyRotationMemberStatus {
                            name: "VOYAGE_API_KEY_2".to_string(),
                            status: "rate_limited".to_string(),
                            message: Some("429".to_string()),
                            last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
                        },
                    ],
                }],
            },
        )
        .expect("write cache");

        let loaded = read_provider_probe_cache(dir.path()).expect("read cache");
        assert_eq!(loaded.last_probe_at, cache.last_probe_at);
        assert_eq!(loaded.ttl_seconds, 24 * 60 * 60);
        assert!(!loaded.is_stale());
        assert_eq!(loaded.probes.len(), 1);
        assert_eq!(loaded.probes[0].status, "ok");
        assert_eq!(loaded.rotation_groups.len(), 1);
        assert_eq!(loaded.rotation_groups[0].logical_name, "VOYAGE_API_KEY");
        assert_eq!(loaded.rotation_groups[0].healthy_keys, 1);
        assert_eq!(loaded.rotation_groups[0].rate_limited_keys, 1);
        assert_eq!(loaded.rotation_groups[0].keys[1].status, "rate_limited");
    }
}
