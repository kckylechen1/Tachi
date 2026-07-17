//! Project #1066 mirror eval intake rows into `tachi_complete`'s
//! `subagents[]`-compatible aggregation surface (AC-5, AC-4, AC-6).
//!
//! Only ADJUDICATED, evidence-usable, non-self-eval rows are projected —
//! "only adjudicated, evidence-usable rows enter routing/card aggregation.
//! Self-eval may be stored but cannot promote itself" (frozen contract). An
//! unresolved `eval_run_id`, a run with no observation/adjudication yet, an
//! `evidence_usable=false` verdict, or a self-eval verdict is silently
//! skipped — completion must never fail because of an "extra" reference.
//!
//! This never mutates `tachi_dispatch::eval`'s aggregate functions: the
//! existing `subagent_has_usable_evidence` gate there already requires
//! `result_collected != Some(false)` and `evidence_usable != Some(false)`,
//! and every row this module projects sets both fields `Some(true)` — the
//! projection is eligible-only BEFORE it ever reaches that gate.

use crate::server_state::MemoryServer;
use crate::tool_params::TachiSubagentEvalParams;

/// The producer's effective model: prefer the carrier-observed identity from
/// `observe` over the `register`-time requested identity.
fn producer_model(
    run: &memcore::MirrorEvalRun,
    observation: Option<&memcore::MirrorEvalObservation>,
) -> Option<String> {
    observation
        .and_then(|o| o.effective_model.clone())
        .filter(|m| !m.trim().is_empty())
        .or_else(|| run.requested_model.clone())
}

fn lineage_of(model: Option<&str>) -> String {
    tachi_dispatch::model_lineage_id(model, tachi_dispatch::UNKNOWN_IDENTITY)
}

/// Producer identity for cross-model GATING (self-eval, AC-7) — the
/// carrier-OBSERVED `effective_model` ONLY, never the register-time
/// requested/planned value. Symmetric with
/// `crate::agent_eval::mirror::producer_lineage_for_gating` (see that
/// function's doc for why a fallback to `requested_model` here would let a
/// `register`-only row (no `observe` yet) launder planned intent into an
/// attribution strong enough to dodge the self-eval gate — codex round-2
/// finding #4). `producer_model` above still prefers observed-over-requested
/// for the *display* `model:` field projected below; this function is
/// gating-only.
fn producer_lineage_for_gating(observation: Option<&memcore::MirrorEvalObservation>) -> String {
    lineage_of(
        observation
            .and_then(|o| o.effective_model.as_deref())
            .filter(|m| !m.trim().is_empty()),
    )
}

/// Self-eval: producer and verifier are identifiably the SAME known engine.
/// An unknown identity on either side is unattributable, never proof of
/// self-eval — it also never satisfies cross-model independence (AC-7), so
/// it is not silently treated as "safe to promote" either. Symmetric with
/// `crate::agent_eval::mirror::is_self_eval`.
fn is_self_eval(producer_lineage: &str, verifier_lineage: &str) -> bool {
    producer_lineage != tachi_dispatch::UNKNOWN_IDENTITY
        && verifier_lineage != tachi_dispatch::UNKNOWN_IDENTITY
        && producer_lineage == verifier_lineage
}

fn to_subagent_eval_params(view: &memcore::MirrorEvalRunView) -> Option<TachiSubagentEvalParams> {
    let adjudication = view.current_adjudication()?;
    if !adjudication.evidence_usable {
        return None;
    }
    let producer_lineage = producer_lineage_for_gating(view.observation.as_ref());
    let verifier_lineage = lineage_of(adjudication.verifier_model.as_deref());
    if is_self_eval(&producer_lineage, &verifier_lineage) {
        return None;
    }

    let observation = view.observation.as_ref();
    let notes = (!adjudication.first_review_findings.is_empty())
        .then(|| adjudication.first_review_findings.join("; "));

    Some(TachiSubagentEvalParams {
        role: view
            .run
            .requested_profile
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "other".to_string()),
        agent: view
            .run
            .requested_agent
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
        model: producer_model(&view.run, observation),
        task: None,
        task_type: None,
        outcome: Some(adjudication.usefulness.clone()),
        usefulness_score: None,
        failure_mode: adjudication.failure_mode.clone(),
        verification_impact: None,
        verification_present: true,
        evaluator: Some(adjudication.actor.clone()),
        plan_delta: adjudication.plan_delta.clone(),
        human_override: adjudication.human_override,
        retry_count: 0,
        notes,
        latency_ms: observation.and_then(|o| o.duration_ms),
        input_tokens: None,
        output_tokens: None,
        cost_tokens: observation.and_then(|o| o.cost_tokens),
        cost_usd: observation.and_then(|o| o.cost_usd),
        execution_origin: Some(view.run.execution_origin.clone()),
        lifecycle_owner: Some(view.run.lifecycle_owner.clone()),
        harness: view.run.harness.clone(),
        native_agent_id: view.run.native_child_id.clone(),
        tachi_dispatch_id: None,
        result_collected: Some(observation.is_some()),
        evidence_usable: Some(true),
        used_in_final_claim: Some(adjudication.used_in_final_claim),
        next_prompt_delta: adjudication.next_prompt_delta.clone(),
    })
}

/// Resolve each `eval_run_id` and project the eligible ones. Never FAILS the
/// completion — an unresolved (never-registered), ineligible (unadjudicated
/// / not evidence-usable / self-eval), or lookup-erroring id is simply
/// absent from the returned vec — but a genuine storage-read error (codex
/// round-2 finding #4a) is NOT the same event as a benign "id not found":
/// it is distinguished with `tracing::error!` (vs `warn!` for not-found) so
/// it is operationally alertable, and its id is returned separately so the
/// caller can disclose it on the completion record instead of the failure
/// being invisible outside a log line.
pub(super) fn project_eval_run_ids(
    server: &MemoryServer,
    eval_run_ids: &[String],
) -> (Vec<TachiSubagentEvalParams>, Vec<String>) {
    let mut projected = Vec::new();
    let mut lookup_errors = Vec::new();
    for eval_run_id in eval_run_ids {
        let eval_run_id = eval_run_id.trim();
        if eval_run_id.is_empty() {
            continue;
        }
        let view = server.with_global_store_read(|store| {
            memcore::get_mirror_eval_run_view(store.connection(), Some(eval_run_id), None)
                .map_err(|e| e.to_string())
        });
        let view = match view {
            Ok(Some(view)) => view,
            Ok(None) => {
                tracing::warn!(
                    eval_run_id,
                    "eval_run_ids: no mirror eval run found, skipping"
                );
                continue;
            }
            Err(error) => {
                tracing::error!(
                    eval_run_id,
                    error = %error,
                    "eval_run_ids: lookup failed (storage error, not a missing reference), skipping"
                );
                lookup_errors.push(eval_run_id.to_string());
                continue;
            }
        };
        if let Some(entry) = to_subagent_eval_params(&view) {
            projected.push(entry);
        }
    }
    (projected, lookup_errors)
}

/// AC-8 / codex round-2 finding #3b: net-new-capability coverage, no
/// pre-existing entry point on `origin/main` to regress — structural-
/// justification exception (compile-red, not behavioral-red); canonical
/// explanation in `memcore::db::migrations::mirror_eval`'s module doc.
#[cfg(test)]
mod tests {
    use super::*;

    fn base_run(native_id: &str) -> memcore::NewMirrorEvalRun {
        memcore::NewMirrorEvalRun {
            frozen_contract_ref: "kckylechen1/tachi#1066".to_string(),
            execution_origin: "host_native_subagent".to_string(),
            lifecycle_owner: "host".to_string(),
            harness: Some("claude_code_task_tool".to_string()),
            native_child_id: Some(native_id.to_string()),
            requested_profile: Some("explore".to_string()),
            requested_model: Some("anthropic/claude-sonnet".to_string()),
            requested_agent: Some("claude".to_string()),
        }
    }

    fn open_conn() -> rusqlite::Connection {
        memcore::enable_simple_auto_extension().unwrap();
        memcore::register_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        memcore::init_schema(&conn).unwrap();
        conn
    }

    /// AC-4 / self-eval: an unadjudicated run projects to nothing. Codex
    /// round-2 finding #3c: an observation is deliberately present (unlike
    /// the pre-fix fixture, which omitted it) so a None result here can only
    /// be attributed to the intended discriminator — "no adjudication" — and
    /// not to an accidental "no observation" gate that would pass this test
    /// for the WRONG reason (a bug that gated on observation presence would
    /// have silently passed both this test and the one below, without
    /// exercising either's real discriminator).
    #[test]
    fn unadjudicated_run_is_not_projected() {
        let conn = open_conn();
        let run = memcore::register_mirror_eval_run(&conn, &base_run("p-1")).unwrap();
        memcore::record_mirror_eval_observation(
            &conn,
            &memcore::NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let view = memcore::get_mirror_eval_run_view(&conn, Some(&run.eval_run_id), None)
            .unwrap()
            .unwrap();
        assert!(to_subagent_eval_params(&view).is_none());
    }

    /// AC-4: evidence_usable=false is not projected even though adjudicated.
    /// Observation present (see `unadjudicated_run_is_not_projected`'s doc
    /// comment for why) so this test's None is attributable ONLY to
    /// `evidence_usable=false`, not to a coincidentally-absent observation.
    #[test]
    fn not_evidence_usable_is_not_projected() {
        let conn = open_conn();
        let run = memcore::register_mirror_eval_run(&conn, &base_run("p-2")).unwrap();
        memcore::record_mirror_eval_observation(
            &conn,
            &memcore::NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "failure".to_string(),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        memcore::append_mirror_eval_adjudication(
            &conn,
            &memcore::NewMirrorEvalAdjudication {
                adjudication_id: "adj-1".to_string(),
                eval_run_id: run.eval_run_id.clone(),
                event_key: "adj-1-key".to_string(),
                actor: "leader".to_string(),
                verifier_model: Some("openai/gpt-5".to_string()),
                usefulness: "failed".to_string(),
                evidence_usable: false,
                evidence_ref: "run-1".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let view = memcore::get_mirror_eval_run_view(&conn, Some(&run.eval_run_id), None)
            .unwrap()
            .unwrap();
        assert!(to_subagent_eval_params(&view).is_none());
    }

    /// Self-eval cannot promote itself even when marked evidence_usable.
    #[test]
    fn self_eval_is_not_projected() {
        let conn = open_conn();
        let run = memcore::register_mirror_eval_run(&conn, &base_run("p-3")).unwrap();
        memcore::record_mirror_eval_observation(
            &conn,
            &memcore::NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        memcore::append_mirror_eval_adjudication(
            &conn,
            &memcore::NewMirrorEvalAdjudication {
                adjudication_id: "adj-2".to_string(),
                eval_run_id: run.eval_run_id.clone(),
                event_key: "adj-2-key".to_string(),
                actor: "self".to_string(),
                // Same lineage as the producer's observed effective model.
                verifier_model: Some("anthropic/claude-opus".to_string()),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                evidence_ref: "run-2".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let view = memcore::get_mirror_eval_run_view(&conn, Some(&run.eval_run_id), None)
            .unwrap()
            .unwrap();
        assert!(
            to_subagent_eval_params(&view).is_none(),
            "same-lineage producer/verifier must not promote itself"
        );
    }

    /// A cross-model adjudicated, evidence-usable run DOES project, carrying
    /// the adjudicator's judgment fields through.
    #[test]
    fn eligible_run_projects_with_judgment_fields() {
        let conn = open_conn();
        let run = memcore::register_mirror_eval_run(&conn, &base_run("p-4")).unwrap();
        memcore::record_mirror_eval_observation(
            &conn,
            &memcore::NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                duration_ms: Some(4_200),
                cost_tokens: Some(1_500),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        memcore::append_mirror_eval_adjudication(
            &conn,
            &memcore::NewMirrorEvalAdjudication {
                adjudication_id: "adj-3".to_string(),
                eval_run_id: run.eval_run_id.clone(),
                event_key: "adj-3-key".to_string(),
                actor: "leader".to_string(),
                verifier_model: Some("openai/gpt-5".to_string()),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                used_in_final_claim: true,
                evidence_ref: "run-3".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let view = memcore::get_mirror_eval_run_view(&conn, Some(&run.eval_run_id), None)
            .unwrap()
            .unwrap();
        let projected = to_subagent_eval_params(&view).expect("eligible run must project");
        assert_eq!(projected.outcome.as_deref(), Some("useful"));
        assert_eq!(projected.evaluator.as_deref(), Some("leader"));
        assert_eq!(projected.evidence_usable, Some(true));
        assert_eq!(projected.used_in_final_claim, Some(true));
        assert_eq!(projected.native_agent_id.as_deref(), Some("p-4"));
        assert_eq!(projected.latency_ms, Some(4_200));
        assert_eq!(projected.cost_tokens, Some(1_500));
    }
}
