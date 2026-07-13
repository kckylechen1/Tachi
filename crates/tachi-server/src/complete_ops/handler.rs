use chrono::Utc;
use serde_json::json;

use crate::facade_memory_ops::shape_complete_response;
use crate::hub_ops::handle_distill_trajectory;
use crate::memory_search_ops::save_eval_memory;
use crate::tool_params::{DistillTrajectoryParams, TachiCompleteParams};
use crate::MemoryServer;

use super::eval_record::{build_complete_eval_record, CompleteEvalRecord};
use super::kanban::read_kanban_snapshot;
use super::lessons::run_lesson_post_complete_hook;

/// The #878-A completion-predicate verdict, resolved ONCE per completion so the
/// canonical outcome row and the kanban row agree on the same machine verdict
/// (#773 Layer-2 ②). `verdict_tag` is the short predicate tag
/// (`pass`/`fail`/`unverified`); `new_state`/`reviewed_flag` are the resolved
/// kanban terminal state; `override_reason` is `Some` only when the predicate
/// intercepted a false self-reported success.
struct CompletionVerdict {
    declared: bool,
    verdict_tag: &'static str,
    new_state: &'static str,
    reviewed_flag: bool,
    override_reason: Option<String>,
}

pub(crate) async fn handle_tachi_complete(
    server: &MemoryServer,
    mut params: TachiCompleteParams,
) -> Result<String, String> {
    let now = Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let ts = now.format("%Y%m%dT%H%M%SZ").to_string();

    // #773 (S2 prep): eval rows carry dispatch_id but almost never issue_ref
    // because the calling agent must manually re-supply it and mostly
    // doesn't (live: 0/15 at time of design). Auto-inject from the
    // dispatch's own kanban card — which already has issue_ref on file from
    // launch (`init_kanban_task`) — when the caller gave us dispatch_id but
    // no issue_ref. Fail-safe: any lookup miss leaves params.issue_ref as
    // None and completion proceeds unchanged; this must never fail the
    // completion. Runs before `build_complete_eval_record` so the injected
    // value flows into the eval metadata, the kanban/flow completion
    // payloads, and the review bundle exactly as a caller-supplied
    // issue_ref would have.
    if params.issue_ref.as_deref().is_none_or(str::is_empty) {
        if let Some(dispatch_id) = params
            .dispatch_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        {
            params.issue_ref =
                super::flow_link::resolve_issue_ref_for_dispatch(server, dispatch_id, None);
        }
    }

    let CompleteEvalRecord {
        task_id,
        path,
        safe_task,
        safe_agent,
        safe_notes,
        safe_skills_used,
        safe_evidence_refs,
        safe_tests_run,
        safe_trajectory,
        safe_subagents,
        safe_feedback_rules,
        outcome_norm,
        diff_present,
        verification_present,
        secret_redactions,
        mem_params,
    } = build_complete_eval_record(&params, &date, &ts);

    // Writes never auto-select a named project from machine state (workspace
    // detection / "single project on disk"): that silently reroutes eval rows
    // away from the server's own stores. Callers must pass `project` (or the
    // server must have a bound project DB) for project-scoped persistence.
    let save_result = save_eval_memory(server, mem_params).await?;
    let save_json: serde_json::Value = serde_json::from_str(&save_result)
        .unwrap_or_else(|_| serde_json::json!({"raw": save_result}));

    // Extract the eval memory ID for the kanban hook
    let eval_memory_id = save_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(&task_id)
        .to_string();

    // #773 Layer-2 ②: resolve the #878-A completion predicate BEFORE writing
    // the canonical outcome row, so `execution_outcome` records the MACHINE
    // verdict (a false self-reported success intercepted to `failed`), not the
    // raw self-report. The kanban block below reuses this exact verdict rather
    // than recomputing it. `None` when there is no dispatch_id (no predicate to
    // apply; the outcome write is skipped anyway).
    let completion_verdict = params
        .dispatch_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|did| {
            let (predicate_run_dir, declared_predicate, predicate_cwd) =
                crate::dispatch_ops::resolve_completion_predicate_context(did);
            let predicate_output = predicate_run_dir
                .as_deref()
                .and_then(|dir| std::fs::read_to_string(dir.join("result.md")).ok())
                .unwrap_or_default();
            let empty_run_dir = std::path::PathBuf::new();
            let verdict = crate::dispatch_ops::evaluate_completion_predicate(
                declared_predicate.as_ref(),
                predicate_run_dir.as_deref().unwrap_or(&empty_run_dir),
                predicate_cwd.as_deref(),
                &predicate_output,
            );
            let (new_state, reviewed_flag, override_reason) =
                crate::dispatch_ops::resolve_completion_state(params.outcome.as_str(), &verdict);
            CompletionVerdict {
                declared: declared_predicate.is_some(),
                verdict_tag: verdict.tag(),
                new_state,
                reviewed_flag,
                override_reason,
            }
        });

    // Machine-resolved execution outcome + interception class for the outcome
    // row: the value AFTER the predicate has had its chance to intercept.
    let (machine_execution_outcome, outcome_error_class) = match &completion_verdict {
        Some(cv) => (
            crate::dispatch_ops::execution_outcome_for_kanban_state(cv.new_state).to_string(),
            cv.override_reason.as_ref().map(|_| "false_success"),
        ),
        None => (outcome_norm.clone(), None),
    };

    // #773 v4 (sol carve): write the ONE canonical dispatch_outcomes row
    // FIRST — before any other derive (kanban, signatures, precedents,
    // lesson hooks) touches state. A derivation failure downstream must
    // never lose this row; this call itself is fail-safe (see module docs)
    // and never fails the completion. `reported_outcome` keeps the raw
    // self-report; `execution_outcome` is the machine verdict computed above.
    //
    // #774 round 2: `reported_outcome` must be the agent's VERBATIM claim
    // (trim only, no case-folding) — `outcome_norm` is `params.outcome`
    // lowercased for the machine-side bucketing logic above/in
    // `build_complete_eval_record`, not the self-report itself. Passing
    // `outcome_norm` here silently rewrote "Complete " -> "complete" in the
    // row the module doc promises is verbatim.
    let reported_outcome_verbatim = params.outcome.trim();
    let dispatch_outcome_status = super::dispatch_outcome::record_complete_outcome(
        server,
        &params,
        &eval_memory_id,
        reported_outcome_verbatim,
        &machine_execution_outcome,
        outcome_error_class,
        verification_present,
        diff_present,
        &safe_evidence_refs,
    );

    let mut pipeline_status = serde_json::json!({
        "dispatch_outcome": dispatch_outcome_status,
        "kanban_update": "skipped (no dispatch_id)",
        "distill_trajectory": "skipped (no trajectory data)",
        "skill_evolve": "skipped",
        "continuity_events": "pending",
        "post_complete_hooks": "pending",
    });
    let mut completion_warning: Option<String> = None;

    let task_event_payload = json!({
        "task_id": task_id.clone(),
        "task": safe_task.clone(),
        "agent": safe_agent.clone(),
        "outcome": outcome_norm.clone(),
        "task_type": params.task_type.clone(),
        "profile": params.profile.clone(),
        "risk": params.risk.clone(),
        "duration_ms": params.duration_ms,
        "skills_used": safe_skills_used.clone(),
        "cost_tokens": params.cost_tokens,
        "cost_usd": params.cost_usd,
        "quality_score": params.quality_score,
        "verification_present": verification_present,
        "diff_present": diff_present,
        "evidence_refs": safe_evidence_refs.clone(),
        "tests_run": safe_tests_run.clone(),
        "eval_memory_id": eval_memory_id.clone(),
        "eval_path": path.clone(),
        "dispatch_id": params.dispatch_id.clone(),
        "flow_id": params.flow_id.clone(),
        "issue_ref": params.issue_ref.clone(),
        "pr_ref": params.pr_ref.clone(),
        "feedback_rules_applied": safe_feedback_rules.clone(),
    });
    let subagent_event_payloads = safe_subagents.as_array().cloned().unwrap_or_default();
    pipeline_status["continuity_events"] = crate::continuity_ops::emit_task_completion_events(
        server,
        task_event_payload,
        &subagent_event_payloads,
        params.project.as_deref(),
    );
    let pattern_feedback_refs =
        crate::continuity_ops::pattern_feedback_refs_from_strings(&safe_evidence_refs);
    let default_pattern_outcome = match outcome_norm.as_str() {
        "success" => "hit",
        "failure" => "miss",
        _ => "seen",
    };
    pipeline_status["pattern_feedback"] = if pattern_feedback_refs.is_empty() {
        json!("skipped (no pattern refs in evidence_refs)")
    } else {
        crate::continuity_ops::emit_pattern_feedback_for_refs(
            server,
            params.project.as_deref(),
            &pattern_feedback_refs,
            default_pattern_outcome,
            Some(&safe_task),
            safe_notes.as_deref(),
            "tachi_complete",
            json!({
                "task_id": task_id,
                "outcome": outcome_norm,
                "eval_memory_id": eval_memory_id,
                "source": "tachi_complete.evidence_refs",
            }),
        )
    };

    if let Some(ref trajectory) = safe_trajectory {
        if let Some(trace_arr) = trajectory.as_array() {
            if !trace_arr.is_empty() && outcome_norm == "success" {
                let server_clone = server.clone();
                let task_desc = safe_task.clone();
                let agent = safe_agent.clone();
                let trace = trace_arr.clone();
                let skills_used = safe_skills_used.clone();
                let skill_path = if skills_used.is_empty() {
                    format!("/skills/auto/{}", task_id)
                } else {
                    skills_used[0].clone()
                };
                pipeline_status["distill_trajectory"] = json!("enqueued");
                pipeline_status["skill_evolve"] = json!("will follow distill if successful");
                tokio::spawn(async move {
                    let distill_params = DistillTrajectoryParams {
                        task_description: task_desc,
                        execution_trace: trace,
                        final_outcome: serde_json::json!({"outcome": "success", "agent": agent}),
                        agent_id: agent.clone(),
                        skill_path,
                        skill_id: None,
                        importance: None,
                        domain: None,
                        project: None,
                        scope: "project".to_string(),
                    };
                    match handle_distill_trajectory(&server_clone, distill_params).await {
                        Ok(r) => eprintln!(
                            "[tachi_complete/worker] distill OK: {}",
                            &r[..r.len().min(200)]
                        ),
                        Err(e) => eprintln!("[tachi_complete/worker] distill failed: {e}"),
                    }
                });
            }
        }
    }

    // --- Kanban Hook: auto-update task board ---
    if let Some(ref did) = params.dispatch_id {
        // Completion derives the authoritative terminal state first. The
        // subsequent receipt emission and presence release are best-effort,
        // downstream lifecycle bookkeeping and never block that transition.
        // #878-A: gate the COMPLETED/reviewed write behind a machine-checkable
        // completion predicate declared at dispatch time. A self-reported
        // outcome="success" only earns a *reviewed* COMPLETED when the declared
        // predicate is satisfied (Pass). An unsatisfied predicate (Fail)
        // intercepts the false success and routes the row to FAILED. No
        // predicate (Unverified) still lands COMPLETED, but reviewed=false —
        // success could not be machine-verified, matching the watchdog's
        // conservative posture. failure/partial/aborted keep their mapping and
        // stay reviewed (an explicit tachi_complete is a deliberate close).
        //
        // Reuse the verdict computed above (#773 ②) — the outcome row and the
        // kanban row MUST agree on the same machine verdict, so it is resolved
        // once. `completion_verdict` is always Some inside this dispatch_id arm.
        let CompletionVerdict {
            declared,
            verdict_tag,
            new_state,
            reviewed_flag,
            override_reason: predicate_override_reason,
        } = completion_verdict
            .expect("completion_verdict is Some when dispatch_id is present");

        // One shared terminal receipt path. It is best-effort by contract: a
        // receipt DB failure cannot prevent this explicit lifecycle close.
        crate::claims_ops::emit_terminal_receipt(
            server,
            did,
            new_state,
            "Dispatch reached a terminal state through explicit completion.",
            Some(&eval_memory_id),
        );
        crate::claims_ops::release_claim_for_dispatch(server, did, "complete");

        pipeline_status["completion_predicate"] = json!({
            "declared": declared,
            "verdict": verdict_tag,
            "outcome_reported": params.outcome.clone(),
            "resolved_state": new_state,
            "reviewed": reviewed_flag,
            "reason": predicate_override_reason.clone(),
        });
        if let Some(reason) = predicate_override_reason.clone() {
            eprintln!(
                "[tachi_complete] completion predicate intercepted false success for dispatch_id={did}: {reason}"
            );
            completion_warning = Some(reason);
        }

        match crate::dispatch_ops::update_kanban_state(
            server,
            did,
            new_state,
            Some(&eval_memory_id),
            Some(reviewed_flag),
        )
        .await
        {
            Ok(()) => match read_kanban_snapshot(server, did) {
                Ok(Some(snapshot))
                    if snapshot.state.as_deref() == Some(new_state)
                        && snapshot.eval_ledger_id.as_deref() == Some(eval_memory_id.as_str())
                        && snapshot.reviewed == Some(reviewed_flag) =>
                {
                    pipeline_status["kanban_update"] = json!({
                        "status": "updated",
                        "dispatch_id": did,
                        "scope": snapshot.scope,
                        "state": new_state,
                        "eval_memory_id": eval_memory_id,
                        "reviewed": reviewed_flag,
                    });
                }
                Ok(Some(snapshot)) => {
                    let warning = format!(
                            "kanban update verification failed after eval persistence for dispatch_id={did}, task_id={}, task={}: expected state={new_state}, eval_memory_id={}, reviewed={reviewed_flag} but found scope={}, state={:?}, eval_memory_id={:?}, reviewed={:?}",
                            task_id,
                            safe_task,
                            eval_memory_id,
                            snapshot.scope,
                            snapshot.state,
                            snapshot.eval_ledger_id,
                            snapshot.reviewed
                        );
                    eprintln!("[tachi_complete] {warning}");
                    completion_warning = Some(warning.clone());
                    pipeline_status["kanban_update"] = json!({
                        "status": "stale",
                        "dispatch_id": did,
                        "scope": snapshot.scope,
                        "state": snapshot.state,
                        "eval_memory_id": snapshot.eval_ledger_id,
                        "reviewed": snapshot.reviewed,
                        "expected_state": new_state,
                        "expected_eval_memory_id": eval_memory_id,
                    });
                }
                Ok(None) => {
                    let warning = format!(
                            "kanban card missing after eval persistence for dispatch_id={did}, task_id={}, task={}",
                            task_id, safe_task
                        );
                    eprintln!("[tachi_complete] {warning}");
                    completion_warning = Some(warning.clone());
                    pipeline_status["kanban_update"] = json!({
                        "status": "missing",
                        "dispatch_id": did,
                        "expected_state": new_state,
                        "expected_eval_memory_id": eval_memory_id,
                        "reviewed": reviewed_flag,
                    });
                }
                Err(error) => {
                    let warning = format!(
                            "kanban update readback failed after eval persistence for dispatch_id={did}, task_id={}, task={}: {error}",
                            task_id, safe_task
                        );
                    eprintln!("[tachi_complete] {warning}");
                    completion_warning = Some(warning.clone());
                    pipeline_status["kanban_update"] = json!({
                        "status": "readback_failed",
                        "dispatch_id": did,
                        "expected_state": new_state,
                        "expected_eval_memory_id": eval_memory_id,
                        "reviewed": reviewed_flag,
                        "error": error,
                    });
                }
            },
            Err(error) => {
                let warning = format!(
                    "kanban update failed after eval persistence for dispatch_id={did}, task_id={}, task={}: {error}",
                    task_id, safe_task
                );
                eprintln!("[tachi_complete] {warning}");
                completion_warning = Some(warning.clone());
                pipeline_status["kanban_update"] = json!({
                    "status": "failed",
                    "dispatch_id": did,
                    "state": new_state,
                    "eval_memory_id": eval_memory_id,
                    "reviewed": reviewed_flag,
                    "error": error,
                });
            }
        }
    }

    let dispatch_completion_link = {
        let dispatch_id = params
            .dispatch_id
            .as_deref()
            .filter(|dispatch_id| !dispatch_id.is_empty());
        let flow_id = dispatch_id.and_then(|dispatch_id| {
            super::flow_link::resolve_flow_id_for_dispatch(
                server,
                dispatch_id,
                params.flow_id.as_deref(),
            )
        });
        match (flow_id.as_deref(), dispatch_id) {
            (Some(flow_id), Some(dispatch_id)) => {
                let completion_payload = json!({
                    "task_id": task_id.clone(),
                    "task": safe_task.clone(),
                    "agent": safe_agent.clone(),
                    "outcome": outcome_norm.clone(),
                    "profile": params.profile.clone(),
                    "risk": params.risk.clone(),
                    "eval_memory_id": eval_memory_id.clone(),
                    "eval_path": path.clone(),
                    "verification_present": verification_present,
                    "diff_present": diff_present,
                    "evidence_refs": safe_evidence_refs.clone(),
                    "tests_run": safe_tests_run.clone(),
                    "subagent_count": params.subagents.len(),
                    "feedback_rules_applied": safe_feedback_rules.clone(),
                    "skills_used": safe_skills_used.clone(),
                    "issue_ref": params.issue_ref.clone(),
                    "pr_ref": params.pr_ref.clone(),
                    "duration_ms": params.duration_ms,
                    "cost_tokens": params.cost_tokens,
                    "cost_usd": params.cost_usd,
                    "quality_score": params.quality_score,
                });
                match crate::task_lifecycle::mark_task_dispatch_completion(
                    flow_id,
                    dispatch_id,
                    completion_payload,
                ) {
                    Ok(value) => value,
                    Err(error) => json!({
                        "recorded": false,
                        "flow_id": flow_id,
                        "dispatch_id": dispatch_id,
                        "error": error,
                    }),
                }
            }
            (None, Some(dispatch_id)) => json!({
                "recorded": false,
                "dispatch_id": dispatch_id,
                "reason": "missing flow_id (not found on kanban card or dispatch run ledger)",
            }),
            (Some(flow_id), None) => json!({
                "recorded": false,
                "flow_id": flow_id,
                "reason": "missing dispatch_id",
            }),
            _ => json!({
                "recorded": false,
                "reason": "missing dispatch_id",
            }),
        }
    };
    pipeline_status["dispatch_completion_link"] = dispatch_completion_link;

    pipeline_status["signature_recording"] =
        crate::signature_evidence::record_complete_signatures(server, &params);

    // Precedent capture (#950 slice 1): persist caller-supplied structured
    // leader rulings as /precedents rows. Best-effort — a malformed ruling is
    // skipped + warned and never fails completion (the primary contract).
    pipeline_status["precedent_recording"] =
        crate::precedent_ops::record_complete_rulings(server, &params).await;

    pipeline_status["post_complete_hooks"] = run_lesson_post_complete_hook(
        server,
        &params,
        &outcome_norm,
        safe_notes.as_deref(),
        &safe_task,
        &safe_agent,
        &safe_skills_used,
        &date,
        &task_id,
    )
    .await;

    let mut next_steps =
        vec!["Use tachi_search with 'eval' keyword to find related outcomes.".to_string()];
    if params
        .worktree
        .as_deref()
        .is_some_and(|worktree| !worktree.trim().is_empty())
    {
        next_steps.push("For worktree-based dispatch, run approve_merge when ready.".to_string());
    } else {
        next_steps.push(
            "No worktree was recorded; no approve_merge step is implied by this completion."
                .to_string(),
        );
    }

    let mut review_bundle = serde_json::json!({
        "recorded": true,
        "task_id": task_id,
        "task": safe_task,
        "agent": safe_agent,
        "path": path,
        "outcome": outcome_norm,
        "dispatch_id": params.dispatch_id,
        "profile": params.profile,
        "risk": params.risk,
        "quality_score": params.quality_score,
        "flow_id": params.flow_id,
        "issue_ref": params.issue_ref,
        "pr_ref": params.pr_ref,
        "evidence_refs": safe_evidence_refs,
        "tests_run": safe_tests_run,
        "diff_present": diff_present,
        "subagent_count": params.subagents.len(),
        "subagents": safe_subagents,
        "eval_entry": save_json,
        "next_steps": next_steps,
        "pipeline": pipeline_status,
        "secret_redactions": secret_redactions,
    });
    if let (Some(warning), Some(obj)) = (completion_warning, review_bundle.as_object_mut()) {
        crate::mcp_proxy::append_warning(obj, warning);
    }

    let response = shape_complete_response(review_bundle, params.format.as_deref());
    serde_json::to_string(&response)
        .map_err(|e| format!("Failed to serialize review bundle: {}", e))
}
