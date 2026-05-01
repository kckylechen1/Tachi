use super::*;

pub(crate) async fn handle_chain_skills(
    server: &MemoryServer,
    params: ChainSkillsParams,
) -> Result<String, String> {
    if params.steps.is_empty() {
        return Err("chain_skills requires at least one step".to_string());
    }

    let mut current_input = params.initial_input;
    let mut step_results: Vec<serde_json::Value> = Vec::new();

    for (i, step) in params.steps.iter().enumerate() {
        let start = Instant::now();

        let mut args = match &step.extra_args {
            Some(Value::Object(obj)) => Value::Object(obj.clone()),
            _ => json!({}),
        };
        if let Value::Object(ref mut map) = args {
            map.insert("input".into(), json!(current_input));
        }

        let run_params = RunSkillParams {
            skill_id: step.skill_id.clone(),
            args,
        };

        match server.run_skill(Parameters(run_params)).await {
            Ok(output) => {
                let elapsed_ms = start.elapsed().as_millis();
                step_results.push(json!({
                    "step": i,
                    "skill_id": step.skill_id,
                    "elapsed_ms": elapsed_ms,
                    "status": "ok",
                }));
                current_input = output;
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

    serde_json::to_string(&json!({
        "status": "ok",
        "total_steps": params.steps.len(),
        "output": current_input,
        "steps": step_results,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
