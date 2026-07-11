use std::path::Path;

use serde_json::json;
use tachi_llm::RerankConfig;

use super::api_keys::collect_api_key_status;
use crate::status_ops::EXPECTED_EMBEDDING_DIM;

pub(crate) fn provider_key_status_json(global_db_path: &Path) -> serde_json::Value {
    json!(collect_api_key_status(global_db_path))
}

pub(crate) fn model_lanes_json() -> serde_json::Value {
    // Report the actually configured rerank provider (not a hardcoded "voyage").
    // Invalid config surfaces as a status error field rather than lying.
    let rerank_cfg = RerankConfig::from_env();
    let (rerank_provider, rerank_model, rerank_keys, local_endpoint, rerank_config_error) =
        match &rerank_cfg {
            Ok(cfg) => {
                let keys: serde_json::Value = match cfg.provider {
                    tachi_llm::RerankProviderKind::Voyage => {
                        json!(["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"])
                    }
                    tachi_llm::RerankProviderKind::Local => json!([]),
                };
                (
                    cfg.provider_name(),
                    cfg.model_name().map(str::to_string),
                    keys,
                    cfg.local_endpoint.clone(),
                    None::<String>,
                )
            }
            Err(err) => ("invalid", None, json!([]), None, Some(err.clone())),
        };

    let mut rerank_lane = json!({
        "provider": rerank_provider,
        "model": rerank_model,
        "keys": rerank_keys,
    });
    if let Some(endpoint) = local_endpoint {
        rerank_lane
            .as_object_mut()
            .expect("rerank_lane object")
            .insert("local_endpoint".into(), json!(endpoint));
    }
    if let Some(err) = rerank_config_error {
        rerank_lane
            .as_object_mut()
            .expect("rerank_lane object")
            .insert("config_error".into(), json!(err));
    }

    let auth_failure_hint = match rerank_cfg.as_ref().map(|c| c.provider) {
        Ok(tachi_llm::RerankProviderKind::Local) => {
            "403 during query generation points to SILICONFLOW_API_KEY; local rerank uses TACHI_RERANK_LOCAL_ENDPOINT (no Voyage key)"
        }
        _ => {
            "403 during query generation points to SILICONFLOW_API_KEY; 403 during Voyage rerank points to VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY"
        }
    };

    json!({
        "embedding": {
            "provider": "voyage",
            "model": "voyage-4",
            "expected_dimension": EXPECTED_EMBEDDING_DIM,
            "key": "VOYAGE_API_KEY",
        },
        "rerank": rerank_lane,
        "recall_rerank_cache": {
            "query_generation_provider": "extract/SiliconFlow",
            "rerank_provider": rerank_provider,
            "auth_failure_hint": auth_failure_hint,
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
            "default_base_url_when_deepseek_key_selected": "https://api.deepseek.com/chat/completions",
            "default_model_when_deepseek_key_selected": "deepseek-chat",
            "keys": ["DISTILL_API_KEY", "DEEPSEEK_API_KEY", "REASONING_API_KEY", "ZAI_API_KEY", "BIGMODEL_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "reasoning": {
            "provider": "claude-cli-first, openai-compatible fallback",
            "default_base_url_when_deepseek_key_selected": "https://api.deepseek.com/chat/completions",
            "default_model_when_deepseek_key_selected": "deepseek-reasoner",
            "keys": ["DEEPSEEK_API_KEY", "REASONING_API_KEY", "ZAI_API_KEY", "BIGMODEL_API_KEY", "DISTILL_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        }
    })
}
