//! Canonical dispatch outcome write (#773 v4 sol carve).
//!
//! `handle_tachi_complete` writes ONE canonical `dispatch_outcomes` row
//! FIRST — before the eval memory row, signature evidence, or precedent
//! capture — so that if any of those derived writes fails, the canonical
//! record of "this dispatch completed with this outcome" is never lost.
//! This module owns exactly that first write; everything downstream of it
//! in `handler.rs` is a best-effort derive that must never roll this row
//! back.
//!
//! Skipped (not an error) when the completion carries no `dispatch_id`: a
//! dispatch outcome without a dispatch to key it on has nothing to be
//! canonical about, and plenty of legitimate `tachi_complete` calls today
//! have no dispatch_id (solo-session work). Every other failure mode
//! (DB unavailable, write error) is surfaced in the returned status value,
//! never propagated as an `Err` — recording the canonical row is important,
//! but it must not be capable of failing the completion itself, matching
//! the fail-safe posture of `precedent_ops`/`signature_evidence` alongside
//! it in the same handler.

use serde_json::{json, Value};

use crate::tool_params::TachiCompleteParams;
use crate::MemoryServer;

/// Write the canonical outcome row for this completion. `eval_memory_id` is
/// the id of the eval memory row already persisted by
/// `build_complete_eval_record` + `save_eval_memory` (this fn runs AFTER
/// that save so the row can carry a real link, but BEFORE every other
/// derive in `handler.rs` — kanban update, signature recording, precedent
/// capture, lesson hooks).
///
/// Truthfulness (#773 Layer-2 ②): `reported_outcome` is the agent's raw
/// self-report, while `execution_outcome` is the MACHINE-RESOLVED value the
/// caller computed by running the #878-A completion predicate FIRST — so a
/// false `success` intercepted into `failed` is recorded as `failed` here,
/// with the original `success` preserved in `reported_outcome`. `error_class`
/// carries the interception reason class (e.g. `false_success`) when the
/// machine verdict diverges from the self-report, else `None`.
///
/// Returns a pipeline-status JSON value; never propagates a hard error to
/// the caller (fail-safe by design — see module docs).
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_complete_outcome(
    server: &MemoryServer,
    params: &TachiCompleteParams,
    eval_memory_id: &str,
    reported_outcome: &str,
    execution_outcome: &str,
    error_class: Option<&str>,
    verification_present: bool,
    diff_present: bool,
    evidence_refs: &[String],
) -> Value {
    let Some(dispatch_id) = params
        .dispatch_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return json!("skipped (no dispatch_id)");
    };

    let (role, vendor, model) = resolve_outcome_lane(params);
    let outcome_id = uuid::Uuid::new_v4().to_string();
    let new_outcome = memcore::NewDispatchOutcome {
        outcome_id,
        dispatch_id: dispatch_id.to_string(),
        eval_memory_id: Some(eval_memory_id.to_string()),
        model,
        vendor,
        role,
        // seat: no reliable source at the complete seam (the dispatch profile
        // carries backend/role/model, not a seat identity) — left None rather
        // than fabricated. Awaiting dispatch-context plumb, same as retry_count.
        seat: None,
        task_type: params
            .task_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        execution_outcome: execution_outcome.to_string(),
        reported_outcome: Some(reported_outcome.to_string()),
        // retry_count: awaiting dispatch-context plumb — no retry ledger at the
        // complete seam yet, so held at 0 (never invented).
        retry_count: 0,
        error_class: error_class.map(str::to_string),
        issue_ref: params
            .issue_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        pr_ref: params
            .pr_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        flow_id: params
            .flow_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        cost_tokens: params.cost_tokens,
        cost_usd: params.cost_usd,
        verification_present,
        diff_present,
        evidence_refs: json!(evidence_refs),
    };

    let (scope, _) = server.resolve_write_scope(params.scope.as_deref().unwrap_or(""));
    let write_result = if let Some(project) = params.project.as_deref().filter(|s| !s.is_empty()) {
        server.with_named_project_store(project, |store| {
            memcore::upsert_outcome(store.connection(), &new_outcome).map_err(|e| e.to_string())
        })
    } else {
        server.with_store_for_scope(scope, |store| {
            memcore::upsert_outcome(store.connection(), &new_outcome).map_err(|e| e.to_string())
        })
    };

    match write_result {
        Ok(row) => json!({
            "recorded": true,
            "outcome_id": row.outcome_id,
            "dispatch_id": row.dispatch_id,
            "idempotency_key": row.idempotency_key,
            "vendor": row.vendor,
            "scope": scope.as_str(),
        }),
        Err(error) => {
            tracing::warn!(
                error = %error,
                dispatch_id = %dispatch_id,
                "failed to persist canonical dispatch_outcomes row"
            );
            json!({
                "recorded": false,
                "dispatch_id": dispatch_id,
                "error": error,
            })
        }
    }
}

/// Resolve `(role, vendor, model)` for the outcome row using the same dispatch
/// profile lookup `signature_evidence::resolve_complete_lane` uses, so the
/// two writes never disagree about which lane a completion belongs to.
/// Unlike that function, an unresolved lane is not fatal here — the row is
/// still written (a canonical outcome record is more valuable with a
/// best-guess/absent lane than not written at all), it simply carries
/// `None`/`"unknown"`. `model` comes from the resolved profile when known
/// (previously hard-coded `None`, #773 ④).
fn resolve_outcome_lane(params: &TachiCompleteParams) -> (Option<String>, String, Option<String>) {
    let profile_def = params
        .profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .and_then(crate::dispatch_profile::resolve_dispatch_profile);
    let vendor = match profile_def {
        Some(p) => tachi_dispatch::normalize_vendor(p.backend, p.model),
        None => tachi_dispatch::normalize_vendor(&params.agent, None),
    };
    let role = profile_def.and_then(|p| tachi_dispatch::dispatch_role_class(p.role));
    let model = profile_def
        .and_then(|p| p.model)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());
    (role.map(str::to_string), vendor, model)
}

/// Record a canonical `dispatch_outcomes` row for a NON-`tachi_complete`
/// terminal path (#773 Layer-2 ② hole b): a dispatch that reaches a terminal
/// state without the agent ever calling `tachi_complete` — backend/preflight
/// prep failure, watchdog crash/timeout, or cancel. These previously wrote
/// NOTHING to the outcome ledger, so the router saw only self-reported
/// completions and none of the failures it most needs to learn from.
///
/// `reported_outcome` is NULL (there was no self-report); `execution_outcome`
/// is the machine terminal value (`"failed"` for the failure paths);
/// `error_class` names the path (`backend`/`preflight`/`watchdog`/`cancelled`
/// /`dispatch`). Reuses the single writer [`memcore::upsert_outcome`].
///
/// FIRST-WRITER-WINS: if a row already exists for this `dispatch_id`, this is a
/// no-op — so the path that first classified the failure (e.g. the backend
/// recorder tagging `backend`) is not clobbered by a later coarser catch-all
/// (the generic early-exit closer tagging `dispatch`). Fail-safe: any error is
/// logged and swallowed — this must never escalate on an already-failing path.
pub(crate) fn record_terminal_failure_outcome(
    server: &MemoryServer,
    dispatch_id: &str,
    error_class: &str,
    agent: Option<&str>,
) {
    let dispatch_id = dispatch_id.trim();
    if dispatch_id.is_empty() {
        return;
    }
    let vendor = agent
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|a| tachi_dispatch::normalize_vendor(a, None))
        .unwrap_or_else(|| "unknown".to_string());
    let new_outcome = memcore::NewDispatchOutcome {
        outcome_id: uuid::Uuid::new_v4().to_string(),
        dispatch_id: dispatch_id.to_string(),
        vendor,
        // No self-report on a non-complete terminal; machine says failed.
        execution_outcome: "failed".to_string(),
        reported_outcome: None,
        error_class: Some(error_class.to_string()),
        // Keep the evidence_refs array contract (Default would be JSON null).
        evidence_refs: json!([]),
        ..Default::default()
    };

    let (scope, _) = server.resolve_write_scope("");
    let write_result = server.with_store_for_scope(scope, |store| {
        let conn = store.connection();
        // First-writer-wins: skip if this dispatch already has an outcome row.
        if memcore::outcome_exists_for_dispatch(conn, dispatch_id).map_err(|e| e.to_string())? {
            return Ok(false);
        }
        memcore::upsert_outcome(conn, &new_outcome)
            .map(|_| true)
            .map_err(|e| e.to_string())
    });
    if let Err(error) = write_result {
        tracing::warn!(
            error = %error,
            dispatch_id = %dispatch_id,
            error_class = %error_class,
            "failed to persist terminal dispatch_outcomes row"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::TachiCompleteParams;

    fn base_params() -> TachiCompleteParams {
        TachiCompleteParams {
            task_id: None,
            task: "fix the thing".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: Some(500),
            cost_usd: Some(0.02),
            quality_score: None,
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: Some("dispatch-abc".to_string()),
            flow_id: Some("flow-1".to_string()),
            issue_ref: Some("kckylechen1/tachi#773".to_string()),
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: None,
            scope: Some("global".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
        }
    }

    fn test_server() -> (MemoryServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let server = MemoryServer::new(global_db, None).expect("server");
        (server, dir)
    }

    #[test]
    fn writes_canonical_row_with_expected_fields() {
        let (server, _dir) = test_server();
        let params = base_params();

        let status = record_complete_outcome(
            &server,
            &params,
            "eval-mem-1",
            "success",
            "completed",
            None,
            true,
            true,
            &["cargo test".to_string()],
        );

        assert_eq!(status["recorded"], json!(true));
        let outcome_id = status["outcome_id"].as_str().expect("outcome_id present");

        let row = server
            .with_global_store_read(|store| {
                memcore::get_outcome(store.connection(), outcome_id).map_err(|e| e.to_string())
            })
            .expect("read outcome")
            .expect("row present");

        assert_eq!(row.dispatch_id, "dispatch-abc");
        assert_eq!(row.eval_memory_id.as_deref(), Some("eval-mem-1"));
        assert_eq!(row.vendor, "codex");
        assert_eq!(row.task_type.as_deref(), Some("fix_request"));
        assert_eq!(row.execution_outcome, "completed");
        assert_eq!(row.reported_outcome.as_deref(), Some("success"));
        assert_eq!(row.issue_ref.as_deref(), Some("kckylechen1/tachi#773"));
        assert_eq!(row.flow_id.as_deref(), Some("flow-1"));
        assert_eq!(row.cost_tokens, Some(500));
        assert!(row.verification_present);
        assert!(row.diff_present);
    }

    #[test]
    fn skips_when_no_dispatch_id() {
        let (server, _dir) = test_server();
        let mut params = base_params();
        params.dispatch_id = None;

        let status = record_complete_outcome(
            &server,
            &params,
            "eval-mem-1",
            "success",
            "completed",
            None,
            true,
            true,
            &[],
        );

        assert_eq!(status, json!("skipped (no dispatch_id)"));
    }

    #[test]
    fn recompleting_same_dispatch_and_task_type_updates_not_duplicates() {
        let (server, _dir) = test_server();
        let params = base_params();

        let first = record_complete_outcome(
            &server,
            &params,
            "eval-mem-1",
            "success",
            "completed",
            None,
            true,
            true,
            &[],
        );
        let second = record_complete_outcome(
            &server,
            &params,
            "eval-mem-2",
            "success",
            "failed",
            Some("false_success"),
            false,
            false,
            &[],
        );

        assert_eq!(
            first["outcome_id"], second["outcome_id"],
            "same canonical row"
        );

        let outcome_id = second["outcome_id"].as_str().unwrap();
        let row = server
            .with_global_store_read(|store| {
                memcore::get_outcome(store.connection(), outcome_id).map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        // Replay updated the resolved machine verdict + interception class in
        // place while preserving the (unchanged) self-report; still ONE row.
        assert_eq!(row.execution_outcome, "failed");
        assert_eq!(row.reported_outcome.as_deref(), Some("success"));
        assert_eq!(row.error_class.as_deref(), Some("false_success"));
        assert_eq!(row.eval_memory_id.as_deref(), Some("eval-mem-2"));
    }

    #[test]
    fn records_terminal_failure_row_with_error_class_and_null_report() {
        let (server, _dir) = test_server();

        super::record_terminal_failure_outcome(&server, "dispatch-watchdog", "watchdog", Some("codex"));

        let rows = server
            .with_global_store_read(|store| {
                memcore::list_outcomes_by_vendor_window(
                    store.connection(),
                    "codex",
                    "1970-01-01T00:00:00Z",
                    None,
                )
                .map_err(|e| e.to_string())
            })
            .expect("read outcomes");
        let row = rows
            .iter()
            .find(|r| r.dispatch_id == "dispatch-watchdog")
            .expect("terminal row present");
        assert_eq!(row.execution_outcome, "failed");
        assert_eq!(row.reported_outcome, None, "no self-report on a terminal path");
        assert_eq!(row.error_class.as_deref(), Some("watchdog"));

        // First-writer-wins: a second, coarser classification does not clobber.
        super::record_terminal_failure_outcome(
            &server,
            "dispatch-watchdog",
            "dispatch",
            Some("codex"),
        );
        let rows_after = server
            .with_global_store_read(|store| {
                memcore::list_outcomes_by_vendor_window(
                    store.connection(),
                    "codex",
                    "1970-01-01T00:00:00Z",
                    None,
                )
                .map_err(|e| e.to_string())
            })
            .expect("read outcomes");
        let matching: Vec<_> = rows_after
            .iter()
            .filter(|r| r.dispatch_id == "dispatch-watchdog")
            .collect();
        assert_eq!(matching.len(), 1, "no duplicate terminal row");
        assert_eq!(
            matching[0].error_class.as_deref(),
            Some("watchdog"),
            "first classifier ('watchdog') is preserved, not clobbered by 'dispatch'"
        );
    }
}
