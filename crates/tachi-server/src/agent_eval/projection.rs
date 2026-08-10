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

/// Row cap for one projection read. The ledger stays independently
/// inspectable; this only bounds one response.
const MAX_PROJECTION_ROWS: usize = 2_000;

pub(crate) fn handle_route_projection(
    server: &MemoryServer,
    params: RouteProjectionParams,
) -> Result<String, String> {
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
        memcore::list_eval_observations(store.connection(), &since, None, MAX_PROJECTION_ROWS)
            .map_err(|err| format!("route_projection: read eval ledger: {err}"))
    })?;

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
/// itself blocked (a contradictory classification), the restriction is
/// dropped with an explicit note rather than yielding an empty set.
fn eligible_candidate_set(risk: &tachi_dispatch::DispatchRisk) -> EligibleCandidates {
    let mut gates = EligibleCandidates::default();
    let mut after_block = Vec::new();
    for profile in tachi_dispatch::DISPATCH_PROFILES.iter() {
        if risk.blocked_profiles.iter().any(|p| p == profile.name) {
            gates
                .excluded
                .push((profile.name.to_string(), REASON_BLOCKED));
        } else {
            after_block.push(profile.name.to_string());
        }
    }

    if risk.required_profiles.is_empty() {
        gates.eligible = after_block;
        return gates;
    }

    let (required, rest): (Vec<String>, Vec<String>) = after_block
        .into_iter()
        .partition(|profile| risk.required_profiles.iter().any(|p| p == profile));
    if required.is_empty() {
        gates.notes.push(format!(
            "every required profile ({}) is also blocked for this risk class; the required \
             restriction was dropped rather than emptying the candidate set",
            risk.required_profiles.join(", ")
        ));
        gates.eligible = rest;
        return gates;
    }
    for profile in rest {
        gates.excluded.push((profile, REASON_NOT_REQUIRED));
    }
    gates.eligible = required;
    gates
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
    // `resolved_completion.state` is the reconciled terminal state when a
    // completion path wrote one; otherwise the live `state`.
    let receipt_state = status
        .pointer("/resolved_completion/state")
        .and_then(Value::as_str)
        .or_else(|| status.get("state").and_then(Value::as_str));
    let Some(receipt_state) = receipt_state else {
        return TerminalCheck::Unverifiable;
    };
    let durable = observation.terminal_outcome.as_deref().unwrap_or_default();
    compare_terminal_states(durable, receipt_state)
}

fn compare_terminal_states(execution_outcome: &str, receipt_state: &str) -> TerminalCheck {
    let Some(durable_family) = outcome_family(execution_outcome) else {
        // An outcome vocabulary this projection does not know is not evidence
        // of a mismatch — it is an unverifiable claim.
        return TerminalCheck::Unverifiable;
    };
    let Some(receipt_family) = receipt_family(receipt_state) else {
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

fn receipt_family(receipt_state: &str) -> Option<&'static str> {
    match receipt_state.trim() {
        "TASK_STATE_COMPLETED" => Some("completed"),
        "TASK_STATE_FAILED" => Some("failed"),
        "TASK_STATE_CANCELED" | "TASK_STATE_CANCELLED" => Some("aborted"),
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
            compare_terminal_states("completed", "TASK_STATE_COMPLETED"),
            TerminalCheck::Consistent
        );
        assert_eq!(
            compare_terminal_states("failed", "TASK_STATE_FAILED"),
            TerminalCheck::Consistent
        );
        assert_eq!(
            compare_terminal_states("completed", "TASK_STATE_FAILED"),
            TerminalCheck::Mismatch
        );
        assert_eq!(
            compare_terminal_states("completed", "TASK_STATE_WORKING"),
            TerminalCheck::Mismatch
        );
        assert_eq!(
            compare_terminal_states("partial", "TASK_STATE_COMPLETED"),
            TerminalCheck::Consistent
        );
        assert_eq!(
            compare_terminal_states("who_knows", "TASK_STATE_COMPLETED"),
            TerminalCheck::Unverifiable
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
