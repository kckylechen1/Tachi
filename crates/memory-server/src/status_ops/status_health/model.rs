use std::path::Path;

use serde_json::json;

use super::api_keys::collect_api_key_status;
use crate::status_ops::EXPECTED_EMBEDDING_DIM;

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
