//! `tachi_agent_eval(action='route_projection')` — the ledger-backed routing
//! projection (tachi#1675 PR2, design D6 phase 1).
//!
//! This extends the EXISTING facade rather than adding a tool (the #1066
//! mirror precedent), and it runs in PARALLEL with the `/eval`-memory path:
//! `tachi_orchestrator(recommend)` is deliberately untouched here — flipping
//! its evidence input is PR4's explicitly versioned cutover, never a silent
//! change. Every response on this path carries `evidence_source` so a
//! consumer can tell which evidence base answered it.
//!
//! This module owns only what needs the outside world:
//! - the hard-gate candidate set, taken from the EXISTING risk classifier
//!   (`admission/required/blocked`) so pruning happens before any scoring;
//! - the `status.json` cross-check for the dispatch spine;
//! - the store read and the response shape.
//!
//! Every decision rule is in [`rules`], as a pure function of its inputs.

mod rules;

use serde_json::{json, Value};

use memcore::{EvalObservation, EvalSpine};

use crate::server_state::MemoryServer;
use crate::tool_params::RouteProjectionParams;

use self::rules::{ProjectionRow, TerminalCheck};

pub(crate) fn handle_route_projection(
    server: &MemoryServer,
    params: RouteProjectionParams,
    limit: Option<usize>,
) -> Result<String, String> {
    // Same cap the other `tachi_agent_eval` read actions use — one bound for
    // the whole facade, not a second one invented here. It also bounds the
    // per-row `status.json` reads below.
    let row_limit = super::capped_eval_limit(limit);
    let now = memcore::now_utc_iso();
    let window_days = params
        .window_days
        .unwrap_or(rules::DEFAULT_WINDOW_DAYS)
        .clamp(1, rules::MAX_WINDOW_DAYS);
    let since = rules::window_since(&now, window_days)
        .ok_or_else(|| format!("route_projection: cannot derive a window start from {now}"))?;

    let task = params.task.unwrap_or_default();
    let file_paths = params.file_paths.unwrap_or_default();
    let risk_override = params
        .risk
        .as_deref()
        .map(str::trim)
        .filter(|risk| !risk.is_empty());
    // The SAME classifier the recommendation path uses — not a second,
    // drifting copy of the admission rules.
    let risk = match params
        .task_type
        .as_deref()
        .map(str::trim)
        .filter(|task_type| !task_type.is_empty())
    {
        Some(task_type) => {
            tachi_dispatch::classify_dispatch_risk(&task, task_type, risk_override, &file_paths)
        }
        None => crate::dispatch_profile::classify_dispatch_risk(&task, risk_override, &file_paths),
    };
    let gates = eligible_candidate_set(&risk);

    let observations = server.with_global_store_read(|store| {
        memcore::list_eval_observations(store.connection(), &since, None, row_limit)
            .map_err(|err| format!("route_projection: read eval ledger: {err}"))
    })?;
    // A truncated read is a fact the consumer has to see: silently answering
    // from a capped slice of the window would be the projection overstating
    // what it looked at.
    let rows_truncated = observations.len() >= row_limit;

    let rows = observations
        .into_iter()
        .map(|observation| {
            let terminal = terminal_check_for(&observation);
            ProjectionRow {
                observation,
                terminal,
            }
        })
        .collect::<Vec<_>>();

    let query_task_type = (!risk.task_type.trim().is_empty()).then(|| risk.task_type.clone());
    let outcome = rules::project(
        &rows,
        &gates.eligible,
        &since,
        None,
        &now,
        query_task_type.as_deref(),
    );

    let payload = json!({
        "evidence_source": rules::EVIDENCE_SOURCE,
        "policy_version": rules::POLICY_VERSION,
        "generated_at": now,
        "window": {
            "since": since,
            "until": Value::Null,
            "days": window_days,
        },
        "task": {
            "task_type": risk.task_type,
            "risk": risk.risk,
            "reasons": risk.reasons,
            "required_profiles": risk.required_profiles,
            "blocked_profiles": risk.blocked_profiles,
        },
        "hard_gates": {
            "eligible_profiles": gates.eligible,
            "excluded_profiles": gates
                .excluded
                .iter()
                .map(|(profile, reason)| json!({"profile": profile, "reason": reason}))
                .collect::<Vec<_>>(),
            "notes": gates.notes,
        },
        "eligible_candidates": outcome
            .candidates
            .iter()
            .map(|candidate| candidate.to_json())
            .collect::<Vec<_>>(),
        "excluded_counts": outcome.excluded_counts,
        "excluded_rows": outcome.explained_exclusions_json(),
        "decision": outcome.decision.to_json(),
        "rows_considered": outcome.rows_considered,
        "rows_limit": row_limit,
        "rows_truncated": rows_truncated,
        "usable_rows": outcome.usable_rows,
        "quality_only_rows": outcome.quality_only_rows,
        "n_min_usable_rows": rules::N_MIN_USABLE_ROWS,
        "notes": [
            "latency is not_available for dispatch-spine rows: dispatch_outcomes has no \
             duration column (design D3 / codex finding 3); only mirror observations carry one",
            "mirror-spine rows are permanently off-policy for routing (no candidate set exists \
             to bind) and contribute quality evidence only",
            "no-evidence resolves to abstain, never to a baseline MBIT fit (design D7)",
            "raw ledger rows stay independently inspectable; excluded_rows is a capped explain \
             list, not the source of truth",
        ],
    });
    serde_json::to_string(&payload).map_err(|err| format!("serialize route_projection: {err}"))
}

/// The hard-gate result: which candidates the CURRENT admission rules allow,
/// and why each removed one was removed.
#[derive(Debug, Clone, Default)]
struct EligibleCandidates {
    eligible: Vec<String>,
    excluded: Vec<(String, &'static str)>,
    notes: Vec<String>,
}

const REASON_BLOCKED: &str = "blocked_by_risk_classifier";
const REASON_NOT_REQUIRED: &str = "not_required_for_risk_class";

/// Prune the candidate set with the EXISTING admission/required/blocked
/// logic, before any scoring touches it (design D6: "historical score can
/// never resurrect a removed candidate").
///
/// `blocked_profiles` is a removal, not a penalty — in the legacy scorer a
/// block was a `-40` score adjustment, which a strong enough history could
/// out-score. Here it is categorical.
///
/// `required_profiles`, when the classifier names any, RESTRICTS the set to
/// those profiles: at high/critical risk the classifier is stating who is
/// admissible, not merely who gets a bonus. If every required profile is
/// itself blocked or unknown (a contradictory classification, unreachable
/// with today's rules but not structurally impossible), the set comes back
/// EMPTY with an explicit note and the projection abstains. Dropping the
/// restriction instead would admit profiles the classifier never authorized —
/// at exactly the risk classes where the restriction exists — which is the
/// one direction a hard gate must never fail in.
fn eligible_candidate_set(risk: &tachi_dispatch::DispatchRisk) -> EligibleCandidates {
    let mut excluded = Vec::new();
    let mut notes = Vec::new();
    let mut after_block = Vec::new();
    for profile in tachi_dispatch::DISPATCH_PROFILES.iter() {
        if risk.blocked_profiles.iter().any(|p| p == profile.name) {
            excluded.push((profile.name.to_string(), REASON_BLOCKED));
        } else {
            after_block.push(profile.name.to_string());
        }
    }

    let eligible = if risk.required_profiles.is_empty() {
        after_block
    } else {
        let (required, rest): (Vec<String>, Vec<String>) = after_block
            .into_iter()
            .partition(|profile| risk.required_profiles.iter().any(|p| p == profile));
        for profile in rest {
            excluded.push((profile, REASON_NOT_REQUIRED));
        }
        if required.is_empty() {
            notes.push(format!(
                "contradictory classification: no required profile ({}) survives this risk \
                 class's blocks, so the candidate set is EMPTY and the projection abstains — \
                 admitting the non-required remainder would resurrect candidates the \
                 admission rules removed",
                risk.required_profiles.join(", ")
            ));
        }
        // Empty when the classification contradicts itself: the restriction
        // stands, and an empty set is the answer.
        required
    };

    EligibleCandidates {
        eligible,
        excluded,
        notes,
    }
}

/// Cross-check a durable terminal row against the live `status.json` receipt
/// (design D6's "terminal state exists and matches status.json").
///
/// The mirror spine has no `status.json` by construction — Tachi never
/// dispatched that work, so its observation row IS the terminal receipt and
/// there is nothing to reconcile against.
fn terminal_check_for(observation: &EvalObservation) -> TerminalCheck {
    if observation.spine == EvalSpine::Mirror {
        return TerminalCheck::NotApplicable;
    }
    let Some(dispatch_id) = observation
        .dispatch_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return TerminalCheck::Unverifiable;
    };
    // Same path gate every other status reader uses: an invalid or escaping
    // id behaves exactly like "receipt not found".
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return TerminalCheck::Unverifiable;
    }
    let status_path = crate::dispatch_ops::dispatch_runs_root()
        .join(dispatch_id)
        .join("status.json");
    let Ok(Some(status)) = crate::task_lifecycle::read_json_file(&status_path) else {
        return TerminalCheck::Unverifiable;
    };
    let Some((receipt_state, closure_kind)) = receipt_state_of(&status) else {
        return TerminalCheck::Unverifiable;
    };
    let durable = observation.terminal_outcome.as_deref().unwrap_or_default();
    compare_terminal_states(durable, receipt_state, closure_kind)
}

/// Read the receipt's terminal state AND the `closure_kind` that travels with
/// it out of a `status.json` body.
///
/// `resolved_completion.state` is the reconciled terminal state when a
/// completion path wrote one; otherwise the live `state`. The two are read as
/// a PAIR on purpose: `TASK_STATE_INPUT_REQUIRED` means a closed partial or an
/// active plan review depending on `closure_kind`, so a reader that takes the
/// state alone cannot tell a finished partial from an unfinished review.
fn receipt_state_of(status: &Value) -> Option<(&str, Option<&str>)> {
    let resolved = status.pointer("/resolved_completion");
    match resolved
        .and_then(|receipt| receipt.get("state"))
        .and_then(Value::as_str)
    {
        Some(state) => Some((
            state,
            resolved
                .and_then(|receipt| receipt.get("closure_kind"))
                .and_then(Value::as_str),
        )),
        // No resolved-completion receipt: the live `state` is all there is,
        // and it carries no partial-closure discriminator of its own.
        None => status
            .get("state")
            .and_then(Value::as_str)
            .map(|state| (state, None)),
    }
}

fn compare_terminal_states(
    execution_outcome: &str,
    receipt_state: &str,
    closure_kind: Option<&str>,
) -> TerminalCheck {
    let Some(durable_family) = outcome_family(execution_outcome) else {
        // An outcome vocabulary this projection does not know is not evidence
        // of a mismatch — it is an unverifiable claim.
        return TerminalCheck::Unverifiable;
    };
    let Some(receipt_family) = receipt_family(receipt_state, closure_kind) else {
        // The durable ledger says terminal, the live receipt says still
        // running: that disagreement is exactly what this check is for.
        return TerminalCheck::Mismatch;
    };
    // `partial` is a nuance the receipt's closed state vocabulary cannot
    // express; any terminal receipt is consistent with it.
    if durable_family == "partial" || durable_family == receipt_family {
        TerminalCheck::Consistent
    } else {
        TerminalCheck::Mismatch
    }
}

fn outcome_family(execution_outcome: &str) -> Option<&'static str> {
    match execution_outcome.trim().to_ascii_lowercase().as_str() {
        "completed" | "success" | "succeeded" => Some("completed"),
        "failed" | "failure" => Some("failed"),
        "aborted" | "canceled" | "cancelled" => Some("aborted"),
        "partial" => Some("partial"),
        _ => None,
    }
}

/// The receipt's terminal vocabulary. `TASK_STATE_INPUT_REQUIRED` is the one
/// ambiguous member: `tachi_complete(partial)` resolves to it, and so does an
/// ACTIVE plan review. The existing readers all disambiguate it with the
/// receipt's explicit `closure_kind` marker
/// (`dispatch_ops::dispatch::execution::completion_receipt_state`,
/// `dispatch_ops::board::status::is_terminal_state_with_closure_kind`), and
/// this one uses the SAME discriminator rather than inventing a third rule.
///
/// Without the marker the state is not terminal here either — a durable
/// terminal row against an in-flight plan review is a real disagreement.
fn receipt_family(receipt_state: &str, closure_kind: Option<&str>) -> Option<&'static str> {
    match receipt_state.trim() {
        "TASK_STATE_COMPLETED" => Some("completed"),
        "TASK_STATE_FAILED" => Some("failed"),
        "TASK_STATE_CANCELED" | "TASK_STATE_CANCELLED" => Some("aborted"),
        "TASK_STATE_INPUT_REQUIRED" if closure_kind == Some("partial") => Some("partial"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::TachiAgentEvalParams;

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "route-projection-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db_path, None).expect("test memory server")
    }

    fn eval_params(
        action: &str,
        projection: Option<RouteProjectionParams>,
    ) -> TachiAgentEvalParams {
        TachiAgentEvalParams {
            action: action.to_string(),
            fixture_path: None,
            limit: None,
            register: None,
            observe: None,
            adjudicate: None,
            get: None,
            projection,
        }
    }

    /// The action is reachable through the SAME `tachi_agent_eval` facade
    /// (no new tool), it declares its evidence source, and on an empty ledger
    /// it abstains — never a baseline fit, never a silent recommendation.
    #[tokio::test]
    async fn route_projection_action_is_ledger_sourced_and_abstains_without_evidence() {
        let server = test_server();
        let raw = crate::agent_eval::handle_agent_eval(
            &server,
            eval_params(
                "route_projection",
                Some(RouteProjectionParams {
                    task: Some("fix a bug in the parser".to_string()),
                    task_type: Some("fix_request".to_string()),
                    ..Default::default()
                }),
            ),
        )
        .await
        .expect("route_projection succeeds on an empty ledger");
        let payload: Value = serde_json::from_str(&raw).expect("valid JSON");

        assert_eq!(payload["evidence_source"], json!(rules::EVIDENCE_SOURCE));
        assert_eq!(payload["policy_version"], json!(rules::POLICY_VERSION));
        assert_eq!(payload["decision"]["kind"], json!("abstain"));
        assert_eq!(payload["decision"]["profile"], Value::Null);
        assert_eq!(payload["rows_considered"], json!(0));
        assert_eq!(payload["task"]["task_type"], json!("fix_request"));
        // A capped read is declared, so a truncated answer can never look
        // like a complete one.
        assert_eq!(
            payload["rows_limit"],
            json!(super::super::capped_eval_limit(None))
        );
        assert_eq!(payload["rows_truncated"], json!(false));
        assert!(
            payload["hard_gates"]["eligible_profiles"]
                .as_array()
                .is_some_and(|profiles| !profiles.is_empty()),
            "the hard-gate candidate set is reported even with zero evidence"
        );
        assert!(
            payload["eligible_candidates"]
                .as_array()
                .is_some_and(|candidates| !candidates.is_empty()),
            "every eligible candidate is listed, with its (empty) evidence window"
        );
        assert_eq!(payload["window"]["days"], json!(rules::DEFAULT_WINDOW_DAYS));
    }

    /// The payload is optional and the window is clamped, not trusted.
    #[tokio::test]
    async fn route_projection_accepts_no_payload_and_clamps_the_window() {
        let server = test_server();
        let raw =
            crate::agent_eval::handle_agent_eval(&server, eval_params("route_projection", None))
                .await
                .expect("route_projection succeeds with no payload");
        let payload: Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(payload["window"]["days"], json!(rules::DEFAULT_WINDOW_DAYS));

        let raw = crate::agent_eval::handle_agent_eval(
            &server,
            eval_params(
                "route_projection",
                Some(RouteProjectionParams {
                    window_days: Some(10_000),
                    ..Default::default()
                }),
            ),
        )
        .await
        .expect("route_projection succeeds with an absurd window");
        let payload: Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(payload["window"]["days"], json!(rules::MAX_WINDOW_DAYS));
    }

    /// Terminal reconciliation: agreement passes, disagreement is a mismatch,
    /// a still-running receipt against a durable terminal row is a mismatch,
    /// and an unknown vocabulary is unverifiable rather than "fine".
    #[test]
    fn terminal_state_comparison_is_conservative() {
        assert_eq!(
            compare_terminal_states("completed", "TASK_STATE_COMPLETED", None),
            TerminalCheck::Consistent
        );
        assert_eq!(
            compare_terminal_states("failed", "TASK_STATE_FAILED", None),
            TerminalCheck::Consistent
        );
        assert_eq!(
            compare_terminal_states("completed", "TASK_STATE_FAILED", None),
            TerminalCheck::Mismatch
        );
        assert_eq!(
            compare_terminal_states("completed", "TASK_STATE_WORKING", None),
            TerminalCheck::Mismatch
        );
        assert_eq!(
            compare_terminal_states("partial", "TASK_STATE_COMPLETED", None),
            TerminalCheck::Consistent
        );
        assert_eq!(
            compare_terminal_states("who_knows", "TASK_STATE_COMPLETED", None),
            TerminalCheck::Unverifiable
        );
    }

    /// A `tachi_complete(partial)` is a REAL terminal: it resolves to
    /// `TASK_STATE_INPUT_REQUIRED` with `closure_kind='partial'`
    /// (`dispatch_ops::predicate::resolve_completion_state`) and maps back to
    /// `execution_outcome='partial'` on the outcome row
    /// (`dispatch_ops::predicate::execution_outcome_for_kanban_state`).
    /// Reading that state without its discriminator excluded every partial
    /// completion as a bookkeeping fault.
    #[test]
    fn a_closed_partial_receipt_is_a_terminal_state_not_a_mismatch() {
        assert_eq!(
            compare_terminal_states("partial", "TASK_STATE_INPUT_REQUIRED", Some("partial")),
            TerminalCheck::Consistent
        );
        // Without the marker the SAME state is an active plan review, so a
        // durable terminal row against it is still a real disagreement.
        assert_eq!(
            compare_terminal_states("partial", "TASK_STATE_INPUT_REQUIRED", None),
            TerminalCheck::Mismatch
        );
        // And a partial receipt does not excuse a durable `completed` claim.
        assert_eq!(
            compare_terminal_states("completed", "TASK_STATE_INPUT_REQUIRED", Some("partial")),
            TerminalCheck::Mismatch
        );
    }

    /// The state and its `closure_kind` are read as a PAIR off the real
    /// `status.json` shape `complete_ops` writes — reading the state alone is
    /// what made a closed partial indistinguishable from an open plan review.
    #[test]
    fn the_receipt_state_is_read_together_with_its_closure_kind() {
        // The shape `complete_ops::handler` writes for `tachi_complete(partial)`.
        let partial = json!({
            "state": "TASK_STATE_WORKING",
            "resolved_completion": {
                "state": "TASK_STATE_INPUT_REQUIRED",
                "closure_kind": "partial",
                "eval_ledger_id": "mem-1",
                "reviewed": true,
                "recorded_at": "2026-08-10T00:00:00Z"
            }
        });
        assert_eq!(
            receipt_state_of(&partial),
            Some(("TASK_STATE_INPUT_REQUIRED", Some("partial")))
        );
        assert_eq!(
            compare_terminal_states("partial", "TASK_STATE_INPUT_REQUIRED", Some("partial")),
            TerminalCheck::Consistent,
            "a closed partial reconciles against its own receipt"
        );

        // A non-partial close carries an explicit null discriminator.
        let completed = json!({
            "resolved_completion": {"state": "TASK_STATE_COMPLETED", "closure_kind": null}
        });
        assert_eq!(
            receipt_state_of(&completed),
            Some(("TASK_STATE_COMPLETED", None))
        );

        // No resolved completion: the live state, and no discriminator to
        // borrow — an INPUT_REQUIRED here is an open plan review.
        let live = json!({"state": "TASK_STATE_INPUT_REQUIRED"});
        assert_eq!(
            receipt_state_of(&live),
            Some(("TASK_STATE_INPUT_REQUIRED", None))
        );
        assert_eq!(receipt_state_of(&json!({"dispatch_id": "d-1"})), None);
    }

    /// A contradictory classification fails CLOSED. Unreachable with today's
    /// classifier (required and blocked are disjoint at every risk class), so
    /// the risk is built directly — the point is that the gate cannot be made
    /// to re-admit unauthorized profiles by feeding it a contradiction.
    #[test]
    fn a_contradictory_required_set_empties_the_gate_instead_of_reopening_it() {
        let risk = tachi_dispatch::DispatchRisk {
            task_type: "migration_request".to_string(),
            risk: "critical".to_string(),
            reasons: vec!["test".to_string()],
            required_profiles: vec!["codex_53_fast".to_string()],
            blocked_profiles: vec!["codex_53_fast".to_string()],
        };
        let gates = eligible_candidate_set(&risk);
        assert!(
            gates.eligible.is_empty(),
            "the non-required remainder must not re-enter, got {:?}",
            gates.eligible
        );
        assert!(
            gates.notes.iter().any(|note| note.contains("EMPTY")),
            "an empty set is stated, never silent: {:?}",
            gates.notes
        );
        assert!(gates
            .excluded
            .iter()
            .any(|(profile, reason)| profile == "codex_53_fast" && *reason == REASON_BLOCKED));
        assert!(
            gates
                .excluded
                .iter()
                .any(|(_, reason)| *reason == REASON_NOT_REQUIRED),
            "every profile the gate removed is reported with its reason"
        );
    }

    /// Hard gates: a blocked profile is REMOVED from the candidate set, not
    /// merely penalized — the structural half of discrimination 5.
    #[test]
    fn blocked_profiles_are_removed_from_the_candidate_set() {
        let risk = tachi_dispatch::classify_dispatch_risk(
            "rewrite the auth token validation path",
            "fix_request",
            Some("critical"),
            &[],
        );
        assert!(
            !risk.blocked_profiles.is_empty(),
            "the critical risk class blocks at least one profile"
        );
        let gates = eligible_candidate_set(&risk);
        for blocked in &risk.blocked_profiles {
            assert!(
                !gates.eligible.iter().any(|p| p == blocked),
                "{blocked} must not survive the hard gate"
            );
            assert!(
                gates
                    .excluded
                    .iter()
                    .any(|(profile, reason)| profile == blocked && *reason == REASON_BLOCKED),
                "{blocked} must be reported as blocked, not silently dropped"
            );
        }
    }

    /// When the classifier names required profiles, the candidate set is
    /// restricted to them and everything else is reported as excluded.
    #[test]
    fn required_profiles_restrict_the_candidate_set() {
        let risk =
            tachi_dispatch::classify_dispatch_risk("plan the migration", "plan_request", None, &[]);
        assert!(!risk.required_profiles.is_empty());
        let gates = eligible_candidate_set(&risk);
        for profile in &gates.eligible {
            assert!(
                risk.required_profiles.iter().any(|p| p == profile),
                "{profile} is eligible but not required"
            );
        }
        assert!(gates
            .excluded
            .iter()
            .any(|(_, reason)| *reason == REASON_NOT_REQUIRED));
    }

    /// No admission rule may yield an empty candidate set silently.
    #[test]
    fn eligible_set_is_never_silently_empty() {
        for (task, task_type, risk_override) in [
            ("fix a typo", "fix_request", None),
            ("review this PR", "review_request", None),
            ("rewrite auth", "migration_request", Some("critical")),
            ("explain this module", "explain_request", None),
        ] {
            let risk = tachi_dispatch::classify_dispatch_risk(task, task_type, risk_override, &[]);
            let gates = eligible_candidate_set(&risk);
            assert!(
                !gates.eligible.is_empty(),
                "empty eligible set for {task_type}/{risk_override:?}"
            );
        }
    }
}
