use crate::hub_ops::{
    execute_registered_skill_prompt, SkillExecutionMode, SIMULATED_SKILL_OUTPUT_MARKER,
    SIMULATED_SKILL_OUTPUT_WARNING,
};
use crate::tool_params::ChainSkillsParams;
use crate::MemoryServer;
use serde_json::{json, Value};
use std::time::Instant;

pub(crate) async fn handle_chain_skills(
    server: &MemoryServer,
    params: ChainSkillsParams,
) -> Result<String, String> {
    if params.steps.is_empty() {
        return Err("chain_skills requires at least one step".to_string());
    }

    let mut current_input = params.initial_input;
    let mut step_results: Vec<serde_json::Value> = Vec::new();
    let mut any_simulated = false;

    for (i, step) in params.steps.iter().enumerate() {
        let start = Instant::now();

        let mut args = match &step.extra_args {
            Some(Value::Object(obj)) => Value::Object(obj.clone()),
            _ => json!({}),
        };
        if let Value::Object(ref mut map) = args {
            map.insert("input".into(), json!(current_input));
        }

        match execute_registered_skill_prompt(server, &step.skill_id, &args).await {
            Ok(execution) => {
                let elapsed_ms = start.elapsed().as_millis();
                let execution_mode: SkillExecutionMode = execution.execution;
                any_simulated |= execution_mode.is_simulated();
                step_results.push(json!({
                    "step": i,
                    "skill_id": step.skill_id,
                    "elapsed_ms": elapsed_ms,
                    "status": "ok",
                    "execution": execution_mode.as_str(),
                }));
                current_input = execution.output;
            }
            Err(e) => {
                step_results.push(json!({
                    "step": i,
                    "skill_id": step.skill_id,
                    "status": "error",
                    "error": e,
                }));
                return Err(format!(
                    "chain_skills failed at step {} (skill '{}'): {}",
                    i, step.skill_id, e
                ));
            }
        }
    }

    let output = if any_simulated {
        format!("{SIMULATED_SKILL_OUTPUT_MARKER}\n\n{current_input}")
    } else {
        current_input
    };

    let mut response = json!({
        "status": "ok",
        "total_steps": params.steps.len(),
        "simulated": any_simulated,
        "output": output,
        "steps": step_results,
    });
    if any_simulated {
        response["warning"] = json!(SIMULATED_SKILL_OUTPUT_WARNING);
    }

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}
