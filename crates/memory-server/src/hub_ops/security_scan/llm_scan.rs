use super::backend::{resolve_security_scan_backend, SecurityScanBackend};
use crate::utils::parse_env_bool;
use crate::MemoryServer;

pub(in crate::hub_ops) async fn scan_skill_definition_with_llm(
    server: &MemoryServer,
    def: &serde_json::Value,
) -> Option<serde_json::Value> {
    if cfg!(test) {
        return None;
    }
    let enabled = parse_env_bool("SKILL_SECURITY_SCAN_USE_LLM").unwrap_or(true);
    if !enabled {
        return None;
    }

    // Phase 2: SKILL_SECURITY_SCAN_BACKEND chooses how the LLM portion is
    // executed. `claude_cli` is the default — call the pool first and fall
    // back to the raw_api lane on Err. `raw_api` bypasses the pool entirely.
    // `disabled` skips the LLM portion (callers still receive the static
    // heuristic scan via merge_skill_scans).
    let backend = resolve_security_scan_backend();
    if backend == SecurityScanBackend::Disabled {
        return None;
    }

    let model = std::env::var("SKILL_SECURITY_SCAN_MODEL")
        .unwrap_or_else(|_| "Qwen/Qwen3.5-27B".to_string());
    let payload = serde_json::to_string(def).ok()?;

    let llm_call_result: Result<(String, &'static str), String> = match backend {
        SecurityScanBackend::ClaudeCli => {
            let llm_for_fallback = server.llm.clone();
            let payload_for_fallback = payload.clone();
            let model_for_fallback = model.clone();
            tachi_llm::claude_pool::pool_call_with_fallback(
                &server.claude_pool,
                crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                &payload,
                "security-scan",
                move || async move {
                    llm_for_fallback
                        .call_extract_llm(
                            crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                            &payload_for_fallback,
                            Some(&model_for_fallback),
                            0.1,
                            800,
                        )
                        .await
                },
            )
            .await
            .map(|(text, src)| (text, src.as_str()))
        }
        SecurityScanBackend::RawApi => server
            .llm
            .call_extract_llm(
                crate::prompts::SKILL_SECURITY_SCAN_PROMPT,
                &payload,
                Some(&model),
                0.1,
                800,
            )
            .await
            .map(|text| (text, "raw_api")),
        SecurityScanBackend::Disabled => unreachable!("handled above"),
    };

    match llm_call_result {
        Ok((raw, source)) => {
            let parsed: serde_json::Value = serde_json::from_str(
                tachi_llm::LlmClient::strip_code_fence(&raw),
            )
            .unwrap_or_else(|_| {
                serde_json::json!({
                    "risk": "medium",
                    "blocked": false,
                    "findings": ["Failed to parse LLM security scan JSON output"],
                    "reason": raw
                })
            });
            Some(serde_json::json!({
                "status": "ok",
                "model": model,
                "backend": source,
                "result": parsed,
            }))
        }
        Err(e) => Some(serde_json::json!({
            "status": "error",
            "model": model,
            "backend": backend.as_str(),
            "error": e,
        })),
    }
}
