use super::super::kanban_helpers::update_kanban_state;
use super::*;

// ─── V2 plan stage (optional Stage 1) ────────────────────────────────────────

pub(super) struct PlanStageInputs<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) params: &'a TachiDispatchParams,
    pub(super) dispatch_id: &'a str,
    pub(super) agent_norm: &'a str,
    pub(super) resolved_profile: &'a ResolvedDispatchProfile,
    pub(super) profile_payload: &'a Value,
    pub(super) base_prompt: &'a str,
    pub(super) plan_path: &'a Path,
    pub(super) prompt_md_path: &'a Path,
    pub(super) context_md_path: &'a Path,
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) capability_bundle_card: &'a Value,
    pub(super) capability_bundle_file: &'a str,
    pub(super) feedback_rules_trace: &'a Value,
    pub(super) v2_decision: V2Decision,
}

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
        let label = format!("dispatch-plan-{}", inputs.dispatch_id);
        let plan_timeout = Duration::from_secs(plan_timeout_secs());
        let plan_fut = run_plan_stage(inputs.server, &inputs.params.task, &label);
        let plan_outcome = match tokio::time::timeout(plan_timeout, plan_fut).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                append_trajectory_event(
                    inputs.trajectory_path,
                    json!({
                        "event": "plan_failed",
                        "dispatch_id": inputs.dispatch_id,
                        "timestamp": Utc::now().to_rfc3339(),
                        "error": e,
                    }),
                );
                write_status_json(
                    inputs.workspace_dir,
                    inputs.dispatch_id,
                    true,
                    None,
                    None,
                    "failed",
                    Some(1),
                    None,
                    None,
                    None,
                    Some(json!({
                        "error": e,
                        "capability_bundle": inputs.capability_bundle_card.clone(),
                    })),
                );
                // #971: the kanban row now exists before this stage runs
                // (BOARD-FIRST) — a plan failure must close it, not leave it
                // orphaned in TASK_STATE_WORKING. Mirrors the watchdog's own
                // failure-close pattern in execution.rs (reused helper, same
                // terminal state + unreviewed flag).
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
                return Err(e);
            }
            Err(_) => {
                let e = format!(
                    "dispatch v2 stage1 (plan) timed out after {}s",
                    plan_timeout.as_secs()
                );
                append_trajectory_event(
                    inputs.trajectory_path,
                    json!({
                        "event": "plan_failed",
                        "dispatch_id": inputs.dispatch_id,
                        "timestamp": Utc::now().to_rfc3339(),
                        "error": e,
                    }),
                );
                write_status_json(
                    inputs.workspace_dir,
                    inputs.dispatch_id,
                    true,
                    None,
                    None,
                    "failed",
                    Some(1),
                    None,
                    None,
                    None,
                    Some(json!({
                        "error": e,
                        "capability_bundle": inputs.capability_bundle_card.clone(),
                    })),
                );
                // #971: same as above — plan-stage TIMEOUT must also close
                // the kanban row (this branch previously had no kanban row
                // to close at all, since BOARD-FIRST didn't exist yet).
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
                        "[dispatch-v2] failed to mark dispatch {} FAILED in kanban after plan timeout: {}",
                        inputs.dispatch_id, kanban_err
                    );
                }
                return Err(e);
            }
        };

        // Persist plan.md (overwrites the V1 placeholder).
        crate::utils::write_owner_only_file_atomic(
            inputs.plan_path,
            plan_outcome.plan_md.as_bytes(),
        )
        .map_err(|e| format!("Failed to write plan.md: {e}"))?;
        let sections = parse_plan_sections(&plan_outcome.plan_md);

        plan_duration_ms = Some(plan_outcome.duration_ms);
        let pgen_at = Utc::now().to_rfc3339();
        plan_generated_at = Some(pgen_at.clone());
        append_trajectory_event(
            inputs.trajectory_path,
            json!({
                "event": "plan_generated",
                "dispatch_id": inputs.dispatch_id,
                "timestamp": pgen_at,
                "duration_ms": plan_outcome.duration_ms,
                "bytes": plan_outcome.plan_md.len(),
                "sections_complete": sections.is_complete(),
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
                Some(json!({
                    "capability_bundle": inputs.capability_bundle_card.clone(),
                })),
            );
            append_trajectory_event(
                inputs.trajectory_path,
                json!({
                    "event": "plan_pending_review",
                    "dispatch_id": inputs.dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
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
            // `kanban::KANBAN_DISPATCH_NON_TERMINAL_STATES`
            // (`kanban.rs`), so an abandoned row (caller never re-dispatches)
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
            let response = json!({
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
                "agent": inputs.agent_norm,
                "profile": inputs.profile_payload,
                "selected_profile": inputs.resolved_profile.selected_profile,
                "tool_access": inputs.resolved_profile.mcp_access,
                "route_explanation": inputs.resolved_profile.route_explanation,
                "fallback_chain": inputs.resolved_profile.fallback_chain,
                "issue_ref": inputs.params.issue_ref,
                "pr_ref": inputs.params.pr_ref,
                "flow_id": inputs.params.flow_id,
                "auto_capability_bundle": inputs.resolved_profile.auto_capability_bundle,
                "capability_bundle": inputs.capability_bundle_card,
                "capability_bundle_file": inputs.capability_bundle_file,
                "feedback_rules": inputs.feedback_rules_trace.clone(),
                "v2": true,
                "plan_review_status": "pending_review",
                "message": "Plan generated. DISPATCH_V2_PLAN_REVIEW=true — execute stage paused. Audit plan.md and re-dispatch with the env var unset to proceed.",
                "suggested_complete_command": suggested_complete_payload(inputs.dispatch_id, inputs.agent_norm, inputs.params),
                "plan_file": inputs.plan_path.to_string_lossy(),
                "prompt_file": inputs.prompt_md_path.to_string_lossy(),
                "context_file": inputs.context_md_path.to_string_lossy(),
                "trajectory_file": inputs.trajectory_path.to_string_lossy(),
                "run_dir": inputs.workspace_dir.to_string_lossy(),
            });
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
        // it contains the plan + implement-plan skill.
        append_trajectory_event(
            inputs.trajectory_path,
            json!({
                "event": "plan_approved",
                "dispatch_id": inputs.dispatch_id,
                "timestamp": Utc::now().to_rfc3339(),
                "auto": true,
            }),
        );
        prompt = build_execute_prompt(
            &plan_outcome.plan_md,
            &inputs.params.task,
            inputs.base_prompt,
        );
    }

    Ok(PlanStageOutcome {
        prompt,
        plan_duration_ms,
        plan_generated_at,
        early_response: None,
    })
}
