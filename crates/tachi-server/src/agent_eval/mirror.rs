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
#[cfg(test)]
use serde_json::Value;

use crate::server_state::MemoryServer;
use crate::tool_params::{
    EvalRubricParams, MirrorEvalAdjudicateParams, MirrorEvalGetParams, MirrorEvalObserveParams,
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
    // tachi#1675 PR1 D3: hold onto the rubric block (if any) and the
    // adjudicator actor identity BEFORE `new` partially moves `actor` — the
    // rubric write below needs both.
    let rubric_params = params.rubric;
    let adjudicator_actor = actor.clone();
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

    // tachi#1675 PR1 D3: optional structured rubric companion row. Best-effort
    // — a rubric write failure never fails the (already-appended, already
    // authoritative) free-text adjudication above.
    let rubric_row = rubric_params.map(|rubric| {
        write_rubric_score_best_effort(
            server,
            &adjudication.adjudication_id,
            &adjudicator_actor,
            &producer_lineage,
            &verifier_lineage,
            rubric,
        )
    });

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
        "rubric_written": rubric_row.map(|r| r.is_some()),
    }))
    .map_err(|e| format!("serialize adjudicate: {e}"))
}

/// tachi#1675 PR1 D3: rubric v1 is a CODE CONSTANT — no runtime-mutable
/// rubric entity, no new model-facing mutation surface (`tachi_tune`'s 8
/// closed actions stay closed). `rubric_hash` on every row pins it to this
/// exact definition so a later rubric revision cannot silently reinterpret
/// an old row. Reuses the SAME content-addressing primitive
/// `route_policy_source_revision`/route-policy proposals already use — not a
/// second hashing scheme.
fn rubric_v1_hash() -> String {
    crate::tune_ops::route_policy::content_digest_hex(&json!({
        "rubric": "v1",
        "dimensions": [
            "contract_correctness", "evidence_quality", "safety",
            "scope_discipline", "intervention_burden", "completion_integrity",
        ],
        "dimension_values": memcore::RUBRIC_DIMENSION_VALUES,
        "confidence_values": memcore::RUBRIC_CONFIDENCE_VALUES,
    }))
}

/// tachi#1675 PR1 D5: `independence_basis` is a MACHINE-COMPUTED disposition,
/// never caller-asserted. On the mirror spine there is no
/// `dispatch_outcomes.vendor` / `identity_attribution_basis` pair to read —
/// this reuses the SAME structural primitives (`is_self_eval` /
/// `cross_model_independent`, both already gating cross_model_independent/
/// self_eval in the response above) as the mirror-spine equivalent of D5's
/// dispatch-spine rule: same known lineage on both sides -> `self`
/// (hard-excluded); different KNOWN lineages -> `structural_cross_vendor`
/// (the only basis eligible for positive routing evidence); anything with an
/// unknown lineage on either side -> `declared_only` (kept as evidence,
/// excluded from positive labels). `identity_bound` is reserved and never
/// returned here — [`memcore::insert_eval_rubric_score`] independently
/// refuses it regardless.
fn compute_independence_basis(producer_lineage: &str, verifier_lineage: &str) -> &'static str {
    if is_self_eval(producer_lineage, verifier_lineage) {
        "self"
    } else if cross_model_independent(producer_lineage, verifier_lineage) {
        "structural_cross_vendor"
    } else {
        "declared_only"
    }
}

#[allow(clippy::too_many_arguments)]
fn write_rubric_score_best_effort(
    server: &MemoryServer,
    adjudication_id: &str,
    adjudicator_actor: &str,
    producer_lineage: &str,
    verifier_lineage: &str,
    rubric: EvalRubricParams,
) -> Option<memcore::EvalRubricScoreRow> {
    let adjudicator_vendor = rubric
        .adjudicator_vendor
        .map(scrub)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| verifier_lineage.to_string());
    let independence_basis = compute_independence_basis(producer_lineage, verifier_lineage);
    let new_score = memcore::NewEvalRubricScore {
        rubric_score_id: uuid::Uuid::new_v4().to_string(),
        adjudication_id: adjudication_id.to_string(),
        subject_kind: "mirror".to_string(),
        rubric_hash: rubric_v1_hash(),
        contract_correctness: scrub(rubric.contract_correctness),
        evidence_quality: scrub(rubric.evidence_quality),
        safety: scrub(rubric.safety),
        scope_discipline: scrub(rubric.scope_discipline),
        intervention_burden: scrub(rubric.intervention_burden),
        completion_integrity: scrub(rubric.completion_integrity),
        adjudication_confidence: scrub(rubric.adjudication_confidence),
        adjudicator_actor: adjudicator_actor.to_string(),
        adjudicator_vendor,
        independence_basis: independence_basis.to_string(),
        occurred_at: memcore::now_utc_iso(),
    };
    let result = server.with_global_store(|store| {
        memcore::insert_eval_rubric_score(store.connection(), &new_score).map_err(|e| e.to_string())
    });
    match result {
        Ok(row) => Some(row),
        Err(err) => {
            tracing::warn!(
                adjudication_id,
                error = %err,
                "tachi#1675 Seam D3: failed to record eval_rubric_scores row"
            );
            None
        }
    }
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

#[cfg(test)]
mod rubric_tests {
    use super::*;

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "agent-eval-rubric-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db_path, None).expect("test memory server")
    }

    fn register(server: &MemoryServer, native_child_id: &str) -> String {
        let params = MirrorEvalRegisterParams {
            frozen_contract_ref: "kckylechen1/tachi#1675".to_string(),
            execution_origin: "host_native_subagent".to_string(),
            lifecycle_owner: "host".to_string(),
            harness: Some("claude_code_task_tool".to_string()),
            native_child_id: Some(native_child_id.to_string()),
            requested_profile: None,
            requested_model: None,
            requested_agent: None,
        };
        let raw = handle_register(server, params).expect("register succeeds");
        let value: Value = serde_json::from_str(&raw).unwrap();
        value["eval_run_id"].as_str().unwrap().to_string()
    }

    fn observe(server: &MemoryServer, eval_run_id: &str, effective_model: &str) {
        let params = MirrorEvalObserveParams {
            eval_run_id: Some(eval_run_id.to_string()),
            native_child_id: None,
            terminal_outcome: "success".to_string(),
            duration_ms: Some(1000),
            cost_tokens: Some(500),
            cost_usd: Some(0.01),
            result_ref: None,
            artifacts: Vec::new(),
            effective_model: Some(effective_model.to_string()),
            effective_backend: None,
            effective_harness: None,
        };
        handle_observe(server, params).expect("observe succeeds");
    }

    fn rubric_block() -> EvalRubricParams {
        EvalRubricParams {
            contract_correctness: "pass".to_string(),
            evidence_quality: "pass".to_string(),
            safety: "pass".to_string(),
            scope_discipline: "pass".to_string(),
            intervention_burden: "not_assessed".to_string(),
            completion_integrity: "pass".to_string(),
            adjudication_confidence: "high".to_string(),
            adjudicator_vendor: None,
        }
    }

    fn adjudicate_params(
        eval_run_id: &str,
        actor: &str,
        verifier_model: Option<&str>,
        rubric: Option<EvalRubricParams>,
    ) -> MirrorEvalAdjudicateParams {
        MirrorEvalAdjudicateParams {
            eval_run_id: Some(eval_run_id.to_string()),
            native_child_id: None,
            actor: actor.to_string(),
            verifier_model: verifier_model.map(str::to_string),
            usefulness: "useful".to_string(),
            failure_mode: None,
            first_review_findings: Vec::new(),
            plan_delta: None,
            next_prompt_delta: None,
            evidence_usable: true,
            used_in_final_claim: true,
            human_override: false,
            evidence_ref: "run-1".to_string(),
            event_key: None,
            rubric,
        }
    }

    fn rubric_row_count(server: &MemoryServer) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM eval_rubric_scores", [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            })
            .unwrap()
    }

    /// A rubric block on `adjudicate` writes an `eval_rubric_scores` row
    /// keyed on the SAME `adjudication_id` the free-text verdict landed
    /// under (subject_kind='mirror' — the only spine this seam wires).
    #[test]
    fn adjudicate_with_rubric_writes_a_rubric_row() {
        let server = test_server();
        let eval_run_id = register(&server, "native-1");
        // tachi_dispatch::model_lineage_id requires a "provider/model" shape
        // (no '/' -> UNKNOWN_IDENTITY, see identity.rs:331-334) — a bare
        // "claude-sonnet-5" here silently collapses BOTH sides to unknown,
        // which is the RED-before-fix bug (both assertions below observed
        // "declared_only" instead of the intended basis).
        observe(&server, &eval_run_id, "anthropic/claude-sonnet");

        let raw = handle_adjudicate(
            &server,
            adjudicate_params(
                &eval_run_id,
                "leader",
                Some("openai/gpt-5"),
                Some(rubric_block()),
            ),
        )
        .expect("adjudicate succeeds");
        let value: Value = serde_json::from_str(&raw).unwrap();
        let adjudication_id = value["adjudication_id"].as_str().unwrap().to_string();
        assert_eq!(value["rubric_written"], serde_json::json!(true));

        let row = server
            .with_global_store_read(|store| {
                memcore::get_eval_rubric_score(store.connection(), "mirror", &adjudication_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .expect("rubric row present");
        assert_eq!(row.subject_kind, "mirror");
        assert_eq!(row.contract_correctness, "pass");
        assert_eq!(row.adjudication_confidence, "high");
        assert_eq!(
            row.independence_basis, "structural_cross_vendor",
            "distinct known producer/verifier lineages -> structural_cross_vendor"
        );
    }

    /// No rubric block -> zero eval_rubric_scores rows, the free-text verdict
    /// still lands normally.
    #[test]
    fn adjudicate_without_rubric_writes_no_rubric_row() {
        let server = test_server();
        let eval_run_id = register(&server, "native-2");
        observe(&server, &eval_run_id, "anthropic/claude-sonnet");

        let raw = handle_adjudicate(
            &server,
            adjudicate_params(&eval_run_id, "leader", Some("openai/gpt-5"), None),
        )
        .expect("adjudicate succeeds");
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["rubric_written"], Value::Null);
        assert_eq!(rubric_row_count(&server), 0);
    }

    /// D5 independence_basis: same known lineage on both sides -> 'self'.
    #[test]
    fn independence_basis_self_when_lineages_match() {
        let server = test_server();
        let eval_run_id = register(&server, "native-3");
        observe(&server, &eval_run_id, "anthropic/claude-sonnet");

        let raw = handle_adjudicate(
            &server,
            adjudicate_params(
                &eval_run_id,
                "leader",
                Some("anthropic/claude-sonnet"),
                Some(rubric_block()),
            ),
        )
        .expect("adjudicate succeeds");
        let value: Value = serde_json::from_str(&raw).unwrap();
        let adjudication_id = value["adjudication_id"].as_str().unwrap().to_string();
        let row = server
            .with_global_store_read(|store| {
                memcore::get_eval_rubric_score(store.connection(), "mirror", &adjudication_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        assert_eq!(row.independence_basis, "self");
    }

    /// D5 independence_basis: an unknown producer lineage (no `observe`
    /// carried an `effective_model`) -> 'declared_only', never a fabricated
    /// cross-vendor claim.
    #[test]
    fn independence_basis_declared_only_when_producer_lineage_unknown() {
        let server = test_server();
        // register WITHOUT a following observe — producer lineage stays unknown.
        let eval_run_id = register(&server, "native-4");

        let raw = handle_adjudicate(
            &server,
            adjudicate_params(
                &eval_run_id,
                "leader",
                Some("openai/gpt-5"),
                Some(rubric_block()),
            ),
        )
        .expect("adjudicate succeeds");
        let value: Value = serde_json::from_str(&raw).unwrap();
        let adjudication_id = value["adjudication_id"].as_str().unwrap().to_string();
        let row = server
            .with_global_store_read(|store| {
                memcore::get_eval_rubric_score(store.connection(), "mirror", &adjudication_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        assert_eq!(row.independence_basis, "declared_only");
    }

    /// Negative test: the rubric write path never touches session_claims or
    /// agent_identities.
    #[test]
    fn rubric_write_never_touches_session_or_identity_tables() {
        let server = test_server();
        let eval_run_id = register(&server, "native-5");
        observe(&server, &eval_run_id, "anthropic/claude-sonnet");

        let count_of = |table: &str| -> i64 {
            server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                        .map_err(|e| e.to_string())
                })
                .unwrap()
        };
        let before_claims = count_of("session_claims");
        let before_identities = count_of("agent_identities");

        handle_adjudicate(
            &server,
            adjudicate_params(
                &eval_run_id,
                "leader",
                Some("openai/gpt-5"),
                Some(rubric_block()),
            ),
        )
        .unwrap();

        assert_eq!(before_claims, count_of("session_claims"));
        assert_eq!(before_identities, count_of("agent_identities"));
    }
}
