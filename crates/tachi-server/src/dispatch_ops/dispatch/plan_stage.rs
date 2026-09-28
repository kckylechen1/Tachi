use super::super::kanban_helpers::update_kanban_state;
use super::*;
use crate::dispatch_ops::dispatch_v2::{
    CommittedModelPlan, PlanCommit, PlanCommitPre, PlanFailurePublication,
};
use crate::dispatch_ops::kanban_helpers::plan_reference;

// ─── V2 plan stage (optional Stage 1) ────────────────────────────────────────

pub(super) struct PlanStageInputs<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) request: &'a tachi_params::StaffAssignmentRequest,
    pub(super) dispatch_id: &'a str,
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) resolved_profile: &'a ResolvedDispatchProfile,
    pub(super) profile_payload: &'a Value,
    pub(super) base_prompt: &'a str,
    pub(super) prompt_md_path: &'a Path,
    pub(super) context_md_path: &'a Path,
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) feedback_rules_trace: &'a Value,
    pub(super) v2_decision: V2Decision,
}

#[derive(Debug)]
pub(super) struct PlanStageOutcome {
    pub(super) prompt: String,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) plan_generated_at: Option<String>,
    /// If Some, the review gate fired and the coordinator must return this
    /// serialized JSON response immediately instead of proceeding to spawn.
    pub(super) early_response: Option<String>,
}

pub(super) async fn run_v2_plan_stage(
    inputs: PlanStageInputs<'_>,
) -> Result<PlanStageOutcome, String> {
    let v2 = matches!(inputs.v2_decision, V2Decision::Enabled);

    // The actual prompt fed to the executing agent. In V1 this is just the
    // assembled prompt. In V2 it is rewritten after Stage 1 succeeds.
    let mut prompt = inputs.base_prompt.to_string();
    let mut plan_duration_ms: Option<u64> = None;
    let mut plan_generated_at: Option<String> = None;

    if v2 {
        // The plan content and its exact serving receipt are published in one
        // atomic `status.json` replacement. The anchor is opened before the
        // provider is awaited, and no lock is held across that await.
        let label = format!("dispatch-plan-{}", inputs.dispatch_id);
        // An unverifiable anchor means we cannot durably publish or refuse a
        // planner failure: report reconciliation unknown and let the caller's
        // planner-settled path avoid fabricating a terminal kanban state.
        let plan_commit =
            PlanCommit::open(inputs.workspace_dir, inputs.dispatch_id).map_err(|error| {
                format!("{error} (plan status anchor unverifiable; reconciliation unknown)")
            })?;
        // A read refusal (terminal/cancelled run, an already-committed
        // completion, or an untrusted prior plan) must not be clobbered by a
        // synthetic failure; propagate and let the planner-settled path leave
        // the authoritative state alone.
        let pre = plan_commit.read()?;
        let (committed, provider_duration_ms): (CommittedModelPlan, Option<u64>) = match pre {
            // A valid committed plan is reused without a second model call.
            PlanCommitPre::Reuse(existing) => (existing, None),
            PlanCommitPre::NeedsCommit { revision } => {
                let plan_timeout = Duration::from_secs(plan_timeout_secs());
                let plan_fut = run_plan_stage(inputs.server, &inputs.request.task, &label);
                let plan_outcome = match tokio::time::timeout(plan_timeout, plan_fut).await {
                    Ok(Ok(p)) => p,
                    Ok(Err(e)) => {
                        return Err(
                            publish_plan_stage_failure(&plan_commit, &inputs, revision, e).await,
                        );
                    }
                    Err(_) => {
                        let e = format!(
                            "dispatch v2 stage1 (plan) timed out after {}s",
                            plan_timeout.as_secs()
                        );
                        return Err(
                            publish_plan_stage_failure(&plan_commit, &inputs, revision, e).await,
                        );
                    }
                };
                let plan_md = plan_outcome.plan_md;
                match plan_commit.commit(revision, &plan_md, &plan_outcome.invocation) {
                    Ok(committed) => (committed, Some(plan_outcome.duration_ms)),
                    // A commit failure is a publication fault (IO) or a race
                    // that a concurrent first winner may have settled. Re-read
                    // once for a winner; otherwise publish the failure
                    // conditionally under the same anchor/fence/CAS so a newer
                    // terminal/cancelled/committed state is never overwritten
                    // and status/kanban do not split.
                    Err(error) => match plan_commit.read() {
                        Ok(PlanCommitPre::Reuse(existing)) => {
                            (existing, Some(plan_outcome.duration_ms))
                        }
                        _ => {
                            return Err(publish_plan_stage_failure(
                                &plan_commit,
                                &inputs,
                                revision,
                                error,
                            )
                            .await);
                        }
                    },
                }
            }
        };

        // Never overwrite the V1-placeholder `plan.md`; the canonical model
        // plan lives only in the committed `status.json#/model_plan` receipt.
        let plan_md = committed.content.clone();
        let sections = parse_plan_sections(&plan_md);
        plan_duration_ms = provider_duration_ms;
        let pgen_at = committed
            .plan_generated_at
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        plan_generated_at = Some(pgen_at.clone());
        append_trajectory_event(
            inputs.trajectory_path,
            json!({
                "event": "plan_generated",
                "dispatch_id": inputs.dispatch_id,
                "timestamp": pgen_at,
                "duration_ms": provider_duration_ms,
                "bytes": plan_md.len(),
                "sections_complete": sections.is_complete(),
                "artifact_revision": committed.artifact_revision,
                "payload_digest": committed.payload_digest,
            }),
        );

        // ─── Optional review gate ────────────────────────────────────────
        if plan_review_required() {
            write_status_json(
                inputs.workspace_dir,
                inputs.dispatch_id,
                true,
                plan_generated_at.as_deref(),
                None,
                "pending_review",
                None,
                plan_duration_ms,
                None,
                plan_duration_ms,
                None,
            );
            append_trajectory_event(
                inputs.trajectory_path,
                json!({
                    "event": "plan_pending_review",
                    "dispatch_id": inputs.dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
                    "artifact_revision": committed.artifact_revision,
                    "payload_digest": committed.payload_digest,
                }),
            );
            // #971 review-fix (F2, second pass): the kanban row was seeded
            // TASK_STATE_WORKING by BOARD-FIRST init (`init_kanban_task`).
            // This branch returns an early response with no approve/resume
            // action — the documented recovery is re-dispatch with plan
            // review disabled, which allocates a *new* dispatch_id and never
            // touches this row. So the kanban projection here must be
            // `TASK_STATE_INPUT_REQUIRED`, not `TASK_STATE_PENDING_REVIEW`:
            // (1) it matches what `status.json`/`status_state()` already
            // projects for this exact state (`board/status.rs`: a
            // `plan_review_status == "pending_review"` status.json maps to
            // TASK_STATE_INPUT_REQUIRED) — one vocabulary, no drift between
            // the two projections of the same run; and (2) unlike
            // TASK_STATE_PENDING_REVIEW, TASK_STATE_INPUT_REQUIRED IS in
            // `KANBAN_DISPATCH_NON_TERMINAL_STATES`
            // (`memcore/src/db/operator_maintenance.rs`), so an abandoned row (caller never re-dispatches)
            // ages out through `gc_expired_kanban_cards` instead of staying
            // pinned as a phantom "in review" card forever.
            // Best-effort: log but do not fail the dispatch response over a
            // kanban write hiccup — the plan itself already succeeded and
            // the caller needs the response to act on it.
            if let Err(kanban_err) = update_kanban_state(
                inputs.server,
                inputs.dispatch_id,
                "TASK_STATE_INPUT_REQUIRED",
                None,
                None,
            )
            .await
            {
                eprintln!(
                    "[dispatch-v2] failed to mark dispatch {} INPUT_REQUIRED in kanban: {}",
                    inputs.dispatch_id, kanban_err
                );
            }
            let plan_reference = plan_reference(None, inputs.workspace_dir, true);
            let mut response = json!({
                "dispatch_id": inputs.dispatch_id,
                "task": {
                    "id": inputs.dispatch_id,
                    // #971 review-fix (F2, second pass): mirror the kanban
                    // row and status.json projection above — both now say
                    // TASK_STATE_INPUT_REQUIRED for this state. Keeping this
                    // in sync avoids handing the caller a task state string
                    // that a subsequent board/status poll will never repeat.
                    "status": { "state": "TASK_STATE_INPUT_REQUIRED" },
                },
                "agent": inputs.assignment.selected_backend,
                "profile": inputs.profile_payload,
                "selected_profile": inputs.assignment.selected_profile,
                "tool_access": inputs.resolved_profile.mcp_access,
                "route_explanation": inputs.assignment.route_explanation,
                "fallback_chain": inputs.assignment.fallback_chain,
                "issue_ref": inputs.request.issue_ref,
                "pr_ref": inputs.request.pr_ref,
                "flow_id": inputs.request.flow_id,
                "feedback_rules": inputs.feedback_rules_trace.clone(),
                "v2": true,
                "plan_review_status": "pending_review",
                "message": "Plan generated. DISPATCH_V2_PLAN_REVIEW=true — execute stage paused. Audit status.json#/model_plan and re-dispatch with the env var unset to proceed.",
                "suggested_complete_command": suggested_complete_payload(inputs.dispatch_id, inputs.assignment, inputs.request),
                "plan_file": plan_reference.file,
                "prompt_file": inputs.prompt_md_path.to_string_lossy(),
                "context_file": inputs.context_md_path.to_string_lossy(),
                "trajectory_file": inputs.trajectory_path.to_string_lossy(),
                "run_dir": inputs.workspace_dir.to_string_lossy(),
            });
            if let Some(pointer) = plan_reference.pointer {
                response["plan_pointer"] = json!(pointer);
            }
            return Ok(PlanStageOutcome {
                prompt,
                plan_duration_ms,
                plan_generated_at,
                early_response: Some(
                    serde_json::to_string(&response).unwrap_or_else(|e| format!("serialize: {e}")),
                ),
            });
        }

        // Auto-approve: rewrite the prompt fed to the executing agent so
        // it contains the committed plan + implement-plan skill.
        append_trajectory_event(
            inputs.trajectory_path,
            json!({
                "event": "plan_approved",
                "dispatch_id": inputs.dispatch_id,
                "timestamp": Utc::now().to_rfc3339(),
                "auto": true,
                "artifact_revision": committed.artifact_revision,
                "payload_digest": committed.payload_digest,
            }),
        );
        prompt = build_execute_prompt(&plan_md, &inputs.request.task, inputs.base_prompt);
    }

    Ok(PlanStageOutcome {
        prompt,
        plan_duration_ms,
        plan_generated_at,
        early_response: None,
    })
}

/// Conditionally publish a plan-stage failure and return the typed outcome.
///
/// The failure is written under the existing `PlanCommit` anchor/fence/CAS at
/// the revision captured before the provider call. It is refused (writing
/// nothing) when a valid committed plan, a terminal/cancelled state, or a
/// newer status revision already won, so a concurrent completion or
/// cancellation is never overwritten. The kanban row is projected to FAILED
/// **only** when this publication wins; a refused or unpersistable failure
/// leaves the row and status untouched so they cannot split. The `plan_failed`
/// trajectory event is emitted only after a winning publication.
async fn publish_plan_stage_failure(
    plan_commit: &PlanCommit,
    inputs: &PlanStageInputs<'_>,
    expected_revision: u64,
    error: String,
) -> String {
    match plan_commit.publish_plan_failure(expected_revision, &error) {
        PlanFailurePublication::Published => {
            append_trajectory_event(
                inputs.trajectory_path,
                json!({
                    "event": "plan_failed",
                    "dispatch_id": inputs.dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
                    "error": error,
                }),
            );
            // #971: the BOARD-FIRST row must be closed when this failure is
            // the winner. This runs only after the status publication won, so
            // it cannot clobber a concurrent terminal winner.
            if let Err(kanban_err) = update_kanban_state(
                inputs.server,
                inputs.dispatch_id,
                "TASK_STATE_FAILED",
                None,
                Some(false),
            )
            .await
            {
                eprintln!(
                    "[dispatch-v2] failed to mark dispatch {} FAILED in kanban after plan failure: {}",
                    inputs.dispatch_id, kanban_err
                );
            }
            error
        }
        // A newer authoritative state won: leave both status and kanban.
        PlanFailurePublication::Refused => error,
        // The failure could not be persisted: report reconciliation unknown
        // and do not fabricate a terminal kanban state.
        PlanFailurePublication::PersistFailed => {
            format!("{error} (plan failure could not be persisted; reconciliation unknown)")
        }
    }
}
