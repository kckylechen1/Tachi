use super::*;

// ─── Kanban task initialization + flow dispatch marker ───────────────────────

pub(super) struct FlowSetupInputs<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) dispatch_id: &'a str,
    pub(super) request: &'a tachi_params::StaffAssignmentRequest,
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) grant: &'a tachi_params::ExecutionGrant,
    pub(super) resolved_profile: &'a ResolvedDispatchProfile,
    pub(super) plan_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) prompt_md_path: &'a Path,
    pub(super) context_md_path: &'a Path,
    pub(super) trajectory_path: &'a Path,
    pub(super) capability_bundle_card: &'a Value,
    pub(super) capability_bundle_file: &'a str,
}

pub(super) async fn init_kanban_and_flow(inputs: FlowSetupInputs<'_>) -> Result<(), String> {
    init_kanban_task(
        inputs.server,
        inputs.dispatch_id,
        inputs.request,
        inputs.assignment,
        inputs.grant,
        inputs.resolved_profile,
        Some(&inputs.plan_path.to_string_lossy()),
    )
    .await?;
    if let Some(flow_id) = inputs
        .request
        .flow_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        if let Err(error) = crate::task_lifecycle::mark_task_dispatch(
            flow_id,
            inputs.dispatch_id,
            json!({
                "agent": inputs.assignment.selected_backend,
                "profile": inputs.assignment.selected_profile.clone(),
                "tool_profile": inputs.resolved_profile.tool_profile.clone(),
                "stage": inputs.request.stage.clone(),
                "task": inputs.request.task.clone(),
                "issue_ref": inputs.request.issue_ref.clone(),
                "pr_ref": inputs.request.pr_ref.clone(),
                "run_dir": inputs.workspace_dir.to_string_lossy(),
                "prompt_file": inputs.prompt_md_path.to_string_lossy(),
                "context_file": inputs.context_md_path.to_string_lossy(),
                "trajectory_file": inputs.trajectory_path.to_string_lossy(),
                "plan_file": inputs.plan_path.to_string_lossy(),
                "capability_bundle": inputs.capability_bundle_card.clone(),
                "capability_bundle_file": inputs.capability_bundle_file,
                "evidence_required": inputs.assignment.evidence_required,
                "route_explanation": inputs.assignment.route_explanation,
                "identity_receipt": inputs.assignment.identity_receipt,
                "suggested_complete": suggested_complete_payload(inputs.dispatch_id, inputs.assignment, inputs.request),
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
