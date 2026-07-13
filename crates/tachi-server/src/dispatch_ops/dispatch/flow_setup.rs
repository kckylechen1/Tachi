use super::*;

// ─── Kanban task initialization + flow dispatch marker ───────────────────────

pub(super) struct FlowSetupInputs<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) dispatch_id: &'a str,
    pub(super) params: &'a TachiDispatchParams,
    pub(super) agent_norm: &'a str,
    pub(super) plan_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) prompt_md_path: &'a Path,
    pub(super) context_md_path: &'a Path,
    pub(super) trajectory_path: &'a Path,
    pub(super) capability_bundle_card: &'a Value,
    pub(super) capability_bundle_file: &'a str,
    pub(super) evidence_required: &'a [String],
    pub(super) route_explanation: &'a [String],
    pub(super) identity_receipt: &'a tachi_dispatch::DispatchIdentityReceipt,
}

pub(super) async fn init_kanban_and_flow(inputs: FlowSetupInputs<'_>) -> Result<(), String> {
    init_kanban_task(
        inputs.server,
        inputs.dispatch_id,
        inputs.params,
        Some(&inputs.plan_path.to_string_lossy()),
    )
    .await?;
    if let Some(flow_id) = inputs
        .params
        .flow_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        if let Err(error) = crate::task_lifecycle::mark_task_dispatch(
            flow_id,
            inputs.dispatch_id,
            json!({
                "agent": inputs.agent_norm,
                "profile": inputs.params.profile.clone(),
                "tool_profile": inputs.params.tool_profile.clone(),
                "stage": inputs.params.stage.clone(),
                "task": inputs.params.task.clone(),
                "issue_ref": inputs.params.issue_ref.clone(),
                "pr_ref": inputs.params.pr_ref.clone(),
                "run_dir": inputs.workspace_dir.to_string_lossy(),
                "prompt_file": inputs.prompt_md_path.to_string_lossy(),
                "context_file": inputs.context_md_path.to_string_lossy(),
                "trajectory_file": inputs.trajectory_path.to_string_lossy(),
                "plan_file": inputs.plan_path.to_string_lossy(),
                "capability_bundle": inputs.capability_bundle_card.clone(),
                "capability_bundle_file": inputs.capability_bundle_file,
                "evidence_required": inputs.evidence_required,
                "route_explanation": inputs.route_explanation,
                "identity_receipt": inputs.identity_receipt,
                "suggested_complete": suggested_complete_payload(inputs.dispatch_id, inputs.agent_norm, inputs.params),
            }),
        ) {
            append_trajectory_event(
                inputs.trajectory_path,
                json!({
                    "event": "flow_dispatch_marker_failed",
                    "dispatch_id": inputs.dispatch_id,
                    "flow_id": flow_id,
                    "error": error,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
        }
    }
    Ok(())
}
