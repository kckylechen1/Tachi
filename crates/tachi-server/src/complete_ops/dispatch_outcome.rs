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

    // A corrupt receipt is explicitly unattributable — it must not fall back
    // to reconstructing identity from mutable profiles as if the dispatch
    // never had a receipt.
    let (receipt, receipt_corrupt) =
        match crate::dispatch_ops::load_dispatch_identity_receipt_checked(dispatch_id) {
            crate::dispatch_ops::DispatchReceiptLoad::Present(receipt) => (Some(*receipt), false),
            crate::dispatch_ops::DispatchReceiptLoad::Corrupt => (None, true),
            crate::dispatch_ops::DispatchReceiptLoad::Missing => (None, false),
        };
    // Evidence basis (#1065 D): a receipt's own attribution basis is
    // authoritative; a corrupt receipt is explicitly unattributable
    // ("unknown"); a missing receipt falls back to profile/agent
    // reconstruction, which is only ever `fallback_unreceipted` when that
    // fallback actually resolved a real vendor — otherwise it too is
    // `unknown`.
    let (role, vendor, model, seat, basis) = if receipt_corrupt {
        (
            None,
            "unknown".to_string(),
            None,
            None,
            tachi_dispatch::IdentityAttributionBasis::Unknown
                .as_str()
                .to_string(),
        )
    } else if let Some(receipt) = receipt.as_ref() {
        let (identity, basis) = receipt.attribution();
        let seat =
            (identity.seat != tachi_dispatch::UNKNOWN_IDENTITY).then(|| identity.seat.clone());
        (
            tachi_dispatch::dispatch_role_class(&identity.role).map(str::to_string),
            tachi_dispatch::normalize_vendor(&identity.backend, identity.model.as_deref()),
            identity.model.clone(),
            seat,
            basis.as_str().to_string(),
        )
    } else {
        let (role, vendor, model) = resolve_outcome_lane_from_profile(params);
        let basis = if vendor != "unknown" {
            tachi_dispatch::IdentityAttributionBasis::FallbackUnreceipted
        } else {
            tachi_dispatch::IdentityAttributionBasis::Unknown
        };
        (role, vendor, model, None, basis.as_str().to_string())
    };
    let identity_receipt = receipt.as_ref().map(|receipt| {
        serde_json::to_value(receipt).expect("dispatch identity receipt serializes")
    });
    let outcome_id = uuid::Uuid::new_v4().to_string();
    let new_outcome = memcore::NewDispatchOutcome {
        outcome_id,
        dispatch_id: dispatch_id.to_string(),
        eval_memory_id: Some(eval_memory_id.to_string()),
        model,
        vendor,
        role,
        seat,
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
        identity_receipt,
        identity_attribution_basis: basis,
    };

    let (scope, _) = server.resolve_write_scope(params.scope.as_deref().unwrap_or(""));
    // #774 idempotency-key parity: `record_terminal_failure_outcome` never
    // carries a `task_type`, so a plain `upsert_outcome` here (keyed on
    // `(dispatch_id, task_type)`) would derive a DIFFERENT key than a
    // terminal-placeholder row already written for this dispatch_id and
    // insert a sibling row instead of completing it — see
    // `memcore::upsert_outcome_reconciling_terminal_placeholder`'s docs for
    // the full mechanism.
    let write_result = if let Some(project) = params.project.as_deref().filter(|s| !s.is_empty()) {
        server.with_named_project_store(project, |store| {
            memcore::upsert_outcome_reconciling_terminal_placeholder(
                store.connection(),
                &new_outcome,
            )
            .map_err(|e| e.to_string())
        })
    } else {
        server.with_store_for_scope(scope, |store| {
            memcore::upsert_outcome_reconciling_terminal_placeholder(
                store.connection(),
                &new_outcome,
            )
            .map_err(|e| e.to_string())
        })
    };

    match write_result {
        Ok(row) => json!({
            "recorded": true,
            "outcome_id": row.outcome_id,
            "dispatch_id": row.dispatch_id,
            "idempotency_key": row.idempotency_key,
            "vendor": row.vendor,
            "identity_attribution_basis": row.identity_attribution_basis,
            "identity_receipt": row.identity_receipt,
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

/// Record the leader terminal adjudication for this dispatch outcome (#1035).
///
/// Called from `handle_tachi_complete` AFTER [`record_complete_outcome`], with
/// the outcome status JSON that function returned (the source of `outcome_id`).
/// When `params.adjudication` is `None` this is a skip (the vast majority of
/// `complete` calls carry no adjudication). When present, it writes an
/// append-only `dispatch_adjudications` row linked to the outcome by
/// `outcome_id`, plus one `dispatch_adjudication_signatures` row per resolved
/// signature id in `params.signatures`.
///
/// **Scope symmetry (#774):** mirrors [`record_complete_outcome`]'s two-branch
/// write target exactly — `params.project` (named project store) takes
/// priority, otherwise `resolve_write_scope`. Splitting the outcome and its
/// adjudication across two stores is the #774 historical accident.
///
/// **Fail-safe:** every failure (invalid params, unknown signature id, DB
/// error) is surfaced in the returned status JSON; this never fails the
/// enclosing `complete` call.
///
/// **Signature id gate (#1035 frozen spec):** every `params.signatures[i].id`
/// must resolve via `tachi_dispatch::resolve_signature_id`. An unknown id
/// rejects the ENTIRE adjudication loudly (the error message names it) —
/// never silently dropping it and writing a partial event.
pub(crate) fn record_complete_adjudication(
    server: &MemoryServer,
    params: &TachiCompleteParams,
    dispatch_outcome_status: &Value,
) -> Value {
    let Some(adjudication) = params.adjudication.as_ref() else {
        return json!("skipped (no adjudication)");
    };

    // The outcome row must have been recorded — without an outcome_id to
    // link to, there is nothing to adjudicate.
    let recorded = dispatch_outcome_status
        .get("recorded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !recorded {
        return json!({
            "recorded": false,
            "reason": "dispatch outcome was not recorded; adjudication skipped",
        });
    }
    let outcome_id = dispatch_outcome_status
        .get("outcome_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if outcome_id.is_empty() {
        return json!({
            "recorded": false,
            "reason": "outcome_id missing from dispatch_outcome status; adjudication skipped",
        });
    }

    // Validate exactly-one-of (verdict | not_required_reason).
    if let Err(error) = adjudication.validate_exactly_one() {
        return json!({
            "recorded": false,
            "outcome_id": outcome_id,
            "error": error,
        });
    }

    // Signature id gate: every id must resolve to a canonical taxonomy entry.
    // An unknown id rejects the ENTIRE adjudication — the append-only ledger
    // never accepts an event carrying an unresolvable signature (#1035).
    let mut signatures = Vec::with_capacity(params.signatures.len());
    for sig in &params.signatures {
        match tachi_dispatch::resolve_signature_id(&sig.signature) {
            Some(canonical) => signatures.push(memcore::DispatchAdjudicationSignature {
                signature_id: canonical.to_string(),
                evidence_ref: sig.evidence_ref.clone(),
                resolved: sig.resolved,
            }),
            None => {
                return json!({
                    "recorded": false,
                    "outcome_id": outcome_id,
                    "error": format!(
                        "unknown signature id '{}' in adjudication signatures; \
                         each id must resolve via the error-signature taxonomy",
                        sig.signature
                    ),
                });
            }
        }
    }

    let evidence_ref = adjudication
        .evidence_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&outcome_id)
        .to_string();
    // event_key is deterministic per outcome: a replay of the same complete
    // produces the same outcome_id, so the same event_key short-circuits
    // idempotently (the append primitive's get_by_event_key gate). A
    // different complete produces a new outcome_id → new event_key → a
    // correction row appended, preserving history.
    let event_key = format!("complete:{outcome_id}");
    let new_adjudication = memcore::NewDispatchAdjudication {
        adjudication_id: uuid::Uuid::new_v4().to_string(),
        outcome_id: outcome_id.clone(),
        event_key,
        verdict: adjudication.verdict.clone(),
        not_required_reason: adjudication.not_required_reason.clone(),
        actor: adjudication.adjudicator.clone(),
        evidence_ref,
        signatures,
    };

    let (scope, _) = server.resolve_write_scope(params.scope.as_deref().unwrap_or(""));
    let write_result = if let Some(project) = params.project.as_deref().filter(|s| !s.is_empty()) {
        server.with_named_project_store(project, |store| {
            memcore::append_dispatch_adjudication(store.connection(), &new_adjudication)
                .map_err(|e| e.to_string())
        })
    } else {
        server.with_store_for_scope(scope, |store| {
            memcore::append_dispatch_adjudication(store.connection(), &new_adjudication)
                .map_err(|e| e.to_string())
        })
    };

    match write_result {
        Ok(row) => json!({
            "recorded": true,
            "adjudication_id": row.adjudication_id,
            "outcome_id": row.outcome_id,
            "event_key": row.event_key,
            "verdict": row.verdict,
            "not_required_reason": row.not_required_reason,
            "signatures": row.signatures.len(),
            "scope": scope.as_str(),
        }),
        Err(error) => {
            tracing::warn!(
                error = %error,
                outcome_id = %outcome_id,
                "failed to persist dispatch adjudication row"
            );
            json!({
                "recorded": false,
                "outcome_id": outcome_id,
                "error": error,
            })
        }
    }
}

/// Resolve `(role, vendor, model)` for the outcome row from the completion's
/// own `profile`/`agent` fields — used only when no identity receipt exists
/// to attribute from (a receipt's own [`tachi_dispatch::DispatchIdentityReceipt::attribution`]
/// is authoritative and handled by the caller before falling back here).
/// Uses the same dispatch profile lookup `signature_evidence::
/// resolve_complete_lane` uses, so the two writes never disagree about which
/// lane a completion belongs to. Unlike that function, an unresolved lane is
/// not fatal here — the row is still written (a canonical outcome record is
/// more valuable with a best-guess/absent lane than not written at all), it
/// simply carries `None`/`"unknown"`. `model` comes from the resolved
/// profile when known (previously hard-coded `None`, #773 ④).
fn resolve_outcome_lane_from_profile(
    params: &TachiCompleteParams,
) -> (Option<String>, String, Option<String>) {
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
///
/// Scope symmetry (#774 round 2): [`record_complete_outcome`] resolves its
/// write target from the SAME dispatch's `project` field before falling back
/// to `resolve_write_scope("")` — a bare `resolve_write_scope("")` here
/// (ignoring `project` entirely) could land this row in a different DB than
/// a later/earlier `tachi_complete` for the same `dispatch_id`, splitting the
/// first-writer-wins invariant across two stores. `project` here is threaded
/// from the ORIGINAL dispatch's `TachiDispatchParams::project` at each call
/// site that still has that context (backend prep, harness preflight,
/// post-init early-exit). The daemon-restart orphan-recovery path
/// (`recover_orphaned_dispatch_runs`) has no live dispatch context either,
/// but (#774 round 3) reads `project` back off the same on-disk `status.json`
/// receipt the dispatch seeded it into at dispatch time, so it now threads a
/// real value through too when the receipt carries one; only run
/// directories written before that receipt field existed fall back to the
/// default `resolve_write_scope("")` branch.
pub(crate) fn record_terminal_failure_outcome(
    server: &MemoryServer,
    dispatch_id: &str,
    error_class: &str,
    agent: Option<&str>,
    project: Option<&str>,
) {
    let dispatch_id = dispatch_id.trim();
    if dispatch_id.is_empty() {
        return;
    }
    // Terminal failures attribute exactly like completions: the frozen
    // receipt's attribution identity, never the mutable profile and never the
    // planned route once a carrier acknowledged something else. A corrupt
    // receipt is explicitly unattributable — no agent-string fallback either.
    let (receipt, receipt_corrupt) =
        match crate::dispatch_ops::load_dispatch_identity_receipt_checked(dispatch_id) {
            crate::dispatch_ops::DispatchReceiptLoad::Present(receipt) => (Some(*receipt), false),
            crate::dispatch_ops::DispatchReceiptLoad::Corrupt => (None, true),
            crate::dispatch_ops::DispatchReceiptLoad::Missing => (None, false),
        };
    let attribution = receipt.as_ref().map(|receipt| receipt.attribution());
    let identity = attribution.as_ref().map(|(identity, _)| identity.clone());
    let vendor = identity
        .as_ref()
        .map(|identity| {
            tachi_dispatch::normalize_vendor(&identity.backend, identity.model.as_deref())
        })
        .or_else(|| {
            (!receipt_corrupt)
                .then(|| {
                    agent
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|a| tachi_dispatch::normalize_vendor(a, None))
                })
                .flatten()
        })
        .unwrap_or_else(|| "unknown".to_string());
    // Evidence basis (#1065 D): mirrors `record_complete_outcome`'s rule —
    // receipt basis is authoritative, corrupt is explicitly unknown, and a
    // profile/agent fallback is only `fallback_unreceipted` when it actually
    // resolved a real vendor.
    let basis = if receipt_corrupt {
        tachi_dispatch::IdentityAttributionBasis::Unknown
    } else if let Some((_, basis)) = attribution.as_ref() {
        *basis
    } else if vendor != "unknown" {
        tachi_dispatch::IdentityAttributionBasis::FallbackUnreceipted
    } else {
        tachi_dispatch::IdentityAttributionBasis::Unknown
    };
    let identity_receipt = receipt.as_ref().map(|receipt| {
        serde_json::to_value(receipt).expect("dispatch identity receipt serializes")
    });
    let new_outcome = memcore::NewDispatchOutcome {
        outcome_id: uuid::Uuid::new_v4().to_string(),
        dispatch_id: dispatch_id.to_string(),
        vendor,
        model: identity
            .as_ref()
            .and_then(|identity| identity.model.clone()),
        role: identity.as_ref().and_then(|identity| {
            tachi_dispatch::dispatch_role_class(&identity.role).map(str::to_string)
        }),
        seat: identity.as_ref().and_then(|identity| {
            (identity.seat != tachi_dispatch::UNKNOWN_IDENTITY).then(|| identity.seat.clone())
        }),
        // No self-report on a non-complete terminal; machine says failed.
        execution_outcome: "failed".to_string(),
        reported_outcome: None,
        error_class: Some(error_class.to_string()),
        // Keep the evidence_refs array contract (Default would be JSON null).
        evidence_refs: json!([]),
        identity_receipt,
        identity_attribution_basis: basis.as_str().to_string(),
        ..Default::default()
    };

    let write_fn = |conn: &rusqlite::Connection| {
        // First-writer-wins: skip if this dispatch already has an outcome row.
        if memcore::outcome_exists_for_dispatch(conn, dispatch_id).map_err(|e| e.to_string())? {
            return Ok(false);
        }
        memcore::upsert_outcome(conn, &new_outcome)
            .map(|_| true)
            .map_err(|e| e.to_string())
    };
    // Mirrors `record_complete_outcome`'s branching: a named project DB (when
    // the original dispatch carried one) takes priority over the scope
    // fallback, same as the complete path prioritizes `params.project` over
    // `params.scope`.
    let write_result = if let Some(project) = project.map(str::trim).filter(|s| !s.is_empty()) {
        server.with_named_project_store(project, |store| write_fn(store.connection()))
    } else {
        let (scope, _) = server.resolve_write_scope("");
        server.with_store_for_scope(scope, |store| write_fn(store.connection()))
    };
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
    use crate::tool_params::{AdjudicationParams, SignatureRecordParams, TachiCompleteParams};

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
            adjudication: None,
        }
    }

    fn test_server() -> (MemoryServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let server = MemoryServer::new(global_db, None).expect("server");
        (server, dir)
    }

    fn with_tachi_home<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tachi home");
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", home.path());
        f(home.path());
        if let Some(value) = saved {
            std::env::set_var("TACHI_HOME", value);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
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
    fn completion_persists_frozen_identity_receipt() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let receipt = serde_json::to_value(tachi_dispatch::recommendation_identity_receipt(
                tachi_dispatch::resolve_dispatch_profile("glm_impl").expect("glm profile"),
            ))
            .expect("receipt json");
            let run_dir = home.join("runs").join("dispatch-abc");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({ "identity_receipt": receipt }).to_string(),
            )
            .expect("status receipt");

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
                &[],
            );
            let outcome_id = status["outcome_id"].as_str().expect("outcome id");
            let persisted: String = server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT identity_receipt FROM dispatch_outcomes WHERE outcome_id = ?1",
                            [outcome_id],
                            |row| row.get(0),
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("frozen receipt column");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&persisted).expect("receipt json"),
                receipt
            );
        });
    }

    /// #1065 D: a receipt present but never carrier-acknowledged attributes to
    /// the planned identity with basis `planned_unconfirmed` — not
    /// `fallback_unreceipted` (that basis is reserved for a MISSING receipt).
    #[test]
    fn unconfirmed_receipt_completion_has_planned_unconfirmed_basis() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let profile =
                tachi_dispatch::resolve_dispatch_profile("glm_impl").expect("glm profile");
            let receipt = tachi_dispatch::recommendation_identity_receipt(profile);
            let run_dir = home.join("runs").join("dispatch-abc");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({
                    "identity_receipt": serde_json::to_value(&receipt).expect("receipt json")
                })
                .to_string(),
            )
            .expect("status receipt");

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
                &[],
            );
            assert_eq!(
                status["identity_attribution_basis"].as_str(),
                Some("planned_unconfirmed"),
                "a present-but-unacknowledged receipt attributes on the planned identity"
            );

            let outcome_id = status["outcome_id"].as_str().expect("outcome id");
            let row_basis: String = server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT identity_attribution_basis FROM dispatch_outcomes \
                             WHERE outcome_id = ?1",
                            [outcome_id],
                            |row| row.get(0),
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("persisted basis column");
            assert_eq!(row_basis, "planned_unconfirmed");
        });
    }

    #[test]
    fn completion_replay_preserves_the_first_receipt_byte_for_byte() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let profile =
                tachi_dispatch::resolve_dispatch_profile("glm_impl").expect("glm profile");
            let first =
                serde_json::to_value(tachi_dispatch::recommendation_identity_receipt(profile))
                    .expect("receipt json");
            let run_dir = home.join("runs").join("dispatch-abc");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({ "identity_receipt": first }).to_string(),
            )
            .expect("status receipt");

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
                &[],
            );
            let outcome_id = status["outcome_id"]
                .as_str()
                .expect("outcome id")
                .to_string();
            let row_after_first = |server: &crate::MemoryServer| -> (String, Option<String>) {
                server
                    .with_global_store_read(|store| {
                        store
                            .connection()
                            .query_row(
                                "SELECT identity_receipt, model FROM dispatch_outcomes \
                                 WHERE outcome_id = ?1",
                                [outcome_id.as_str()],
                                |row| Ok((row.get(0)?, row.get(1)?)),
                            )
                            .map_err(|error| error.to_string())
                    })
                    .expect("outcome row")
            };
            let (first_persisted, model_before) = row_after_first(&server);

            // A replay arrives with a DIFFERENT receipt on disk (e.g. a
            // rewritten status.json). The persisted receipt must stay the
            // first one, byte for byte, and the flat attribution columns must
            // stay frozen with it — the row must never disagree with the
            // receipt persisted beside it.
            let mut mutated =
                serde_json::from_value::<tachi_dispatch::DispatchIdentityReceipt>(first.clone())
                    .expect("receipt decodes");
            mutated.planned.model = Some("zhipuai-coding-plan/glm-99".to_string());
            let second = serde_json::to_value(&mutated).expect("mutated json");
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({ "identity_receipt": second }).to_string(),
            )
            .expect("mutated status receipt");
            record_complete_outcome(
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

            let (persisted, model_after) = row_after_first(&server);
            assert_eq!(
                persisted, first_persisted,
                "replay must keep the first receipt byte-for-byte"
            );
            assert_ne!(
                serde_json::from_str::<serde_json::Value>(&persisted).expect("receipt json"),
                second,
                "replay must not adopt a rewritten receipt"
            );
            assert_eq!(
                model_after, model_before,
                "replay must not rewrite flat identity columns"
            );
        });
    }

    #[test]
    fn terminal_failure_attributes_to_substituted_identity() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let profile =
                tachi_dispatch::resolve_dispatch_profile("glm_impl").expect("glm profile");
            let mut receipt = tachi_dispatch::recommendation_identity_receipt(profile);
            let planned_model = receipt.planned.model.clone().expect("planned model");
            let mut observed = receipt.planned.clone();
            observed.model = Some(format!("{planned_model}@2026-07-13"));
            receipt
                .acknowledge(
                    observed,
                    tachi_dispatch::DispatchAcknowledgement::Substituted,
                    "carrier pinned a release".to_string(),
                )
                .expect("same-lineage substitution");
            let run_dir = home.join("runs").join("dispatch-terminal");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({
                    "identity_receipt": serde_json::to_value(&receipt).expect("receipt json")
                })
                .to_string(),
            )
            .expect("status receipt");

            super::record_terminal_failure_outcome(
                &server,
                "dispatch-terminal",
                "watchdog",
                Some("codex"),
                None,
            );

            let (model, _vendor, basis): (Option<String>, String, String) = server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT model, vendor, identity_attribution_basis \
                             FROM dispatch_outcomes WHERE dispatch_id = 'dispatch-terminal'",
                            [],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("terminal outcome row");
            assert_eq!(
                model.as_deref(),
                Some(format!("{planned_model}@2026-07-13").as_str()),
                "terminal failure must attribute to the executed model, not the planned claim"
            );
            assert_eq!(
                basis, "observed",
                "a substituted acknowledgement's attribution basis is carrier-observed, \
                 even on the terminal-failure write path"
            );
        });
    }

    /// #1065 D BUG-3: `record_terminal_failure_outcome` must treat a corrupt
    /// receipt exactly like `record_complete_outcome` does — explicitly
    /// unattributable, never reconstructed from the `agent` fallback. Before
    /// this the terminal path's corrupt case was untested; a regression here
    /// would silently let a terminal failure's vendor/basis diverge from the
    /// completion path's verdict on the SAME corrupt receipt.
    #[test]
    fn terminal_failure_with_corrupt_receipt_is_unattributable() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let run_dir = home.join("runs").join("dispatch-terminal-corrupt");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            // Present but unparseable: wrong shape for the receipt contract.
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({ "identity_receipt": {"contract_id": 42} }).to_string(),
            )
            .expect("corrupt status receipt");

            super::record_terminal_failure_outcome(
                &server,
                "dispatch-terminal-corrupt",
                "watchdog",
                Some("codex"),
                None,
            );

            let (vendor, basis): (String, String) = server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT vendor, identity_attribution_basis FROM dispatch_outcomes \
                             WHERE dispatch_id = 'dispatch-terminal-corrupt'",
                            [],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("terminal outcome row");
            assert_eq!(
                vendor, "unknown",
                "a corrupt receipt must not fall back to the agent string, even \
                 on a terminal-failure path"
            );
            assert_eq!(
                basis, "unknown",
                "a corrupt receipt's attribution basis is explicitly unknown, \
                 never reconstructed"
            );
        });
    }

    #[test]
    fn corrupt_receipt_is_unattributable_not_profile_reconstructed() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let run_dir = home.join("runs").join("dispatch-abc");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            // Present but unparseable: wrong shape for the receipt contract.
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({ "identity_receipt": {"contract_id": 42} }).to_string(),
            )
            .expect("corrupt status receipt");

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
                &[],
            );
            assert_eq!(
                status["vendor"].as_str(),
                Some("unknown"),
                "a corrupt receipt must be explicitly unattributable, never \
                 reconstructed from the agent/profile"
            );
            assert!(
                status["identity_receipt"].is_null(),
                "a corrupt receipt is not persisted as if it were valid"
            );
            assert_eq!(
                status["identity_attribution_basis"].as_str(),
                Some("unknown"),
                "a corrupt receipt's attribution basis is explicitly unknown"
            );
        });
    }

    #[test]
    fn substituted_receipt_attributes_outcome_to_executed_not_planned() {
        with_tachi_home(|home| {
            let (server, _dir) = test_server();
            let profile =
                tachi_dispatch::resolve_dispatch_profile("glm_impl").expect("glm profile");
            let mut receipt = tachi_dispatch::recommendation_identity_receipt(profile);
            let planned_model = receipt.planned.model.clone().expect("planned model");
            let mut observed = receipt.planned.clone();
            observed.model = Some(format!("{planned_model}@2026-07-13"));
            receipt
                .acknowledge(
                    observed,
                    tachi_dispatch::DispatchAcknowledgement::Substituted,
                    "carrier pinned a release".to_string(),
                )
                .expect("same-lineage substitution");
            let run_dir = home.join("runs").join("dispatch-abc");
            std::fs::create_dir_all(&run_dir).expect("run dir");
            std::fs::write(
                run_dir.join("status.json"),
                serde_json::json!({
                    "identity_receipt": serde_json::to_value(&receipt).expect("receipt json")
                })
                .to_string(),
            )
            .expect("status receipt");

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
                &[],
            );
            let outcome_id = status["outcome_id"]
                .as_str()
                .expect("outcome id")
                .to_string();
            let (row_model, persisted, basis): (Option<String>, String, String) = server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT model, identity_receipt, identity_attribution_basis \
                             FROM dispatch_outcomes WHERE outcome_id = ?1",
                            [outcome_id.as_str()],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("outcome row");
            assert_eq!(
                row_model.as_deref(),
                Some(format!("{planned_model}@2026-07-13").as_str()),
                "the outcome row must attribute to the executed model, not the planned claim"
            );
            assert_eq!(
                basis, "observed",
                "a substituted acknowledgement's attribution basis is carrier-observed"
            );
            // Requested and effective identity stay simultaneously observable
            // in the persisted receipt.
            let persisted_receipt =
                serde_json::from_str::<tachi_dispatch::DispatchIdentityReceipt>(&persisted)
                    .expect("receipt decodes");
            assert_eq!(
                persisted_receipt.planned.model.as_deref(),
                Some(planned_model.as_str())
            );
            assert!(persisted_receipt.observed.mismatch);
        });
    }

    #[test]
    fn records_terminal_failure_row_with_error_class_and_null_report() {
        let (server, _dir) = test_server();

        super::record_terminal_failure_outcome(
            &server,
            "dispatch-watchdog",
            "watchdog",
            Some("codex"),
            None,
        );

        let rows = server
            .with_global_store_read(|store| {
                memcore::list_outcomes_by_vendor_window(
                    store.connection(),
                    "codex",
                    "1970-01-01T00:00:00Z",
                    None,
                    memcore::OutcomeEvidenceClass::AnyAttribution,
                )
                .map_err(|e| e.to_string())
            })
            .expect("read outcomes");
        let row = rows
            .iter()
            .find(|r| r.dispatch_id == "dispatch-watchdog")
            .expect("terminal row present");
        assert_eq!(row.execution_outcome, "failed");
        assert_eq!(
            row.reported_outcome, None,
            "no self-report on a terminal path"
        );
        assert_eq!(row.error_class.as_deref(), Some("watchdog"));
        assert_eq!(
            row.identity_attribution_basis, "fallback_unreceipted",
            "no receipt existed; vendor came from the agent fallback ('codex')"
        );

        // First-writer-wins: a second, coarser classification does not clobber.
        super::record_terminal_failure_outcome(
            &server,
            "dispatch-watchdog",
            "dispatch",
            Some("codex"),
            None,
        );
        let rows_after = server
            .with_global_store_read(|store| {
                memcore::list_outcomes_by_vendor_window(
                    store.connection(),
                    "codex",
                    "1970-01-01T00:00:00Z",
                    None,
                    memcore::OutcomeEvidenceClass::AnyAttribution,
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

    /// #774 round 2 discriminator: `record_terminal_failure_outcome` must
    /// resolve its write target the same way `record_complete_outcome` does
    /// for the SAME dispatch — via the dispatch's named `project`, not a
    /// blind `resolve_write_scope("")`. Before this fix the terminal path
    /// ignored `project` entirely, so a terminal-failure call and a
    /// `tachi_complete` call for the same dispatch_id/project could land in
    /// two DIFFERENT stores (this server's global DB vs. the named project
    /// DB), producing two outcome rows instead of one and defeating
    /// first-writer-wins. Runs both call orders against the SAME named
    /// project DB and asserts exactly one row lands there either way.
    fn with_named_project_env<F: FnOnce(&str)>(project_name: &str, f: F) {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tachi_home tmp");
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        std::env::set_var("TACHI_HOME", tmp.path());
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        let named_db = tmp
            .path()
            .join("projects")
            .join(project_name)
            .join("memory.db");
        std::fs::create_dir_all(named_db.parent().unwrap()).expect("named project dir");
        std::fs::write(&named_db, b"").expect("named project db placeholder");

        f(project_name);

        if let Some(v) = saved_home {
            std::env::set_var("TACHI_HOME", v);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
        if let Some(v) = saved_sigil {
            std::env::set_var("SIGIL_HOME", v);
        } else {
            std::env::remove_var("SIGIL_HOME");
        }
        if let Some(v) = saved_app {
            std::env::set_var("TACHI_APP_HOME", v);
        } else {
            std::env::remove_var("TACHI_APP_HOME");
        }
    }

    #[test]
    fn terminal_then_complete_same_project_lands_one_row() {
        with_named_project_env("hyperion", |project_name| {
            let (server, _dir) = test_server();

            super::record_terminal_failure_outcome(
                &server,
                "dispatch-scope-sym-a",
                "preflight",
                Some("codex"),
                Some(project_name),
            );

            let mut params = base_params();
            params.dispatch_id = Some("dispatch-scope-sym-a".to_string());
            params.project = Some(project_name.to_string());
            let status = record_complete_outcome(
                &server,
                &params,
                "eval-mem-sym-a",
                "success",
                "completed",
                None,
                true,
                true,
                &[],
            );
            assert_eq!(status["recorded"], json!(true));

            let rows = server
                .with_named_project_store(project_name, |store| {
                    memcore::list_outcomes_by_vendor_window(
                        store.connection(),
                        "codex",
                        "1970-01-01T00:00:00Z",
                        None,
                        memcore::OutcomeEvidenceClass::AnyAttribution,
                    )
                    .map_err(|e| e.to_string())
                })
                .expect("read named project outcomes");
            let matching: Vec<_> = rows
                .iter()
                .filter(|r| r.dispatch_id == "dispatch-scope-sym-a")
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "terminal-then-complete for the same dispatch/project must land ONE row \
                 in the named project store, not split across it and the global default"
            );
            assert_eq!(matching[0].execution_outcome, "completed");
        });
    }

    #[test]
    fn complete_then_terminal_same_project_lands_one_row() {
        with_named_project_env("hyperion", |project_name| {
            let (server, _dir) = test_server();

            let mut params = base_params();
            params.dispatch_id = Some("dispatch-scope-sym-b".to_string());
            params.project = Some(project_name.to_string());
            let status = record_complete_outcome(
                &server,
                &params,
                "eval-mem-sym-b",
                "success",
                "completed",
                None,
                true,
                true,
                &[],
            );
            assert_eq!(status["recorded"], json!(true));

            // A stray terminal classifier arriving after a real completion
            // must be a no-op (first-writer-wins), not a second row in a
            // different store.
            super::record_terminal_failure_outcome(
                &server,
                "dispatch-scope-sym-b",
                "watchdog",
                Some("codex"),
                Some(project_name),
            );

            let rows = server
                .with_named_project_store(project_name, |store| {
                    memcore::list_outcomes_by_vendor_window(
                        store.connection(),
                        "codex",
                        "1970-01-01T00:00:00Z",
                        None,
                        memcore::OutcomeEvidenceClass::AnyAttribution,
                    )
                    .map_err(|e| e.to_string())
                })
                .expect("read named project outcomes");
            let matching: Vec<_> = rows
                .iter()
                .filter(|r| r.dispatch_id == "dispatch-scope-sym-b")
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "complete-then-terminal for the same dispatch/project must land ONE row"
            );
            assert_eq!(
                matching[0].execution_outcome, "completed",
                "the real completion's verdict is preserved, not clobbered by the later stray terminal call"
            );
        });
    }

    // ─── #1035 adjudication kill-tests ──────────────────────────────────────

    fn signature_rec(id: &str) -> SignatureRecordParams {
        SignatureRecordParams {
            signature: id.to_string(),
            severity: None,
            evidence_ref: Some("review-1".to_string()),
            resolved: false,
            role: None,
            vendor: None,
        }
    }

    fn adjudication_outcome_rows(server: &MemoryServer, outcome_id: &str) -> Vec<memcore::DispatchAdjudication> {
        server
            .with_global_store_read(|store| {
                memcore::list_adjudications_for_outcome(store.connection(), outcome_id)
                    .map_err(|e| e.to_string())
            })
            .expect("read adjudications")
    }

    /// Kill-test 1: a complete carrying an adjudication writes the
    /// append-only judgment row linked to the outcome, and a replay of the
    /// same complete leaves that row byte-for-byte unchanged (the event_key
    /// short-circuits idempotently).
    #[test]
    fn adjudication_with_verdict_lands_and_replay_preserves_original() {
        let (server, _dir) = test_server();
        let mut params = base_params();
        params.adjudication = Some(AdjudicationParams {
            verdict: Some("accepted".to_string()),
            not_required_reason: None,
            adjudicator: "leader".to_string(),
            evidence_ref: Some("run-adjudication-1".to_string()),
        });
        params.signatures = vec![
            signature_rec("fake_security_fix"),
            signature_rec("zero_discriminating_test"),
        ];

        let outcome_status = record_complete_outcome(
            &server, &params, "eval-1", "success", "completed", None, true, true, &[],
        );
        assert_eq!(outcome_status["recorded"], json!(true));
        let outcome_id = outcome_status["outcome_id"].as_str().unwrap().to_string();

        let adj_status = record_complete_adjudication(&server, &params, &outcome_status);
        assert_eq!(adj_status["recorded"], json!(true), "adjudication recorded: {adj_status}");

        let rows = adjudication_outcome_rows(&server, &outcome_id);
        assert_eq!(rows.len(), 1, "exactly one adjudication row");
        assert_eq!(rows[0].verdict.as_deref(), Some("accepted"));
        assert_eq!(rows[0].not_required_reason, None);
        assert_eq!(rows[0].actor, "leader");
        assert_eq!(rows[0].evidence_ref, "run-adjudication-1");
        assert_eq!(rows[0].signatures.len(), 2);
        let is_adj = server
            .with_global_store_read(|store| {
                memcore::outcome_is_adjudicated(store.connection(), &outcome_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert!(is_adj, "outcome_is_adjudicated returns true after a terminal event");

        // Snapshot the first row for byte-for-byte comparison after replay.
        let first_row = rows.into_iter().next().unwrap();

        // Replay 1: same complete, NO adjudication → original row untouched.
        let mut replay_no_adj = params.clone();
        replay_no_adj.adjudication = None;
        let replay_status = record_complete_outcome(
            &server, &replay_no_adj, "eval-2", "success", "completed", None, true, true, &[],
        );
        assert_eq!(
            replay_status["outcome_id"], outcome_id,
            "same canonical outcome row on replay"
        );
        let replay_adj = record_complete_adjudication(&server, &replay_no_adj, &replay_status);
        assert_eq!(replay_adj, json!("skipped (no adjudication)"));
        let after_skip = adjudication_outcome_rows(&server, &outcome_id);
        assert_eq!(after_skip.len(), 1, "no new row from a no-adjudication replay");
        assert_eq!(after_skip[0], first_row, "original row untouched");

        // Replay 2: same complete, DIFFERENT adjudication → idempotent
        // short-circuit (event_key "complete:{outcome_id}" already exists,
        // so the original row is returned unchanged, not overwritten).
        let mut replay_diff_adj = params.clone();
        replay_diff_adj.adjudication = Some(AdjudicationParams {
            verdict: Some("rejected".to_string()),
            not_required_reason: None,
            adjudicator: "different-leader".to_string(),
            evidence_ref: Some("run-different".to_string()),
        });
        let replay2_status = record_complete_outcome(
            &server, &replay_diff_adj, "eval-3", "success", "completed", None, true, true, &[],
        );
        let replay2_adj = record_complete_adjudication(&server, &replay_diff_adj, &replay2_status);
        assert_eq!(replay2_adj["recorded"], json!(true), "replay adjudication status");
        let after_diff = adjudication_outcome_rows(&server, &outcome_id);
        assert_eq!(after_diff.len(), 1, "replay did not append a new row");
        assert_eq!(after_diff[0], first_row, "original row byte-for-byte unchanged on replay");
        assert_eq!(
            after_diff[0].verdict.as_deref(),
            Some("accepted"),
            "the original verdict survived a replay carrying a different one"
        );
    }

    /// Kill-test 1b: the not_required path (verdict=None, reason in closed set).
    #[test]
    fn adjudication_not_required_with_legal_reason_lands() {
        let (server, _dir) = test_server();
        let mut params = base_params();
        params.adjudication = Some(AdjudicationParams {
            verdict: None,
            not_required_reason: Some("superseded".to_string()),
            adjudicator: "leader".to_string(),
            evidence_ref: None, // defaults to outcome_id
        });

        let outcome_status = record_complete_outcome(
            &server, &params, "eval-nr", "success", "completed", None, true, true, &[],
        );
        let outcome_id = outcome_status["outcome_id"].as_str().unwrap().to_string();

        let adj_status = record_complete_adjudication(&server, &params, &outcome_status);
        assert_eq!(adj_status["recorded"], json!(true));

        let rows = adjudication_outcome_rows(&server, &outcome_id);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].verdict.is_none());
        assert_eq!(rows[0].not_required_reason.as_deref(), Some("superseded"));
        // evidence_ref defaulted to outcome_id when caller omitted it.
        assert_eq!(rows[0].evidence_ref, outcome_id);
    }

    /// Kill-test 1c: a not_required adjudication with an illegal reason code
    /// is rejected at the memcore write chokepoint — no row written, error in
    /// the pipeline JSON.
    #[test]
    fn adjudication_not_required_illegal_reason_rejected() {
        let (server, _dir) = test_server();
        let mut params = base_params();
        params.adjudication = Some(AdjudicationParams {
            verdict: None,
            not_required_reason: Some("made_up_reason".to_string()),
            adjudicator: "leader".to_string(),
            evidence_ref: None,
        });

        let outcome_status = record_complete_outcome(
            &server, &params, "eval-bad", "success", "completed", None, true, true, &[],
        );
        let adj_status = record_complete_adjudication(&server, &params, &outcome_status);
        assert_eq!(adj_status["recorded"], json!(false));
        assert!(
            adj_status["error"].as_str().unwrap().contains("made_up_reason"),
            "error must name the illegal reason: {adj_status}"
        );

        let outcome_id = outcome_status["outcome_id"].as_str().unwrap();
        let rows = adjudication_outcome_rows(&server, outcome_id);
        assert!(rows.is_empty(), "no row written for an illegal reason code");
    }

    /// Kill-test 4: an adjudication carrying an unknown signature id is
    /// rejected in full — the ENTIRE adjudication write is refused (not a
    /// partial write dropping the bad id), and the enclosing complete does
    /// not crash. The pipeline JSON carries an error naming the bad id; the
    /// DB has zero adjudication rows for this outcome.
    #[test]
    fn unknown_signature_id_rejects_entire_adjudication_without_crashing() {
        let (server, _dir) = test_server();
        let mut params = base_params();
        params.adjudication = Some(AdjudicationParams {
            verdict: Some("accepted".to_string()),
            not_required_reason: None,
            adjudicator: "leader".to_string(),
            evidence_ref: Some("run-1".to_string()),
        });
        params.signatures = vec![
            signature_rec("fake_security_fix"),      // valid
            signature_rec("totally_made_up_id"),      // unknown → must reject ALL
        ];

        let outcome_status = record_complete_outcome(
            &server, &params, "eval-unk", "success", "completed", None, true, true, &[],
        );
        assert_eq!(outcome_status["recorded"], json!(true));

        let adj_status = record_complete_adjudication(&server, &params, &outcome_status);
        // Rejected — but did not crash (complete proceeds).
        assert_eq!(
            adj_status["recorded"], json!(false),
            "unknown signature id must reject the adjudication, not silently accept it"
        );
        let err_msg = adj_status["error"].as_str().expect("error message present");
        assert!(
            err_msg.contains("totally_made_up_id"),
            "error must name the unknown id: {err_msg}"
        );

        // Zero adjudication rows — not even the valid signature's row landed.
        let outcome_id = outcome_status["outcome_id"].as_str().unwrap();
        let rows = adjudication_outcome_rows(&server, outcome_id);
        assert!(
            rows.is_empty(),
            "an unknown signature id must prevent the ENTIRE adjudication from landing, \
             not just the bad signature"
        );
    }
}
