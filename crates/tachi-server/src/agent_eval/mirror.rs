//! First-class mirror eval intake for harness-native subagents (#1066):
//! `tachi_agent_eval(action='register'|'observe'|'adjudicate'|'get')`.
//!
//! Thin translation layer over `memcore::mirror_eval` — this module owns
//! wire-shape (params in, JSON out) and the cross-model-independence /
//! self-eval computation (reusing `tachi_dispatch::identity::model_lineage_id`,
//! the Phase-1 identity-receipt primitive, per the #1066 frozen contract's
//! "internal reuse is mandatory" clause). All write/idempotency/conflict
//! semantics live in memcore; this module never re-implements them.

use serde_json::json;

use crate::server_state::MemoryServer;
use crate::tool_params::{
    MirrorEvalAdjudicateParams, MirrorEvalGetParams, MirrorEvalObserveParams,
    MirrorEvalRegisterParams,
};

/// The producer's effective model identity: prefer the carrier-observed
/// value from `observe` (ground truth of what actually ran) over the
/// `register`-time requested value (planned intent) — same planned-vs-
/// observed preference the #1065 identity receipt already establishes.
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

/// Cross-model independence requires BOTH sides to carry an identifiable
/// (non-unknown) effective engine AND those engines to differ (#1066 AC-7):
/// an unknown native model identity can never satisfy independence.
fn cross_model_independent(producer_lineage: &str, verifier_lineage: &str) -> bool {
    producer_lineage != tachi_dispatch::UNKNOWN_IDENTITY
        && verifier_lineage != tachi_dispatch::UNKNOWN_IDENTITY
        && producer_lineage != verifier_lineage
}

/// Self-eval: both sides are identifiably the SAME known engine. An unknown
/// identity on either side is unattributable, never proof of self-eval.
fn is_self_eval(producer_lineage: &str, verifier_lineage: &str) -> bool {
    producer_lineage != tachi_dispatch::UNKNOWN_IDENTITY
        && verifier_lineage != tachi_dispatch::UNKNOWN_IDENTITY
        && producer_lineage == verifier_lineage
}

pub(crate) fn handle_register(
    server: &MemoryServer,
    params: MirrorEvalRegisterParams,
) -> Result<String, String> {
    let new = memcore::NewMirrorEvalRun {
        frozen_contract_ref: params.frozen_contract_ref,
        execution_origin: params.execution_origin,
        lifecycle_owner: params.lifecycle_owner,
        harness: params.harness,
        native_child_id: params.native_child_id,
        requested_profile: params.requested_profile,
        requested_model: params.requested_model,
        requested_agent: params.requested_agent,
    };
    let run = server.with_global_store(|store| {
        memcore::register_mirror_eval_run(store.connection(), &new).map_err(|e| e.to_string())
    })?;
    serde_json::to_string(&json!({
        "eval_run_id": run.eval_run_id,
        "frozen_contract_ref": run.frozen_contract_ref,
        "execution_origin": run.execution_origin,
        "lifecycle_owner": run.lifecycle_owner,
        "harness": run.harness,
        "native_child_id": run.native_child_id,
        "created_at": run.created_at,
    }))
    .map_err(|e| format!("serialize register: {e}"))
}

pub(crate) fn handle_observe(
    server: &MemoryServer,
    params: MirrorEvalObserveParams,
) -> Result<String, String> {
    let eval_run_id = resolve_eval_run_id(
        server,
        params.eval_run_id.as_deref(),
        params.native_child_id.as_deref(),
    )?;
    let new = memcore::NewMirrorEvalObservation {
        eval_run_id: eval_run_id.clone(),
        terminal_outcome: params.terminal_outcome,
        duration_ms: params.duration_ms,
        cost_tokens: params.cost_tokens,
        cost_usd: params.cost_usd,
        result_ref: params.result_ref,
        artifacts: params.artifacts,
        effective_model: params.effective_model,
        effective_backend: params.effective_backend,
        effective_harness: params.effective_harness,
    };
    let observation = server.with_global_store(|store| {
        memcore::record_mirror_eval_observation(store.connection(), &new).map_err(|e| e.to_string())
    })?;
    serde_json::to_string(&json!({
        "eval_run_id": observation.eval_run_id,
        "observation_id": observation.observation_id,
        "terminal_outcome": observation.terminal_outcome,
        "created_at": observation.created_at,
    }))
    .map_err(|e| format!("serialize observe: {e}"))
}

/// Deterministic default `event_key` when the caller omits one: a
/// single-shot adjudicate call does not need to invent an id. Bound to
/// `(eval_run_id, actor, usefulness)` so a genuinely different judgment from
/// the same actor about the same run (a different `usefulness`) does not
/// collide with an unrelated prior event, while a same-actor/same-verdict
/// retry replays idempotently through memcore's payload-match check.
fn default_adjudication_event_key(eval_run_id: &str, actor: &str, usefulness: &str) -> String {
    format!("{eval_run_id}\u{1}{actor}\u{1}{usefulness}")
}

pub(crate) fn handle_adjudicate(
    server: &MemoryServer,
    params: MirrorEvalAdjudicateParams,
) -> Result<String, String> {
    let eval_run_id = resolve_eval_run_id(
        server,
        params.eval_run_id.as_deref(),
        params.native_child_id.as_deref(),
    )?;
    let event_key = params
        .event_key
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_else(|| {
            default_adjudication_event_key(&eval_run_id, &params.actor, &params.usefulness)
        });
    let new = memcore::NewMirrorEvalAdjudication {
        adjudication_id: uuid::Uuid::new_v4().to_string(),
        eval_run_id: eval_run_id.clone(),
        event_key,
        actor: params.actor,
        verifier_model: params.verifier_model,
        usefulness: params.usefulness,
        failure_mode: params.failure_mode,
        first_review_findings: params.first_review_findings,
        plan_delta: params.plan_delta,
        next_prompt_delta: params.next_prompt_delta,
        evidence_usable: params.evidence_usable,
        used_in_final_claim: params.used_in_final_claim,
        human_override: params.human_override,
        evidence_ref: params.evidence_ref,
    };
    let adjudication = server.with_global_store(|store| {
        memcore::append_mirror_eval_adjudication(store.connection(), &new)
            .map_err(|e| e.to_string())
    })?;

    let (run, observation) = server.with_global_store_read(|store| {
        let conn = store.connection();
        let run = memcore::get_run_by_id(conn, &eval_run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("eval_run_id '{eval_run_id}' vanished after adjudication"))?;
        let observation =
            memcore::get_observation(conn, &eval_run_id).map_err(|e| e.to_string())?;
        Ok((run, observation))
    })?;
    let producer_lineage = lineage_of(producer_model(&run, observation.as_ref()).as_deref());
    let verifier_lineage = lineage_of(adjudication.verifier_model.as_deref());

    serde_json::to_string(&json!({
        "eval_run_id": adjudication.eval_run_id,
        "adjudication_id": adjudication.adjudication_id,
        "event_key": adjudication.event_key,
        "usefulness": adjudication.usefulness,
        "evidence_usable": adjudication.evidence_usable,
        "insertion_seq": adjudication.insertion_seq,
        "created_at": adjudication.created_at,
        "cross_model_independent": cross_model_independent(&producer_lineage, &verifier_lineage),
        "self_eval": is_self_eval(&producer_lineage, &verifier_lineage),
    }))
    .map_err(|e| format!("serialize adjudicate: {e}"))
}

pub(crate) fn handle_get(
    server: &MemoryServer,
    params: MirrorEvalGetParams,
) -> Result<String, String> {
    let eval_run_id = params.eval_run_id.filter(|s| !s.trim().is_empty());
    let native_child_id = params.native_child_id.filter(|s| !s.trim().is_empty());
    if eval_run_id.is_none() && native_child_id.is_none() {
        return Err("get requires eval_run_id or native_child_id".to_string());
    }

    let view = server.with_global_store_read(|store| {
        memcore::get_mirror_eval_run_view(
            store.connection(),
            eval_run_id.as_deref(),
            native_child_id.as_deref(),
        )
        .map_err(|e| e.to_string())
    })?;
    let Some(view) = view else {
        return Err(format!(
            "no mirror eval run found for eval_run_id={eval_run_id:?} native_child_id={native_child_id:?}"
        ));
    };

    let producer_lineage =
        lineage_of(producer_model(&view.run, view.observation.as_ref()).as_deref());
    let (independent, self_eval, verifier_model) = match view.current_adjudication() {
        Some(adj) => {
            let verifier_lineage = lineage_of(adj.verifier_model.as_deref());
            (
                cross_model_independent(&producer_lineage, &verifier_lineage),
                is_self_eval(&producer_lineage, &verifier_lineage),
                adj.verifier_model.clone(),
            )
        }
        None => (false, false, None),
    };

    serde_json::to_string(&json!({
        "eval_run_id": view.run.eval_run_id,
        "frozen_contract_ref": view.run.frozen_contract_ref,
        "execution_origin": view.run.execution_origin,
        "lifecycle_owner": view.run.lifecycle_owner,
        "harness": view.run.harness,
        "native_child_id": view.run.native_child_id,
        "requested_profile": view.run.requested_profile,
        "requested_model": view.run.requested_model,
        "requested_agent": view.run.requested_agent,
        "created_at": view.run.created_at,
        "observation": view.observation.as_ref().map(|o| json!({
            "observation_id": o.observation_id,
            "terminal_outcome": o.terminal_outcome,
            "duration_ms": o.duration_ms,
            "cost_tokens": o.cost_tokens,
            "cost_usd": o.cost_usd,
            "result_ref": o.result_ref,
            "artifacts": o.artifacts,
            "effective_model": o.effective_model,
            "effective_backend": o.effective_backend,
            "effective_harness": o.effective_harness,
            "created_at": o.created_at,
        })),
        "is_adjudicated": view.is_adjudicated(),
        "adjudications": view.adjudications.iter().map(|a| json!({
            "adjudication_id": a.adjudication_id,
            "event_key": a.event_key,
            "actor": a.actor,
            "verifier_model": a.verifier_model,
            "usefulness": a.usefulness,
            "failure_mode": a.failure_mode,
            "first_review_findings": a.first_review_findings,
            "plan_delta": a.plan_delta,
            "next_prompt_delta": a.next_prompt_delta,
            "evidence_usable": a.evidence_usable,
            "used_in_final_claim": a.used_in_final_claim,
            "human_override": a.human_override,
            "evidence_ref": a.evidence_ref,
            "created_at": a.created_at,
            "insertion_seq": a.insertion_seq,
        })).collect::<Vec<_>>(),
        "current_adjudication_verifier_model": verifier_model,
        "cross_model_independent": independent,
        "self_eval": self_eval,
    }))
    .map_err(|e| format!("serialize get: {e}"))
}

/// Resolve the target `eval_run_id` for observe/adjudicate: caller-supplied
/// `eval_run_id` takes priority; otherwise resolve by `native_child_id`.
/// Neither present, or resolution finds nothing, is an explicit error — never
/// a silent no-op write against a guessed run.
pub(crate) fn resolve_eval_run_id(
    server: &MemoryServer,
    eval_run_id: Option<&str>,
    native_child_id: Option<&str>,
) -> Result<String, String> {
    if let Some(id) = eval_run_id.filter(|s| !s.trim().is_empty()) {
        return Ok(id.to_string());
    }
    let native_id = native_child_id
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "eval_run_id or native_child_id is required".to_string())?;
    let run = server.with_global_store_read(|store| {
        memcore::get_run_by_native_child_id(store.connection(), native_id)
            .map_err(|e| e.to_string())
    })?;
    run.map(|r| r.eval_run_id)
        .ok_or_else(|| format!("no mirror eval run registered for native_child_id '{native_id}'"))
}
