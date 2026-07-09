use std::collections::HashSet;

use crate::status_ops::{ApiKeyStatus, DbStatus};

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

pub(super) fn is_auth_error(reason: &str) -> bool {
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
