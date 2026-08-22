use chrono::Utc;
use serde_json::{json, Value};

use crate::facade_memory_ops::shape_complete_response;
use crate::memory_search_ops::save_eval_memory;
use crate::tool_params::TachiCompleteParams;
use crate::MemoryServer;

use super::eval_record::{build_complete_eval_record, CompleteEvalRecord};
use super::kanban::read_kanban_snapshot;
use super::lessons::run_lesson_post_complete_hook;

const COMPLETION_RECEIPT_STATUS_MAX_BYTES: usize = 1024 * 1024;

#[cfg(test)]
#[derive(Clone)]
struct CompletionStatusReadBarrier {
    read: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
fn completion_status_read_barrier() -> &'static std::sync::Mutex<Option<CompletionStatusReadBarrier>>
{
    static BARRIER: std::sync::OnceLock<std::sync::Mutex<Option<CompletionStatusReadBarrier>>> =
        std::sync::OnceLock::new();
    BARRIER.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
struct CompletionStatusReadBarrierGuard;

#[cfg(test)]
impl Drop for CompletionStatusReadBarrierGuard {
    fn drop(&mut self) {
        *completion_status_read_barrier()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(test)]
fn install_completion_status_read_barrier(
    read: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
) -> CompletionStatusReadBarrierGuard {
    *completion_status_read_barrier()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(CompletionStatusReadBarrier { read, resume });
    CompletionStatusReadBarrierGuard
}

#[cfg(test)]
fn pause_completion_after_status_read() {
    let barrier = completion_status_read_barrier()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(barrier) = barrier {
        barrier.read.wait();
        barrier.resume.wait();
    }
}

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

fn ensure_completion_artifact_read_support(dispatch_id: Option<&str>) -> Result<(), String> {
    if dispatch_id.is_some_and(|dispatch_id| !dispatch_id.trim().is_empty()) {
        crate::dispatch_ops::ensure_descriptor_reads_supported()?;
    }
    Ok(())
}

fn admit_managed_completion(
    server: &MemoryServer,
    dispatch_id: Option<&str>,
    persist_admission: bool,
) -> Result<(), String> {
    let Some(dispatch_id) = dispatch_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(());
    };
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Ok(());
    }
    let run_dir = resolved_completion_run_dir(&server.tachi_home_dir(), dispatch_id)?;
    let status_lock = crate::dispatch_ops::status_json_lock_for(&run_dir);
    let _status_guard = status_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let status_path = run_dir.join("status.json");
    let Some(mut status) = crate::task_lifecycle::read_json_file(&status_path)? else {
        return Ok(());
    };
    let object = status.as_object_mut().ok_or_else(|| {
        format!(
            "managed completion status is not an object: {}",
            status_path.display()
        )
    })?;
    if object
        .get("execution_classification")
        .and_then(Value::as_str)
        != Some("managed_custom")
    {
        return Ok(());
    }
    if crate::managed_run_control::cancellation_blocks_terminal_writer(object) {
        return Err("managed cancellation owns terminal completion".to_string());
    }
    if !persist_admission {
        return Ok(());
    }
    if object.contains_key("completion_recovery") || object.contains_key("resolved_completion") {
        return Ok(());
    }
    object.insert(
        "completion_recovery".to_string(),
        json!({ "status": "completion_admitted" }),
    );
    crate::managed_run_control::advance_status_revision(object)?;
    let body = serde_json::to_vec_pretty(&status)
        .map_err(|error| format!("serialize managed completion admission: {error}"))?;
    crate::utils::write_owner_only_file_atomic(&status_path, &body)
        .map_err(|error| format!("persist managed completion admission: {error}"))
}

/// The admission marker is a narrow cancellation fence, not evidence. If the
/// eval write never starts, remove it so a retry or cancellation is not
/// stranded behind a phantom completion owner.
fn revoke_managed_completion_admission(
    server: &MemoryServer,
    dispatch_id: Option<&str>,
) -> Result<(), String> {
    let Some(dispatch_id) = dispatch_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(());
    };
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Ok(());
    }
    let run_dir = resolved_completion_run_dir(&server.tachi_home_dir(), dispatch_id)?;
    let status_lock = crate::dispatch_ops::status_json_lock_for(&run_dir);
    let _status_guard = status_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let status_path = run_dir.join("status.json");
    let Some(mut status) = crate::task_lifecycle::read_json_file(&status_path)? else {
        return Ok(());
    };
    let object = status.as_object_mut().ok_or_else(|| {
        format!(
            "managed completion status is not an object: {}",
            status_path.display()
        )
    })?;
    if object.get("completion_recovery") == Some(&json!({ "status": "completion_admitted" }))
        && !object.contains_key("resolved_completion")
    {
        object.remove("completion_recovery");
        crate::managed_run_control::advance_status_revision(object)?;
        let body = serde_json::to_vec_pretty(&status)
            .map_err(|error| format!("serialize managed completion admission rollback: {error}"))?;
        crate::utils::write_owner_only_file_atomic(&status_path, &body)
            .map_err(|error| format!("persist managed completion admission rollback: {error}"))?;
    }
    Ok(())
}

/// Persist the resolved close in the dispatch's own run receipt before
/// projecting it to kanban. Kanban is a derived view and may be missing or
/// temporarily unreadable; the watchdog therefore needs this durable source
/// of truth to avoid collapsing a deliberate partial close to exit-0 success.
fn persist_resolved_completion_receipt(
    server: &MemoryServer,
    dispatch_id: &str,
    new_state: &str,
    eval_memory_id: &str,
    reviewed: bool,
) -> Result<(), String> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Err(format!(
            "cannot persist resolved completion receipt: invalid dispatch_id={dispatch_id:?}"
        ));
    }

    let run_dir = resolved_completion_run_dir(&server.tachi_home_dir(), dispatch_id)?;

    persist_resolved_completion_receipt_at(
        &run_dir,
        dispatch_id,
        new_state,
        eval_memory_id,
        reviewed,
    )
}

fn resolved_completion_run_dir(
    home_dir: &std::path::Path,
    dispatch_id: &str,
) -> Result<std::path::PathBuf, String> {
    // The run directory is the dispatch receipt's authority boundary. Never
    // manufacture it from a caller-supplied id: a missing or symlink-escaped
    // directory is a broken dispatch, not a place to create new truth.
    let (run_dir, _, _) =
        crate::dispatch_ops::resolve_completion_predicate_context(home_dir, dispatch_id)?;
    run_dir.ok_or_else(|| {
        format!(
            "cannot persist resolved completion receipt for dispatch_id={dispatch_id}: \
             validated run directory is unavailable"
        )
    })
}

fn persist_resolved_completion_receipt_at(
    run_dir: &std::path::Path,
    dispatch_id: &str,
    new_state: &str,
    eval_memory_id: &str,
    reviewed: bool,
) -> Result<(), String> {
    let status_lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _status_guard = status_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let status_path = run_dir.join("status.json");
    let mut status = match crate::dispatch_ops::read_text_file_within(
        run_dir,
        &status_path,
        COMPLETION_RECEIPT_STATUS_MAX_BYTES,
    )
    .map_err(|error| {
        format!(
            "cannot persist resolved completion receipt for dispatch_id={dispatch_id}: \
                 {error}"
        )
    })? {
        Some(raw) => serde_json::from_str(&raw).map_err(|error| {
            format!(
                "cannot persist resolved completion receipt for dispatch_id={dispatch_id}: \
                 parse {}: {error}",
                status_path.display()
            )
        })?,
        None => json!({ "dispatch_id": dispatch_id }),
    };
    #[cfg(test)]
    pause_completion_after_status_read();
    let status_object = status.as_object_mut().ok_or_else(|| {
        format!(
            "cannot persist resolved completion receipt for dispatch_id={dispatch_id}: \
             {} is not a JSON object",
            status_path.display()
        )
    })?;
    crate::managed_run_control::reconcile_pending_cancellation_unavailable(
        status_object,
        "completion_winner",
    );
    if crate::managed_run_control::cancellation_blocks_terminal_writer(status_object) {
        return Err("cannot overwrite a managed cancellation".to_string());
    }
    status_object.insert(
        "resolved_completion".to_string(),
        json!({
            "state": new_state,
            "closure_kind": if new_state == "TASK_STATE_INPUT_REQUIRED" {
                Value::String("partial".to_string())
            } else {
                Value::Null
            },
            "eval_ledger_id": eval_memory_id,
            "reviewed": reviewed,
            "recorded_at": Utc::now().to_rfc3339(),
        }),
    );
    // A prior lock exhaustion records an explicit recovery marker rather than
    // claiming final completion. The canonical row is durable now, so replace
    // that marker in the same atomic status write.
    status_object.remove("completion_recovery");
    crate::managed_run_control::advance_status_revision(status_object)?;
    let body = serde_json::to_string_pretty(&status).map_err(|error| {
        format!(
            "cannot serialize resolved completion receipt for dispatch_id={dispatch_id}: {error}"
        )
    })?;
    crate::utils::write_owner_only_file_atomic(&status_path, body.as_bytes()).map_err(|error| {
        format!(
            "cannot persist resolved completion receipt for dispatch_id={dispatch_id}: \
             write {}: {error}",
            status_path.display()
        )
    })
}

/// Persist an explicit, idempotent marker when the eval evidence is durable
/// but the canonical `dispatch_outcomes` row is still pending. This is NOT a
/// terminal completion receipt: it clears any stale terminal marker so a
/// watchdog or a later caller cannot mistake local lock exhaustion for a
/// fully-recorded completion.
fn persist_pending_completion_recovery_receipt(
    server: &MemoryServer,
    dispatch_id: &str,
    new_state: &str,
    eval_memory_id: &str,
    reviewed: bool,
    dispatch_outcome: &Value,
) -> Result<(), String> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Err(format!(
            "cannot persist completion recovery receipt: invalid dispatch_id={dispatch_id:?}"
        ));
    }
    let run_dir = resolved_completion_run_dir(&server.tachi_home_dir(), dispatch_id)?;
    persist_pending_completion_recovery_receipt_at(
        &run_dir,
        dispatch_id,
        new_state,
        eval_memory_id,
        reviewed,
        dispatch_outcome,
    )
}

fn persist_pending_completion_recovery_receipt_at(
    run_dir: &std::path::Path,
    dispatch_id: &str,
    new_state: &str,
    eval_memory_id: &str,
    reviewed: bool,
    dispatch_outcome: &Value,
) -> Result<(), String> {
    let status_lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _status_guard = status_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let status_path = run_dir.join("status.json");
    let mut status = match crate::dispatch_ops::read_text_file_within(
        run_dir,
        &status_path,
        COMPLETION_RECEIPT_STATUS_MAX_BYTES,
    )
    .map_err(|error| {
        format!("cannot persist completion recovery receipt for dispatch_id={dispatch_id}: {error}")
    })? {
        Some(raw) => serde_json::from_str(&raw).map_err(|error| {
            format!(
                "cannot persist completion recovery receipt for dispatch_id={dispatch_id}: \
                 parse {}: {error}",
                status_path.display()
            )
        })?,
        None => json!({ "dispatch_id": dispatch_id }),
    };
    #[cfg(test)]
    pause_completion_after_status_read();
    let status_object = status.as_object_mut().ok_or_else(|| {
        format!(
            "cannot persist completion recovery receipt for dispatch_id={dispatch_id}: \
             {} is not a JSON object",
            status_path.display()
        )
    })?;
    crate::managed_run_control::reconcile_pending_cancellation_unavailable(
        status_object,
        "completion_winner",
    );
    if crate::managed_run_control::cancellation_blocks_terminal_writer(status_object) {
        return Err("cannot overwrite a managed cancellation".to_string());
    }
    let recovery = json!({
        "status": "pending_canonical_outcome",
        "state": new_state,
        "closure_kind": if new_state == "TASK_STATE_INPUT_REQUIRED" {
            Value::String("partial".to_string())
        } else {
            Value::Null
        },
        "eval_ledger_id": eval_memory_id,
        "reviewed": reviewed,
        "dispatch_outcome": dispatch_outcome,
    });
    if status_object.get("completion_recovery") == Some(&recovery)
        && !status_object.contains_key("resolved_completion")
    {
        return Ok(());
    }
    status_object.remove("resolved_completion");
    status_object.insert("completion_recovery".to_string(), recovery);
    crate::managed_run_control::advance_status_revision(status_object)?;
    let body = serde_json::to_string_pretty(&status).map_err(|error| {
        format!(
            "cannot serialize completion recovery receipt for dispatch_id={dispatch_id}: {error}"
        )
    })?;
    crate::utils::write_owner_only_file_atomic(&status_path, body.as_bytes()).map_err(|error| {
        format!(
            "cannot persist completion recovery receipt for dispatch_id={dispatch_id}: \
             write {}: {error}",
            status_path.display()
        )
    })
}

pub(crate) async fn handle_tachi_complete(
    server: &MemoryServer,
    mut params: TachiCompleteParams,
    // #1041 B7: whether `params.project` reflects the ORIGINAL caller's own
    // explicit placement decision, as opposed to a value the daemon's
    // session-binding default-injection put there. `TachiCompleteParams`
    // itself carries no wire marker for this (unlike `SaveMemoryParams`) —
    // widening it would touch ~20 direct struct-literal test fixtures for a
    // signal only ONE of its two callers actually needs to get right, so
    // each caller resolves it for its OWN entry point instead:
    //   - `dispatch_facade.rs`'s direct `tachi_complete` tool: `tachi_complete`
    //     is NOT in `session_identity::project_defaults_to_bound_project`'s
    //     list, so an omitted `project=` is NEVER auto-injected for it —
    //     `params.project.is_some()` is genuinely safe there.
    //   - `task_router.rs`'s `tachi_task(action='complete')` bridge: `project`
    //     CAN be a transport-injected default (`tachi_task` IS in that list)
    //     — it passes `TachiTaskParams::project_explicit`, the actual wire
    //     marker, instead.
    project_explicit: bool,
) -> Result<String, String> {
    // Dispatched completion reads its artifact contract. This is deliberately
    // the first executable action so an unsupported platform refuses before
    // eval/outcome/claim/receipt/kanban/continuity state can be mutated.
    ensure_completion_artifact_read_support(params.dispatch_id.as_deref())?;
    admit_managed_completion(server, params.dispatch_id.as_deref(), false)?;
    let resolved_flow_id = params
        .dispatch_id
        .as_deref()
        .filter(|dispatch_id| !dispatch_id.is_empty())
        .map(|dispatch_id| {
            super::flow_link::resolve_flow_id_for_dispatch(
                server,
                dispatch_id,
                params.flow_id.as_deref(),
            )
        })
        .transpose()?
        .flatten();

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

    // tachi#1200 item 1: mirror the issue_ref auto-inject above for
    // `profile`. Live policy replay (`route_simulate`/`recommend`) matches
    // eval rows on `EvalRow.profile` against a known dispatch profile name —
    // a row with no profile is silently dropped from every replay
    // computation, not merely degraded (see `tachi_dispatch::routing`). The
    // dispatch's own kanban card already has `profile` on file from launch
    // (`init_kanban_task`), so auto-inject it here the same
    // explicit-value-wins-then-lookup shape, covering BOTH callers of this
    // function (the direct `tachi_complete` tool never resolves this at all
    // today; the `tachi_task(action='complete')` bridge only resolves it from
    // a filesystem run/flow artifact that isn't always on file). Fail-safe:
    // a manual complete with no dispatch_id has nothing to look up and must
    // NEVER have a profile fabricated for it.
    if params.profile.as_deref().is_none_or(str::is_empty) {
        if let Some(dispatch_id) = params
            .dispatch_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        {
            params.profile =
                super::flow_link::resolve_profile_for_dispatch(server, dispatch_id, None);
        }
    }

    // #1066 AC-5: project ADJUDICATED, evidence-usable, non-self-eval mirror
    // eval intake rows into the legacy subagents[]-compatible aggregation
    // surface. Additive only — an empty/omitted eval_run_ids leaves
    // params.subagents byte-identical to what the caller supplied, and an
    // unresolved/ineligible id is silently skipped (never fails completion).
    // A genuine storage-read error is a DIFFERENT event from "not found"
    // (codex round-2 finding #4a): it is `tracing::error!`-logged inside
    // `project_eval_run_ids` AND disclosed on the completion record's notes
    // below, instead of being indistinguishable from a benign missing
    // reference.
    if !params.eval_run_ids.is_empty() {
        let (projected, lookup_error_ids) =
            super::mirror_eval_projection::project_eval_run_ids(server, &params.eval_run_ids);
        params.subagents.extend(projected);
        if !lookup_error_ids.is_empty() {
            let caveat = format!(
                "mirror eval projection: {} eval_run_id(s) skipped due to a storage lookup \
                 error (not simply unregistered): {}",
                lookup_error_ids.len(),
                lookup_error_ids.join(", ")
            );
            params.notes = Some(match params.notes.take() {
                Some(existing) if !existing.trim().is_empty() => {
                    format!("{existing}\n{caveat}")
                }
                _ => caveat,
            });
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
        safe_subagents,
        safe_feedback_rules,
        outcome_norm,
        diff_present,
        verification_present,
        secret_redactions,
        mem_params,
    } = build_complete_eval_record(&params, &date, &ts, project_explicit);

    // Writes never auto-select a named project from machine state (workspace
    // detection / "single project on disk"): that silently reroutes eval rows
    // away from the server's own stores. Callers must pass `project` (or the
    // server must have a bound project DB) for project-scoped persistence.
    // Recheck and persist immediately before the first side effect. The
    // earlier read-only check keeps invalid inputs cheap; this fence prevents
    // cancellation from winning between validation and eval/outcome writes.
    admit_managed_completion(server, params.dispatch_id.as_deref(), true)?;
    let save_result = match save_eval_memory(server, mem_params).await {
        Ok(result) => result,
        Err(error) => {
            let _ = revoke_managed_completion_admission(server, params.dispatch_id.as_deref());
            return Err(error);
        }
    };
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
    let completion_dispatch_id = params
        .dispatch_id
        .as_deref()
        .filter(|id| !id.trim().is_empty());
    let completion_verdict = if let Some(did) = completion_dispatch_id {
        let (declared, verdict) = crate::dispatch_ops::evaluate_completion_predicate_for_dispatch(
            &server.tachi_home_dir(),
            did,
            "",
        )?;
        let (new_state, reviewed_flag, override_reason) =
            crate::dispatch_ops::resolve_completion_state(params.outcome.as_str(), &verdict);
        Some(CompletionVerdict {
            declared,
            verdict_tag: verdict.tag(),
            new_state,
            reviewed_flag,
            override_reason,
        })
    } else {
        None
    };

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

    let dispatch_outcome_recorded = dispatch_outcome_status
        .get("recorded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let (Some(dispatch_id), Some(verdict)) =
        (completion_dispatch_id, completion_verdict.as_ref())
    {
        if !dispatch_outcome_recorded {
            // The retry in MemoryStore has already exhausted its bounded,
            // database-only policy. Do not run any derived side effects here:
            // the supplied delivery evidence remains durable, while this
            // local recovery may only reconcile the canonical outcome and its
            // receipt on a later complete call.
            let recovery_receipt = match persist_pending_completion_recovery_receipt(
                server,
                dispatch_id,
                verdict.new_state,
                &eval_memory_id,
                verdict.reviewed_flag,
                &dispatch_outcome_status,
            ) {
                Ok(()) => json!({
                    "status": "pending_canonical_outcome",
                    "dispatch_id": dispatch_id,
                    "state": verdict.new_state,
                    "eval_memory_id": eval_memory_id,
                    "reviewed": verdict.reviewed_flag,
                }),
                Err(error) => json!({
                    "status": "recovery_receipt_failed",
                    "dispatch_id": dispatch_id,
                    "error": error,
                }),
            };
            let pipeline_status = json!({
                "dispatch_outcome": dispatch_outcome_status,
                "completion_receipt": recovery_receipt,
                "adjudication": "skipped (canonical outcome pending)",
                "kanban_update": "skipped (canonical outcome pending)",
                "continuity_events": "skipped (canonical outcome pending)",
                "pattern_feedback": "skipped (canonical outcome pending)",
                "distill_trajectory": "skipped (canonical outcome pending)",
                "skill_evolve": "skipped (canonical outcome pending)",
                "post_complete_hooks": "skipped (canonical outcome pending)",
            });
            let response = shape_complete_response(
                json!({
                    "recorded": false,
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
                    "next_steps": ["Canonical dispatch outcome is pending local SQLite recovery; do not replay GitHub delivery."],
                    "pipeline": pipeline_status,
                    "secret_redactions": secret_redactions,
                }),
                params.format.as_deref(),
            );
            return serde_json::to_string(&response)
                .map_err(|error| format!("Failed to serialize recovery bundle: {error}"));
        }
    }

    // #1035: when the leader supplies a terminal adjudication, record it
    // linked to the outcome row just written. Fail-safe — never fails the
    // enclosing complete; errors surface in the pipeline JSON.
    let adjudication_status = super::dispatch_outcome::record_complete_adjudication(
        server,
        &params,
        &dispatch_outcome_status,
    );

    let mut pipeline_status = serde_json::json!({
        "dispatch_outcome": dispatch_outcome_status,
        "adjudication": adjudication_status,
        "kanban_update": "skipped (no dispatch_id)",
        "distill_trajectory": "skipped (automatic distillation retired)",
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
        "dispatch_outcome_id": dispatch_outcome_status.get("outcome_id").cloned().unwrap_or(Value::Null),
        "flow_id": params.flow_id.clone(),
        "issue_ref": params.issue_ref.clone(),
        "pr_ref": params.pr_ref.clone(),
        "feedback_rules_applied": safe_feedback_rules.clone(),
    });
    let subagent_event_payloads = safe_subagents.as_array().cloned().unwrap_or_default();
    pipeline_status["continuity_events"] = crate::continuity_ops::emit_task_completion_events(
        server,
        task_event_payload.clone(),
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
    } else if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expected_issue_ref = params
            .issue_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let verified_flow_revision = params
            .dispatch_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|dispatch_id| {
                crate::task_lifecycle::verified_flow_revision(
                    flow_id,
                    expected_issue_ref,
                    Some(dispatch_id),
                )
            })
            .unwrap_or(Ok(None));
        let mut completion_revision_payload = task_event_payload.clone();
        if let Some(object) = completion_revision_payload.as_object_mut() {
            // The eval row uses a wall-clock fallback when callers omit task_id.
            // That row is a derived receipt, not the stable completion source:
            // exact replay must retain the caller's None rather than hash the
            // newly generated task/eval identity and collide with itself.
            object.insert(
                "task_id".to_string(),
                params
                    .task_id
                    .as_ref()
                    .map(|value| Value::String(value.clone()))
                    .unwrap_or(Value::Null),
            );
            object.remove("eval_memory_id");
            object.remove("eval_path");
        }
        let source_revision =
            crate::tool_params::canonical_json_sha256(&completion_revision_payload);
        let evidence_digest = crate::tool_params::canonical_json_sha256(&json!({
            "pattern_refs": &pattern_feedback_refs,
            "default_outcome": default_pattern_outcome,
        }));
        match (verified_flow_revision, source_revision, evidence_digest) {
            (Ok(Some(flow_revision)), Ok(completion_revision), Ok(evidence_digest)) => {
                let source_revision = format!(
                    "flow:{};completion:sha256:{completion_revision}",
                    flow_revision
                );
                let evidence_digest = format!("sha256:{evidence_digest}");
                crate::continuity_ops::append_pattern_evidence_for_refs(
                    server,
                    crate::continuity_ops::PatternEvidenceBatchInput {
                        project: params.project.as_deref(),
                        refs: &pattern_feedback_refs,
                        default_outcome: default_pattern_outcome,
                        source: crate::continuity_ops::PatternEvidenceSource::TaskCompletion,
                        run_id: flow_id,
                        source_revision: &source_revision,
                        evidence_digest: &evidence_digest,
                    },
                )
            }
            (Ok(None), _, _) => json!({
                "status": "skipped",
                "reason": "unverified_flow_identity",
                "saved_count": 0,
                "error_count": 0,
                "events": [],
                "errors": [],
            }),
            (Err(error), _, _) => json!({
                "status": "failed",
                "reason": "flow_identity_read_failed",
                "error": error,
                "saved_count": 0,
            }),
            (_, Err(error), _) | (_, _, Err(error)) => json!({
                "status": "failed",
                "reason": "completion_evidence_digest_failed",
                "error": error,
                "saved_count": 0,
            }),
        }
    } else {
        json!({
            "status": "skipped",
            "reason": "missing_real_flow_id",
            "saved_count": 0,
            "error_count": 0,
            "events": [],
            "errors": [],
        })
    };

    // --- Kanban Hook: auto-update task board ---
    if let Some(ref did) = params.dispatch_id {
        // #1001 round 2 item 1: release the presence claim this dispatch
        // registered (auto_register_or_heartbeat_claim keys it on
        // dispatch_id). Fires unconditionally for a completed dispatch,
        // ahead of the predicate/kanban logic below, so a claim never stays
        // `active` because a later step in this function returned early or
        // erred. Fail-safe — degrades to a warn, never fails completion.
        crate::claims_ops::release_claim_for_dispatch(server, did, "complete");

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
        } = completion_verdict.expect("completion_verdict is Some when dispatch_id is present");

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

        // This receipt is authoritative for terminal accounting. It must be
        // durable before kanban is touched; a failed write is loud because
        // continuing would allow the watchdog to manufacture COMPLETED from
        // an exit-zero process after a missing/stale kanban projection.
        persist_resolved_completion_receipt(
            server,
            did,
            new_state,
            &eval_memory_id,
            reviewed_flag,
        )?;
        pipeline_status["completion_receipt"] = json!({
            "status": "persisted",
            "dispatch_id": did,
            "state": new_state,
            "closure_kind": if new_state == "TASK_STATE_INPUT_REQUIRED" {
                Value::String("partial".to_string())
            } else {
                Value::Null
            },
            "eval_memory_id": eval_memory_id,
            "reviewed": reviewed_flag,
        });

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
        match (resolved_flow_id.as_deref(), dispatch_id) {
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
        crate::precedent_ops::record_complete_rulings(server, &params, project_explicit).await;

    // Principle-level precedent CANDIDATE decomposition (#1076). Additive and
    // independent of `precedent_recording` above — both read the same
    // caller-supplied `rulings[]`, neither depends on the other's outcome.
    // Candidates only: no pending->established promotion happens here (that
    // is #1077's gate).
    pipeline_status["precedent_candidate_decomposition"] =
        crate::precedent_candidate_ops::record_complete_precedent_candidates(
            server,
            &params,
            project_explicit,
        )
        .await;

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
        project_explicit,
    )
    .await;

    let mut next_steps =
        vec!["Use tachi_search with 'eval' keyword to find related outcomes.".to_string()];
    if params
        .worktree
        .as_deref()
        .is_some_and(|worktree| !worktree.trim().is_empty())
    {
        next_steps.push(
            "A worktree was recorded; use the repository's current ship path when ready."
                .to_string(),
        );
    } else {
        next_steps.push(
            "No worktree was recorded; no local worktree ship step is implied by this completion."
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    fn managed_completion_params(dispatch_id: &str) -> TachiCompleteParams {
        TachiCompleteParams {
            task_id: None,
            task: "complete managed dispatch".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: None,
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: Some(dispatch_id.to_string()),
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: None,
            scope: None,
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }
    }

    fn seed_managed_working_status(
        server: &MemoryServer,
        dispatch_id: &str,
        cancellation: Option<Value>,
    ) {
        let run_dir = server.tachi_home_dir().join("runs").join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("managed run directory");
        let mut status = json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 7,
            "execution_classification": "managed_custom",
        });
        if let Some(cancellation) = cancellation {
            status["cancellation"] = cancellation;
        }
        std::fs::write(run_dir.join("status.json"), status.to_string()).expect("managed status");
    }

    fn dispatch_outcome_count(server: &MemoryServer, dispatch_id: &str) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                        [dispatch_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("query dispatch outcome count")
    }

    fn dispatch_adjudication_count(server: &MemoryServer, dispatch_id: &str) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM dispatch_adjudications AS adjudication \
                         JOIN dispatch_outcomes AS outcome ON outcome.outcome_id = adjudication.outcome_id \
                         WHERE outcome.dispatch_id = ?1",
                        [dispatch_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("query dispatch adjudication count")
    }

    #[tokio::test]
    async fn managed_cancellation_rejects_real_completion_before_outcome_or_receipt_writes() {
        for receipt in ["cancellation_requested", "cancellation_confirmed"] {
            let (server, _home) = crate::tests::make_server_with_temp_home();
            let dispatch_id = format!(
                "20260823T18251{}Z-custom-deadbeef",
                if receipt.ends_with("requested") { 1 } else { 2 }
            );
            seed_managed_working_status(&server, &dispatch_id, Some(json!({ "receipt": receipt })));

            let error =
                handle_tachi_complete(&server, managed_completion_params(&dispatch_id), false)
                    .await
                    .expect_err("managed cancellation owns completion before durable writes");
            assert_eq!(error, "managed cancellation owns terminal completion");
            assert_eq!(
                dispatch_outcome_count(&server, &dispatch_id),
                0,
                "{receipt} must reject before dispatch_outcomes/adjudication can be derived"
            );
            assert_eq!(
                dispatch_adjudication_count(&server, &dispatch_id),
                0,
                "{receipt} must reject before dispatch_adjudications can be derived"
            );
            let status: Value = serde_json::from_slice(
                &std::fs::read(
                    server
                        .tachi_home_dir()
                        .join("runs")
                        .join(&dispatch_id)
                        .join("status.json"),
                )
                .expect("status remains readable"),
            )
            .expect("status JSON");
            assert!(
                status.get("resolved_completion").is_none(),
                "{receipt} must not resolve completion"
            );
            assert!(
                status.get("completion_recovery").is_none(),
                "{receipt} must not admit completion"
            );
        }
    }

    #[tokio::test]
    async fn committed_completion_admission_makes_cancel_unavailable_without_signaling_and_can_finalize(
    ) {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        let dispatch_id = "20260823T182513Z-custom-deadbeef";
        seed_managed_working_status(&server, dispatch_id, None);
        let (mut receiver, _run_guard) = server
            .managed_run_controls
            .register(dispatch_id)
            .expect("managed cancellation registry");

        admit_managed_completion(&server, Some(dispatch_id), true)
            .expect("commit admission marker");
        let cancellation: Value = serde_json::from_str(
            &crate::managed_run_control::request_managed_custom_cancel(&server, dispatch_id, 8)
                .await
                .expect("cancel after completion admission"),
        )
        .expect("cancellation JSON");
        assert_eq!(cancellation["receipt"], "cancellation_unavailable");
        assert_eq!(cancellation["reason"], "terminal_or_recovery_state");
        assert!(
            receiver.try_recv().is_err(),
            "admitted completion must not signal the child"
        );

        handle_tachi_complete(&server, managed_completion_params(dispatch_id), false)
            .await
            .expect("admitted completion can finalize");
        let status: Value = serde_json::from_slice(
            &std::fs::read(
                server
                    .tachi_home_dir()
                    .join("runs")
                    .join(dispatch_id)
                    .join("status.json"),
            )
            .expect("final status"),
        )
        .expect("final status JSON");
        assert_eq!(
            status["resolved_completion"]["state"],
            "TASK_STATE_COMPLETED"
        );
    }

    #[test]
    fn completion_without_dispatch_id_does_not_require_descriptor_platform_support() {
        ensure_completion_artifact_read_support(None).expect("manual completion has no run read");
        ensure_completion_artifact_read_support(Some("   "))
            .expect("blank dispatch id has no run read");
    }

    #[cfg(not(unix))]
    #[test]
    fn dispatched_completion_refuses_before_handler_mutations_on_unsupported_platform() {
        let error = ensure_completion_artifact_read_support(Some("dispatch-123"))
            .expect_err("dispatched completion must require descriptor reads");
        assert!(error.contains("unavailable on this platform"), "{error}");
    }

    /// The receipt is the only source the watchdog can trust when the kanban
    /// projection disappears. A write failure therefore has to escape as an
    /// error; converting it to the handler's best-effort kanban warning would
    /// silently turn a partial + exit 0 into COMPLETED.
    #[test]
    fn resolved_completion_receipt_write_failure_is_loud() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temporary receipt parent");
        let non_directory = temp.path().join("not-a-run-directory");
        std::fs::write(&non_directory, "not a directory").expect("seed blocking file");

        let error = persist_resolved_completion_receipt_at(
            &non_directory,
            "20260719T000001Z-receipt-write-failure",
            "TASK_STATE_INPUT_REQUIRED",
            "eval-receipt-write-failure",
            true,
        )
        .expect_err("receipt write failure must abort completion");

        assert!(error.contains("cannot persist resolved completion receipt"));
        assert!(error.contains("not-a-run-directory/status.json"), "{error}");
    }

    #[test]
    fn resolved_completion_receipt_requires_a_preexisting_confined_run_dir() {
        let temp = tempfile::tempdir().expect("temporary tachi home");
        let dispatch_id = "20260719T000002Z-receipt-existing-run";

        let error = resolved_completion_run_dir(temp.path(), dispatch_id)
            .expect_err("unknown dispatch ids must not create a new receipt directory");
        assert!(
            error.contains("validated run directory is unavailable"),
            "{error}"
        );
        assert!(
            !temp.path().join("runs").join(dispatch_id).exists(),
            "receipt lookup must not create a run directory for an unknown dispatch"
        );

        let run_dir = temp.path().join("runs").join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("seed trusted run directory");
        assert_eq!(
            resolved_completion_run_dir(temp.path(), dispatch_id)
                .expect("pre-existing confined run directory is accepted"),
            run_dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn pending_completion_recovery_receipt_is_idempotent_and_not_terminal() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temporary receipt parent");
        let dispatch_id = "20260719T000003Z-recovery-receipt";
        let run_dir = temp.path().join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run directory");
        let status_path = run_dir.join("status.json");
        std::fs::write(
            &status_path,
            json!({
                "dispatch_id": dispatch_id,
                "resolved_completion": {"state": "TASK_STATE_COMPLETED"}
            })
            .to_string(),
        )
        .expect("seed stale terminal receipt");
        let outcome = json!({
            "recorded": false,
            "error": "dispatch outcome persistence failed after retry_memory_locked(op=dispatch_outcomes_upsert, db_label=global): database is locked"
        });

        persist_pending_completion_recovery_receipt_at(
            &run_dir,
            dispatch_id,
            "TASK_STATE_COMPLETED",
            "eval-recovery",
            false,
            &outcome,
        )
        .expect("persist pending recovery receipt");
        let first = std::fs::read_to_string(&status_path).expect("read first recovery receipt");
        let first_json: Value = serde_json::from_str(&first).expect("parse first recovery receipt");
        assert!(first_json.get("resolved_completion").is_none());
        assert_eq!(
            first_json["completion_recovery"]["status"],
            json!("pending_canonical_outcome")
        );
        assert_eq!(
            first_json["completion_recovery"]["dispatch_outcome"]["recorded"],
            json!(false)
        );
        assert_eq!(
            first_json["status_revision"],
            json!(1),
            "the recovery writer advances the shared revision"
        );

        persist_pending_completion_recovery_receipt_at(
            &run_dir,
            dispatch_id,
            "TASK_STATE_COMPLETED",
            "eval-recovery",
            false,
            &outcome,
        )
        .expect("repeat pending recovery receipt");
        assert_eq!(
            std::fs::read_to_string(&status_path).expect("read repeated recovery receipt"),
            first,
            "repeated lock exhaustion must preserve the same explicit recovery receipt"
        );

        persist_resolved_completion_receipt_at(
            &run_dir,
            dispatch_id,
            "TASK_STATE_COMPLETED",
            "eval-recovery",
            false,
        )
        .expect("persist terminal receipt after canonical outcome recovery");
        let resolved: Value = serde_json::from_str(
            &std::fs::read_to_string(&status_path).expect("read resolved receipt"),
        )
        .expect("parse resolved receipt");
        assert!(resolved.get("completion_recovery").is_none());
        assert_eq!(
            resolved["resolved_completion"]["eval_ledger_id"],
            json!("eval-recovery")
        );
        assert_eq!(
            resolved["status_revision"],
            json!(2),
            "the resolved-completion writer advances after the recovery writer"
        );
    }

    /// Completion receipts pause after their stale read while holding the same
    /// canonical per-run mutex as lifecycle and route writers. The lifecycle
    /// writer uses an existing `run/../run` spelling: a raw-path registry or a
    /// removed completion lock lets it finish before the stale replacement and
    /// loses terminal result, identity, or route evidence. Pending canonical
    /// outcomes deliberately retain their nonterminal status barrier.
    #[test]
    fn completion_receipt_writers_cannot_lose_route_or_project_fields() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let resolved_temp = tempfile::tempdir().expect("temporary resolved receipt parent");
        let resolved_dispatch_id = "20260822T000004Z-resolved-overlap";
        let resolved_dir = resolved_temp.path().join(resolved_dispatch_id);
        std::fs::create_dir_all(&resolved_dir).expect("create resolved run directory");
        let resolved_alias = resolved_dir.join("..").join(resolved_dispatch_id);
        let resolved_lock = crate::dispatch_ops::status_json_lock_for(&resolved_dir);
        let resolved_alias_lock = crate::dispatch_ops::status_json_lock_for(&resolved_alias);
        assert!(
            Arc::ptr_eq(&resolved_lock, &resolved_alias_lock),
            "existing lexical aliases must resolve to one status receipt lock"
        );
        std::fs::write(
            resolved_dir.join("status.json"),
            json!({
                "dispatch_id": resolved_dispatch_id,
                "project": "completion-overlap",
                "completion_recovery": {"status": "pending_canonical_outcome"}
            })
            .to_string(),
        )
        .expect("seed resolved receipt");

        let read = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let _barrier =
            install_completion_status_read_barrier(Arc::clone(&read), Arc::clone(&resume));
        let writer_dir = resolved_dir.clone();
        let resolved_writer = std::thread::spawn(move || {
            persist_resolved_completion_receipt_at(
                &writer_dir,
                resolved_dispatch_id,
                "TASK_STATE_COMPLETED",
                "eval-resolved-overlap",
                true,
            )
        });
        read.wait();
        let terminal_dir = resolved_alias;
        let (terminal_done, terminal_result) = std::sync::mpsc::sync_channel(1);
        let terminal_writer = std::thread::spawn(move || {
            crate::dispatch_ops::write_status_json(
                &terminal_dir,
                resolved_dispatch_id,
                false,
                None,
                None,
                "n/a",
                Some(0),
                None,
                Some(1),
                Some(1),
                Some(json!({
                    "state": "TASK_STATE_COMPLETED",
                    "result_written": true,
                    "result": "resolved terminal result",
                    "identity_receipt": {"model": "resolved-terminal"}
                })),
            );
            terminal_done
                .send(crate::dispatch_ops::stamp_route_decision_id(
                    &terminal_dir,
                    "route-resolved-overlap",
                ))
                .expect("report resolved terminal route stamp");
        });
        assert!(
            terminal_result
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "terminal lifecycle plus route evidence must wait behind the resolved completion lock"
        );
        resume.wait();
        resolved_writer
            .join()
            .expect("join resolved receipt writer")
            .expect("persist resolved receipt");
        terminal_writer
            .join()
            .expect("join resolved terminal writer");
        terminal_result
            .recv_timeout(Duration::from_secs(1))
            .expect("resolved terminal result after receipt release")
            .expect("stamp resolved terminal route evidence");
        let resolved: Value = serde_json::from_slice(
            &std::fs::read(resolved_dir.join("status.json")).expect("read resolved overlap"),
        )
        .expect("parse resolved overlap");
        assert_eq!(resolved["project"], "completion-overlap");
        assert_eq!(resolved["state"], "TASK_STATE_COMPLETED");
        assert_eq!(resolved["result_written"], true);
        assert_eq!(resolved["result"], "resolved terminal result");
        assert_eq!(resolved["identity_receipt"]["model"], "resolved-terminal");
        assert_eq!(resolved["route_decision_id"], "route-resolved-overlap");
        assert_eq!(
            resolved["resolved_completion"]["eval_ledger_id"],
            "eval-resolved-overlap"
        );
        assert!(resolved.get("completion_recovery").is_none());

        let recovery_temp = tempfile::tempdir().expect("temporary recovery receipt parent");
        let recovery_dispatch_id = "20260822T000005Z-recovery-overlap";
        let recovery_dir = recovery_temp.path().join(recovery_dispatch_id);
        std::fs::create_dir_all(&recovery_dir).expect("create recovery run directory");
        let recovery_alias = recovery_dir.join("..").join(recovery_dispatch_id);
        let recovery_lock = crate::dispatch_ops::status_json_lock_for(&recovery_dir);
        let recovery_alias_lock = crate::dispatch_ops::status_json_lock_for(&recovery_alias);
        assert!(
            Arc::ptr_eq(&recovery_lock, &recovery_alias_lock),
            "existing lexical aliases must resolve to one status receipt lock"
        );
        std::fs::write(
            recovery_dir.join("status.json"),
            json!({
                "dispatch_id": recovery_dispatch_id,
                "project": "completion-overlap",
                "resolved_completion": {"state": "TASK_STATE_COMPLETED"}
            })
            .to_string(),
        )
        .expect("seed recovery receipt");

        let read = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let _barrier =
            install_completion_status_read_barrier(Arc::clone(&read), Arc::clone(&resume));
        let writer_dir = recovery_dir.clone();
        let recovery_writer = std::thread::spawn(move || {
            persist_pending_completion_recovery_receipt_at(
                &writer_dir,
                recovery_dispatch_id,
                "TASK_STATE_COMPLETED",
                "eval-recovery-overlap",
                false,
                &json!({"recorded": false}),
            )
        });
        read.wait();
        let terminal_dir = recovery_alias;
        let (terminal_done, terminal_result) = std::sync::mpsc::sync_channel(1);
        let terminal_writer = std::thread::spawn(move || {
            crate::dispatch_ops::write_status_json(
                &terminal_dir,
                recovery_dispatch_id,
                false,
                None,
                None,
                "n/a",
                Some(0),
                None,
                Some(1),
                Some(1),
                Some(json!({
                    "state": "TASK_STATE_FAILED",
                    "result_written": true,
                    "result": "recovery terminal result",
                    "identity_receipt": {"model": "recovery-terminal"}
                })),
            );
            terminal_done
                .send(crate::dispatch_ops::stamp_route_decision_id(
                    &terminal_dir,
                    "route-recovery-overlap",
                ))
                .expect("report recovery terminal route stamp");
        });
        assert!(
            terminal_result
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "terminal lifecycle plus route evidence must wait behind the recovery completion lock"
        );
        resume.wait();
        recovery_writer
            .join()
            .expect("join recovery receipt writer")
            .expect("persist recovery receipt");
        terminal_writer
            .join()
            .expect("join recovery terminal writer");
        terminal_result
            .recv_timeout(Duration::from_secs(1))
            .expect("recovery terminal result after receipt release")
            .expect("stamp recovery terminal route evidence");
        let recovery: Value = serde_json::from_slice(
            &std::fs::read(recovery_dir.join("status.json")).expect("read recovery overlap"),
        )
        .expect("parse recovery overlap");
        assert_eq!(recovery["project"], "completion-overlap");
        assert_eq!(recovery["state"], "TASK_STATE_WORKING");
        assert_eq!(recovery["result_written"], true);
        assert_eq!(recovery["result"], "recovery terminal result");
        assert_eq!(recovery["identity_receipt"]["model"], "recovery-terminal");
        assert_eq!(recovery["route_decision_id"], "route-recovery-overlap");
        assert_eq!(
            recovery["completion_recovery"]["eval_ledger_id"],
            "eval-recovery-overlap"
        );
        assert!(recovery.get("resolved_completion").is_none());

        let source = include_str!("handler.rs");
        let shared_lock_call = ["status_json_lock_for", "(run_dir)"].concat();
        assert_eq!(
            source.matches(&shared_lock_call).count(),
            2,
            "every tachi_complete status read-modify-replace must take the shared per-run lock"
        );
    }
}
