use super::*;

pub(super) async fn handle_stage_action(
    _server: &MemoryServer,
    stage: &str,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| format!("'task' is required for action='{}'", stage))?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill(stage, &run_dir).await;

    let instruction = build_instruction_md(
        &flow_id,
        stage,
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    tokio::fs::write(&instr_path, instruction)
        .await
        .map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, stage, &task, &injection, created)?;
    let required_skills = crate::skill_policy::shell_stage_skills(stage);

    let resp = json!({
        "flow_id": flow_id,
        "stage": stage,
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary(stage, &required_skills),
        "async": false,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}
