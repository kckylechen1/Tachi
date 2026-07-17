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

fn lineage_of(model: Option<&str>) -> String {
    tachi_dispatch::model_lineage_id(model, tachi_dispatch::UNKNOWN_IDENTITY)
}

/// Codex round-2 finding #4d: register/observe/adjudicate persisted every
/// caller-supplied string field verbatim — "existing completion scrubs
/// equivalent [free-text fields]... mirror path has zero scrubbing," a
/// direct violation of the frozen contract's "raw transcripts, hidden
/// reasoning, and secrets never enter storage." `scrub_secrets` is the SAME
/// pure, deterministic regex scrub `complete_ops::eval_record` already
/// applies to `tachi_complete`'s free-text fields (bearer tokens, api
/// keys/tokens/secrets/passwords, `sk-`/`gh?_`/`AKIA`/`xox`/`voy-` literals);
/// being pure and deterministic, scrubbing EVERY string field here —
/// including identifier-shaped ones used for idempotency/lookup matching
/// (`native_child_id`, `event_key`, ...) — is safe for idempotent replay: a
/// legitimate (non-secret-shaped) value scrubs to itself every time, so
/// replay/lookup keys stay stable: only a genuinely secret-shaped value
/// (which was never a legitimate id/model-name/actor to begin with) is
/// altered.
fn scrub(text: String) -> String {
    crate::memory_search_ops::scrub_secrets(&text).0
}

fn scrub_opt(text: Option<String>) -> Option<String> {
    text.map(scrub)
}

fn scrub_vec(values: Vec<String>) -> Vec<String> {
    values.into_iter().map(scrub).collect()
}

/// Producer identity for cross-model GATING (self-eval / independence,
/// #1066 AC-7) — the carrier-OBSERVED `effective_model` ONLY, never the
/// register-time requested/planned value. Gating must never launder a
/// `register`-only run's *requested* identity into something strong enough
/// to pass cross-model independence or dodge a same-engine self-eval: AC-7
/// is explicit that "unknown native model identity ... cannot satisfy
/// cross-model independence," and a run with no `observe` call yet (or an
/// `observe` that omitted `effective_model`) has zero carrier confirmation
/// of what actually ran. Codex round-2 finding #4: the pre-fix code
/// (`producer_model` in `mirror_eval_projection.rs`, formerly duplicated
/// here too) fell back to the requested value for gating, so a known
/// `requested_model` alone could satisfy independence (or evade self-eval)
/// without any observation ever having occurred.
fn producer_lineage_for_gating(observation: Option<&memcore::MirrorEvalObservation>) -> String {
    lineage_of(
        observation
            .and_then(|o| o.effective_model.as_deref())
            .filter(|m| !m.trim().is_empty()),
    )
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
        frozen_contract_ref: scrub(params.frozen_contract_ref),
        execution_origin: scrub(params.execution_origin),
        lifecycle_owner: scrub(params.lifecycle_owner),
        harness: scrub_opt(params.harness),
        native_child_id: scrub_opt(params.native_child_id),
        requested_profile: scrub_opt(params.requested_profile),
        requested_model: scrub_opt(params.requested_model),
        requested_agent: scrub_opt(params.requested_agent),
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
    // Scrub `native_child_id` BEFORE the lookup, not just before storage: it
    // is a lookup key against what `register` stored (already scrubbed
    // there), so an unscrubbed lookup value would fail to resolve a run
    // whose raw native id happened to be secret-shaped. `scrub` is a no-op
    // on any legitimate (non-secret-shaped) id.
    let native_child_id = scrub_opt(params.native_child_id);
    let eval_run_id = resolve_eval_run_id(
        server,
        params.eval_run_id.as_deref(),
        native_child_id.as_deref(),
    )?;
    let new = memcore::NewMirrorEvalObservation {
        eval_run_id: eval_run_id.clone(),
        terminal_outcome: scrub(params.terminal_outcome),
        duration_ms: params.duration_ms,
        cost_tokens: params.cost_tokens,
        cost_usd: params.cost_usd,
        result_ref: scrub_opt(params.result_ref),
        artifacts: scrub_vec(params.artifacts),
        effective_model: scrub_opt(params.effective_model),
        effective_backend: scrub_opt(params.effective_backend),
        effective_harness: scrub_opt(params.effective_harness),
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
    // Scrub BEFORE lookup/key-derivation for the same reason as
    // `handle_observe`: `native_child_id` is a lookup key, and `actor`/
    // `usefulness` feed the default `event_key` derivation below — scrubbing
    // after computing the key would make a caller-omitted `event_key` derive
    // from raw (unscrubbed) text while the persisted `actor`/`usefulness`
    // are scrubbed, a needless inconsistency `scrub`'s determinism costs
    // nothing to avoid.
    let native_child_id = scrub_opt(params.native_child_id);
    let actor = scrub(params.actor);
    let usefulness = scrub(params.usefulness);
    let eval_run_id = resolve_eval_run_id(
        server,
        params.eval_run_id.as_deref(),
        native_child_id.as_deref(),
    )?;
    let event_key = params
        .event_key
        .map(scrub)
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_else(|| default_adjudication_event_key(&eval_run_id, &actor, &usefulness));
    let new = memcore::NewMirrorEvalAdjudication {
        adjudication_id: uuid::Uuid::new_v4().to_string(),
        eval_run_id: eval_run_id.clone(),
        event_key,
        actor,
        verifier_model: scrub_opt(params.verifier_model),
        usefulness,
        failure_mode: scrub_opt(params.failure_mode),
        first_review_findings: scrub_vec(params.first_review_findings),
        plan_delta: scrub_opt(params.plan_delta),
        next_prompt_delta: scrub_opt(params.next_prompt_delta),
        evidence_usable: params.evidence_usable,
        used_in_final_claim: params.used_in_final_claim,
        human_override: params.human_override,
        evidence_ref: scrub(params.evidence_ref),
    };
    let adjudication = server.with_global_store(|store| {
        memcore::append_mirror_eval_adjudication(store.connection(), &new)
            .map_err(|e| e.to_string())
    })?;

    // `_run` is fetched only as an existence check (the "vanished after
    // adjudication" error below) — gating below uses ONLY the observation,
    // never the run's requested identity (see `producer_lineage_for_gating`).
    let (_run, observation) = server.with_global_store_read(|store| {
        let conn = store.connection();
        let run = memcore::get_run_by_id(conn, &eval_run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("eval_run_id '{eval_run_id}' vanished after adjudication"))?;
        let observation =
            memcore::get_observation(conn, &eval_run_id).map_err(|e| e.to_string())?;
        Ok((run, observation))
    })?;
    let producer_lineage = producer_lineage_for_gating(observation.as_ref());
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
    // Scrub before lookup, same rationale as handle_observe/handle_adjudicate
    // — native_child_id is a lookup key against what register stored scrubbed.
    let native_child_id = scrub_opt(params.native_child_id).filter(|s| !s.trim().is_empty());
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

    let producer_lineage = producer_lineage_for_gating(view.observation.as_ref());
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
