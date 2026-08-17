mod hub;

use crate::tool_params::RunSkillParams;
use crate::utils::render_skill_prompt_template;
use crate::MemoryServer;
pub(crate) use hub::{handle_hub_call, handle_hub_disconnect, handle_tachi_audit_log};
use memcore::HubCapability;
use serde_json::Value;
use tachi_hub::{
    build_skill_execution_envelope, capability_callable, SkillExecution, SkillExecutionMode,
};

pub(crate) async fn handle_run_skill(
    server: &MemoryServer,
    params: RunSkillParams,
) -> Result<String, String> {
    let execution = execute_registered_skill_prompt(server, &params.skill_id, &params.args).await?;
    serde_json::to_string(&build_skill_execution_envelope(
        &params.skill_id,
        execution,
        None,
    ))
    .map_err(|e| format!("serialize skill execution envelope: {e}"))
}

pub(crate) async fn execute_registered_skill_prompt(
    server: &MemoryServer,
    skill_id: &str,
    args: &Value,
) -> Result<SkillExecution, String> {
    execute_registered_skill_prompt_with_receipt(server, skill_id, args)
        .await
        .map(|result| result.execution)
}

pub(crate) struct SkillExecutionWithReceipt {
    pub(crate) execution: SkillExecution,
}

pub(crate) async fn execute_registered_skill_prompt_with_receipt(
    server: &MemoryServer,
    skill_id: &str,
    args: &Value,
) -> Result<SkillExecutionWithReceipt, String> {
    let cap = {
        let mut found = None;
        if server.has_project_db() {
            found = server.with_project_store(|store| {
                store
                    .hub_get(skill_id)
                    .map_err(|e| format!("hub get project: {e}"))
            })?;
        }
        if found.is_none() {
            found = server.with_global_store(|store| {
                store
                    .hub_get(skill_id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?;
        }
        found.ok_or_else(|| format!("Skill '{skill_id}' not found in Hub"))?
    };

    execute_loaded_skill_prompt_with_receipt(server, &cap, args).await
}

async fn execute_loaded_skill_prompt_with_receipt(
    server: &MemoryServer,
    cap: &HubCapability,
    args: &Value,
) -> Result<SkillExecutionWithReceipt, String> {
    if cap.cap_type != "skill" {
        return Err(format!(
            "'{}' is type '{}', not 'skill'",
            cap.id, cap.cap_type
        ));
    }
    if !capability_callable(cap) {
        return Err(format!(
            "Skill '{}' is not callable (enabled={}, review_status={}, health_status={}).",
            cap.id, cap.enabled, cap.review_status, cap.health_status
        ));
    }

    let def: serde_json::Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("invalid skill definition JSON: {e}"))?;

    let empty_args = serde_json::Map::new();
    let args_obj = args.as_object().unwrap_or(&empty_args);
    let result = execute_skill_prompt_with_receipt(server, cap, &def, args_obj).await;

    let success = result.is_ok();
    let error_msg = result.as_ref().err().map(|e| e.to_string());
    let _ = server.record_capability_call_outcome(&cap.id, success, error_msg.as_deref());

    result.map_err(|e| format!("skill execution failed: {e}"))
}

async fn execute_skill_prompt_with_receipt(
    server: &MemoryServer,
    _cap: &HubCapability,
    def: &Value,
    args: &serde_json::Map<String, Value>,
) -> Result<SkillExecutionWithReceipt, String> {
    if def.get("execution").and_then(Value::as_str) == Some("document") {
        let output = def
            .get("content")
            .and_then(Value::as_str)
            .or_else(|| def.get("prompt").and_then(Value::as_str))
            .or_else(|| def.get("template").and_then(Value::as_str))
            .ok_or_else(|| {
                "document skill definition missing 'content', 'prompt', or 'template' field"
                    .to_string()
            })?;
        return Ok(SkillExecutionWithReceipt {
            execution: SkillExecution {
                output: output.to_string(),
                execution: SkillExecutionMode::Document,
            },
        });
    }

    let skill_content = def
        .get("content")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let prompt_template = def["prompt"]
        .as_str()
        .or_else(|| def["template"].as_str())
        .or(skill_content)
        .ok_or_else(|| {
            "skill definition missing 'prompt', 'template', or 'content' field".to_string()
        })?;

    let resolved_prompt = render_skill_prompt_template(prompt_template, args)
        .map_err(|e| format!("serialize skill args: {e}"))?;

    if let Some(mock_response) = def.get("mock_response").and_then(|v| v.as_str()) {
        Ok(SkillExecutionWithReceipt {
            execution: SkillExecution {
                output: mock_response.to_string(),
                execution: SkillExecutionMode::MockResponse,
            },
        })
    } else {
        let default_system = "You are an AI assistant executing a specialized skill.";
        let system = def
            .get("system")
            .and_then(|v| v.as_str())
            .or_else(|| {
                if def["prompt"].as_str().is_none() && def["template"].as_str().is_none() {
                    skill_content
                } else {
                    None
                }
            })
            .unwrap_or(default_system);
        let model = def.get("model").and_then(|v| v.as_str());
        let temperature = def
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.3) as f32;
        let max_tokens = def
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(4000) as u32;

        server
            .llm
            .call_extract_llm_with_receipt(system, &resolved_prompt, model, temperature, max_tokens)
            .await
            .and_then(|output| {
                if output.invocation.completion_status() == tachi_llm::CompletionStatusV1::Truncated
                {
                    return Err(tachi_llm::LLM_OUTPUT_TRUNCATED.to_string());
                }
                Ok(SkillExecutionWithReceipt {
                    execution: SkillExecution {
                        output: output.value,
                        execution: SkillExecutionMode::LlmGenerated,
                    },
                })
            })
    }
}
