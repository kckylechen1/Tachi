use std::collections::HashMap;
use std::path::Path;

use super::inference::is_auth_error;
use super::rotation::collect_rotation_sources;
use super::types::{ProviderProbeReport, ProviderProbeResult, ProviderRotationGroupProbe};
use super::vault::load_keychain_vault_api_key_values;
use crate::status_ops::ApiKeyRotationMemberStatus;

pub(crate) async fn run_provider_probe_report(global_db_path: &Path) -> ProviderProbeReport {
    let llm = match probe_llm_client(global_db_path) {
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

    // Run probes concurrently so adding chat lanes does not make status
    // probes serially accumulate their timeout budgets.
    let rerank_docs = vec![
        "Tachi stores operational memory".to_string(),
        "Unrelated weather note".to_string(),
    ];
    let llm_embed = llm.clone();
    let llm_rerank = llm.clone();
    let llm_extract = llm.clone();
    let (embed, rerank, chat_extract, chat_distill) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            llm_embed.embed_voyage("tachi provider probe", "document"),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            llm_rerank.rerank("tachi provider probe", &rerank_docs, 1),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            llm_extract.call_extract_llm(
                "Return exactly OK.",
                "Provider extract probe. Reply OK only.",
                None,
                0.0,
                8,
            ),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            llm.call_distill_llm(
                "Return exactly OK.",
                "Provider distill probe. Reply OK only.",
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
    let rerank_probe_name = match tachi_llm::RerankConfig::from_env() {
        Ok(cfg) => format!("{}_rerank", cfg.provider_name()),
        Err(_) => "rerank".to_string(),
    };
    out.push(match rerank {
        Ok(Ok(rows)) => ProviderProbeResult {
            name: rerank_probe_name.clone(),
            status: "ok".to_string(),
            message: Some(format!("{} result(s)", rows.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: rerank_probe_name.clone(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: rerank_probe_name,
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });
    out.push(match chat_extract {
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
    out.push(match chat_distill {
        Ok(Ok(text)) => ProviderProbeResult {
            name: "chat_distill".to_string(),
            status: "ok".to_string(),
            message: Some(text.chars().take(80).collect()),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "chat_distill".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "chat_distill".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 20s".to_string()),
        },
    });
    ProviderProbeReport {
        probes: out,
        rotation_groups: run_rotation_group_probes(global_db_path).await,
    }
}

fn probe_llm_client(global_db_path: &Path) -> Result<tachi_llm::LlmClient, String> {
    tachi_llm::LlmClient::new_with_vault_db(Some(global_db_path))
}

#[cfg(test)]
pub(crate) fn probe_llm_client_for_tests(
    global_db_path: &Path,
) -> Result<tachi_llm::LlmClient, String> {
    probe_llm_client(global_db_path)
}

pub(crate) async fn run_provider_probes(global_db_path: &Path) -> Vec<ProviderProbeResult> {
    run_provider_probe_report(global_db_path).await.probes
}

pub(super) async fn run_rotation_group_probes(
    global_db_path: &Path,
) -> Vec<ProviderRotationGroupProbe> {
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

async fn probe_rotation_member(
    logical_name: &str,
    key_id: &str,
    value: String,
) -> (String, Option<String>) {
    let client = match tachi_llm::LlmClient::new() {
        Ok(client) => client,
        Err(err) => return ("failed".to_string(), Some(err)),
    };
    client.clear_provider_secrets();
    client.set_provider_secret_pool(
        logical_name,
        vec![tachi_llm::ProviderSecret {
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
        "DISTILL_API_KEY" => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                client.call_distill_llm(
                    "Return exactly OK.",
                    "Provider distill rotation probe. Reply OK only.",
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
