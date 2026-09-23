//! `tachi_agent_eval(action='candidate_projection')` — the native
//! dispatch-experience projection, end-to-end through the actual MCP tool
//! surface: register -> observe -> adjudicate (with a `next_prompt_delta`)
//! -> candidate_projection sees it, negative siblings are excluded with
//! counts/reasons, and a retry yields no new samples. Unit-level identity
//! discipline (revision states, alias handling, bounds) is exercised here
//! too because the discriminator IS the handler's response shape.

use super::*;
use crate::tool_params::{
    CandidateProjectionCandidate, CandidateProjectionParams, MirrorEvalAdjudicateParams,
    MirrorEvalObserveParams, MirrorEvalRegisterParams, TachiAgentEvalParams,
};

fn candidate(candidate_id: &str, model: &str, harness: &str) -> CandidateProjectionCandidate {
    CandidateProjectionCandidate {
        candidate_id: candidate_id.to_string(),
        model: model.to_string(),
        // Role-unscoped by default: role scoping has its own dedicated tests
        // below (a scoped role excludes rows whose role was never observed).
        role: None,
        harness: harness.to_string(),
        model_revision: None,
    }
}

async fn projection(server: &crate::MemoryServer, params: CandidateProjectionParams) -> Value {
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            candidate_projection: Some(params),
            ..agent_eval_params("candidate_projection")
        }))
        .await
        .expect("candidate_projection should succeed");
    serde_json::from_str(&resp).expect("candidate_projection JSON")
}

fn agent_eval_params(action: &str) -> TachiAgentEvalParams {
    TachiAgentEvalParams {
        action: action.to_string(),
        fixture_path: None,
        limit: None,
        register: None,
        observe: None,
        adjudicate: None,
        get: None,
        projection: None,
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
/// register + observe + adjudicate through the real tool surface. The
/// observation always carries the OBSERVED identity dimensions so the
/// fixture exercises matching against observed facts (not requested ones).
/// Returns the eval_run_id.
async fn full_run(
    server: &crate::MemoryServer,
    native_id: &str,
    requested_model: &str,
    effective_model: &str,
    effective_harness: Option<&str>,
    verifier_model: Option<&str>,
    usefulness: &str,
    evidence_usable: bool,
    next_prompt_delta: Option<&str>,
    event_key: &str,
) -> String {
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(MirrorEvalRegisterParams {
                frozen_contract_ref: "kckylechen1/tachi#1888".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                harness: Some("claude_code_task_tool".to_string()),
                native_child_id: Some(native_id.to_string()),
                requested_profile: Some("explore".to_string()),
                requested_model: Some(requested_model.to_string()),
                requested_agent: Some("claude".to_string()),
                requested_task_type: None,
                requested_role: None,
            }),
            ..agent_eval_params("register")
        }))
        .await
        .expect("register should succeed");
    let registered: Value = serde_json::from_str(&resp).expect("register JSON");
    let eval_run_id = registered["eval_run_id"].as_str().unwrap().to_string();

    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            observe: Some(MirrorEvalObserveParams {
                eval_run_id: Some(eval_run_id.clone()),
                native_child_id: None,
                terminal_outcome: "success".to_string(),
                duration_ms: Some(2_500),
                cost_tokens: Some(800),
                cost_usd: None,
                result_ref: Some("PR#42".to_string()),
                artifacts: vec!["diff.patch".to_string()],
                effective_model: Some(effective_model.to_string()),
                effective_backend: None,
                effective_harness: effective_harness.map(str::to_string),
                effective_role: None,
                effective_model_revision: None,
            }),
            ..agent_eval_params("observe")
        }))
        .await
        .expect("observe should succeed");

    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(eval_run_id.clone()),
                native_child_id: None,
                actor: "leader".to_string(),
                verifier_model: verifier_model.map(str::to_string),
                usefulness: usefulness.to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: Some("accepted".to_string()),
                next_prompt_delta: next_prompt_delta.map(str::to_string),
                evidence_usable,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence".to_string(),
                event_key: Some(event_key.to_string()),
                rubric: None,
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("adjudicate should succeed");

    eval_run_id
}

fn table_count(server: &crate::MemoryServer, table: &str) -> i64 {
    server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(|e| e.to_string())
        })
        .unwrap()
}

/// End-to-end: the projection sees a register -> observe -> adjudicate run
/// WITH its whole next_prompt_delta, excludes the negative siblings with
/// counts and reasons, is read-only, and a retry adds no samples.
#[tokio::test]
async fn candidate_projection_sees_verified_run_and_excludes_negatives() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );
    let advisory = "Ask the worker to quote the failing test name verbatim before \
                    proposing a fix, and to name the file it changed.";

    let eligible = full_run(
        &server,
        "cand-eligible",
        "anthropic/claude-sonnet",
        "anthropic/claude-sonnet",
        Some("claude_code_task_tool"),
        Some("openai/gpt-5"),
        "useful",
        true,
        Some(advisory),
        "eligible-event",
    )
    .await;

    // Negative siblings, each failing exactly one discriminator.
    let _not_usable = full_run(
        &server,
        "cand-not-usable",
        "anthropic/claude-sonnet",
        "anthropic/claude-sonnet",
        Some("claude_code_task_tool"),
        Some("openai/gpt-5"),
        "failed",
        false,
        Some("should never surface"),
        "not-usable-event",
    )
    .await;

    let _self_eval = full_run(
        &server,
        "cand-self-eval",
        "anthropic/claude-sonnet",
        "anthropic/claude-sonnet",
        Some("claude_code_task_tool"),
        Some("anthropic/claude-sonnet"),
        "useful",
        true,
        Some("self-praise never surfaces"),
        "self-eval-event",
    )
    .await;

    let before_runs = table_count(&server, "mirror_eval_runs");
    let before_adjudications = table_count(&server, "mirror_eval_adjudications");
    let before_route_decisions = table_count(&server, "route_decisions");
    let before_route_recommendations = table_count(&server, "route_recommendations");

    let payload = projection(
        &server,
        CandidateProjectionParams {
            // Unscoped by task on purpose: these fixtures register without a
            // requested_task_type, and a task-scoped query must (and does,
            // see task_scoping_uses_register_requested_task_type) exclude
            // them as task_type_unrecorded rather than count them.
            task_type: None,
            window_days: Some(365),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
        },
    )
    .await;

    assert_eq!(payload["evidence_source"], json!("mirror_eval_lifecycle"));
    assert_eq!(payload["advisory_only"], json!(true));
    assert_eq!(payload["launch_authority"], json!(false));
    assert_eq!(payload["ranking"], json!(false));
    assert_eq!(payload["read_only"], json!(true));
    assert_eq!(payload["task_type"]["requested"], Value::Null);
    assert_eq!(payload["task_type"]["rows_recording_task_type"], json!(0));

    let cand = &payload["candidates"][0];
    assert_eq!(cand["candidate_id"], json!("c1"));
    assert_eq!(cand["adjudication_status"], json!("verified"));
    assert_eq!(cand["samples_total"], json!(1));
    assert_eq!(cand["samples"].as_array().unwrap().len(), 1);

    let sample = &cand["samples"][0];
    assert_eq!(sample["eval_run_id"], json!(eligible));
    assert_eq!(
        sample["frozen_contract_ref"],
        json!("kckylechen1/tachi#1888")
    );
    assert_eq!(
        sample["next_prompt_delta"],
        json!(advisory),
        "the advisory text is returned WHOLE"
    );
    assert_eq!(sample["next_prompt_delta_omitted_reason"], Value::Null);
    assert_eq!(sample["identity_basis"]["model"], json!("observed"));
    assert_eq!(sample["identity_basis"]["harness"], json!("observed"));
    // No effective_role observed in this fixture: role stays visibly
    // unrecorded — never confirmed from requested_profile/requested_role.
    assert_eq!(sample["identity_basis"]["role"], json!("unrecorded"));
    assert_eq!(
        sample["identity"]["model"],
        json!("anthropic/claude-sonnet")
    );
    assert_eq!(
        sample["adjudication"]["verifier_model"],
        json!("openai/gpt-5")
    );
    assert_eq!(sample["adjudication"]["actor"], json!("leader"));
    assert_eq!(sample["observation_refs"]["result_ref"], json!("PR#42"));

    // Negative siblings excluded with counts and reasons.
    assert_eq!(cand["excluded_counts"]["evidence_not_usable"], json!(1));
    assert_eq!(cand["excluded_counts"]["self_eval"], json!(1));
    assert_eq!(cand["excluded_counts"]["not_observed"], json!(0));
    // No invented score anywhere in the candidate block.
    let cand_dump = cand.to_string();
    assert!(
        !cand_dump.contains("\"score\""),
        "no score key may appear: {cand_dump}"
    );

    // Retry: same evidence, no new samples.
    let again = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
        },
    )
    .await;
    assert_eq!(again["candidates"][0]["samples_total"], json!(1));
    assert_eq!(
        again["candidates"][0]["samples"][0]["eval_run_id"],
        json!(eligible)
    );

    // Read-only: nothing was written by either projection call.
    assert_eq!(before_runs, table_count(&server, "mirror_eval_runs"));
    assert_eq!(
        before_adjudications,
        table_count(&server, "mirror_eval_adjudications")
    );
    assert_eq!(
        before_route_decisions,
        table_count(&server, "route_decisions")
    );
    assert_eq!(
        before_route_recommendations,
        table_count(&server, "route_recommendations")
    );
}

/// Unadjudicated and never-observed runs are excluded with reasons; an
/// unknown-identity candidate reports insufficient instead of anything
/// invented.
#[tokio::test]
async fn unadjudicated_unobserved_and_unknown_candidates_report_honestly() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // Registered + observed, never adjudicated.
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(MirrorEvalRegisterParams {
                frozen_contract_ref: "kckylechen1/tachi#1888".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                harness: Some("claude_code_task_tool".to_string()),
                native_child_id: Some("only-observed".to_string()),
                requested_profile: Some("explore".to_string()),
                requested_model: Some("anthropic/claude-sonnet".to_string()),
                requested_agent: Some("claude".to_string()),
                requested_task_type: None,
                requested_role: None,
            }),
            ..agent_eval_params("register")
        }))
        .await
        .expect("register should succeed");
    let observed_only: Value = serde_json::from_str(&resp).expect("register JSON");
    let observed_only_id = observed_only["eval_run_id"].as_str().unwrap().to_string();
    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            observe: Some(MirrorEvalObserveParams {
                eval_run_id: Some(observed_only_id.clone()),
                native_child_id: None,
                terminal_outcome: "success".to_string(),
                duration_ms: None,
                cost_tokens: None,
                cost_usd: None,
                result_ref: None,
                artifacts: Vec::new(),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                effective_backend: None,
                effective_harness: Some("claude_code_task_tool".to_string()),
                effective_role: None,
                effective_model_revision: None,
            }),
            ..agent_eval_params("observe")
        }))
        .await
        .expect("observe should succeed");

    // Registered only — never observed.
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(MirrorEvalRegisterParams {
                frozen_contract_ref: "kckylechen1/tachi#1888".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                harness: Some("claude_code_task_tool".to_string()),
                native_child_id: Some("register-only".to_string()),
                requested_profile: Some("explore".to_string()),
                requested_model: Some("anthropic/claude-sonnet".to_string()),
                requested_agent: Some("claude".to_string()),
                requested_task_type: None,
                requested_role: None,
            }),
            ..agent_eval_params("register")
        }))
        .await
        .expect("register should succeed");
    let _register_only: Value = serde_json::from_str(&resp).expect("register JSON");

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![
                candidate("known", "anthropic/claude-sonnet", "claude_code_task_tool"),
                candidate(
                    "unknown",
                    "zhipuai-coding-plan/glm-9",
                    "claude_code_task_tool",
                ),
            ],
        },
    )
    .await;

    let known = &payload["candidates"][0];
    assert_eq!(known["adjudication_status"], json!("insufficient"));
    assert_eq!(known["compatibility_status"], json!("insufficient"));
    assert_eq!(known["samples_total"], json!(0));
    assert_eq!(known["excluded_counts"]["unadjudicated"], json!(1));
    assert_eq!(known["excluded_counts"]["not_observed"], json!(1));

    let unknown = &payload["candidates"][1];
    assert_eq!(unknown["adjudication_status"], json!("insufficient"));
    assert_eq!(unknown["samples_total"], json!(0));
    assert_eq!(unknown["samples"].as_array().unwrap().len(), 0);
    assert_eq!(unknown["excluded_counts"]["unadjudicated"], json!(1));
    assert_eq!(unknown["excluded_counts"]["not_observed"], json!(1));
    assert_eq!(unknown["excluded_counts"]["model_mismatch"], json!(0));
}

/// Identity discipline: requested identity is NEVER matched (a run whose
/// requested model differs but whose observed model matches, matches the
/// OBSERVED identity and nothing else), requested-but-unobserved identity
/// excludes as identity_unobserved, and a run without an observed harness
/// excludes as harness_effective_unobserved rather than inheriting the
/// register-time harness.
#[tokio::test]
async fn identity_matches_observed_only_never_requested() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // Observed model DIFFERS from the requested one: only the observed one
    // may match, and no effective_harness was observed. The verifier is a
    // different lineage from the observed producer so the run is NOT
    // self-eval — each candidate below must fail on its OWN discriminator.
    let aliased = full_run(
        &server,
        "alias-run",
        "anthropic/claude-sonnet",
        "openai/gpt-5",
        None,
        Some("anthropic/claude-sonnet"),
        "useful",
        true,
        Some("observed identity wins"),
        "alias-event",
    )
    .await;

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![
                // Same string as the run's REQUESTED model: must NOT match —
                // the observed model is openai/gpt-5 and the run also lacks
                // an observed harness.
                candidate(
                    "requested-lookalike",
                    "anthropic/claude-sonnet",
                    "claude_code_task_tool",
                ),
                // Observed model, but the run never observed a harness.
                candidate(
                    "no-harness-observed",
                    "openai/gpt-5",
                    "claude_code_task_tool",
                ),
            ],
        },
    )
    .await;

    let lookalike = &payload["candidates"][0];
    assert_eq!(lookalike["adjudication_status"], json!("insufficient"));
    assert_eq!(lookalike["excluded_counts"]["model_mismatch"], json!(1));
    assert!(
        lookalike["samples"].as_array().unwrap().is_empty(),
        "requested identity must never match: {lookalike}"
    );

    let no_harness = &payload["candidates"][1];
    assert_eq!(no_harness["adjudication_status"], json!("insufficient"));
    assert_eq!(
        no_harness["excluded_counts"]["harness_effective_unobserved"],
        json!(1),
        "a register-time harness must never confirm an effective harness"
    );
    let _ = aliased;
}

/// Revision discipline: an explicit revision is confirmed only by the
/// observed `@version` suffix; a different observed suffix excludes; a
/// missing suffix stays unresolved (historical), never version-confirmed.
#[tokio::test]
async fn model_revision_is_confirmed_unresolved_or_mismatched_never_guessed() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    let pinned = full_run(
        &server,
        "pinned-run",
        "openai/gpt-5",
        "openai/gpt-5@2026-03-10",
        Some("claude_code_task_tool"),
        Some("anthropic/claude-sonnet"),
        "useful",
        true,
        Some("pinned advisory"),
        "pinned-event",
    )
    .await;
    let unpinned = full_run(
        &server,
        "unpinned-run",
        "anthropic/claude-sonnet",
        "anthropic/claude-sonnet",
        Some("claude_code_task_tool"),
        Some("openai/gpt-5"),
        "useful",
        true,
        Some("unpinned advisory"),
        "unpinned-event",
    )
    .await;

    let mut revision_candidate = candidate("rev", "openai/gpt-5", "claude_code_task_tool");
    revision_candidate.model_revision = Some("2026-03-10".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![revision_candidate],
        },
    )
    .await;
    let rev = &payload["candidates"][0];
    assert_eq!(rev["adjudication_status"], json!("verified"));
    assert_eq!(rev["samples_total"], json!(1));
    assert_eq!(rev["samples"][0]["eval_run_id"], json!(pinned));
    assert_eq!(
        rev["samples"][0]["identity"]["model_revision"],
        json!("2026-03-10")
    );
    assert_eq!(
        rev["samples"][0]["identity_basis"]["model_revision"],
        json!("confirmed")
    );
    assert_eq!(
        rev["samples"][0]["identity_basis"]["model_revision_basis"],
        json!("legacy_model_suffix"),
        "this fixture carries the revision on the model string, not the v34 column"
    );
    assert_eq!(rev["model_revision_status_counts"]["confirmed"], json!(1));

    // A DIFFERENT explicit revision must not collect the pinned run's
    // evidence (distinct observed revision, excluded) nor silently absorb
    // the unpinned run as if it were confirmed.
    let mut wrong_revision = candidate("rev-wrong", "openai/gpt-5", "claude_code_task_tool");
    wrong_revision.model_revision = Some("2026-09-01".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![wrong_revision],
        },
    )
    .await;
    let wrong = &payload["candidates"][0];
    assert_eq!(wrong["adjudication_status"], json!("insufficient"));
    assert_eq!(wrong["excluded_counts"]["revision_mismatch"], json!(1));
    assert_eq!(wrong["samples_total"], json!(0));

    // No revision declared: the unpinned run (observed without a suffix) is
    // eligible but visibly `unrecorded`, and the pinned run's distinct
    // observed revision is surfaced, never merged away.
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![
                candidate("plain-openai", "openai/gpt-5", "claude_code_task_tool"),
                candidate(
                    "plain-anthropic",
                    "anthropic/claude-sonnet",
                    "claude_code_task_tool",
                ),
            ],
        },
    )
    .await;
    let plain_openai = &payload["candidates"][0];
    assert_eq!(plain_openai["adjudication_status"], json!("verified"));
    assert_eq!(
        plain_openai["samples"][0]["identity_basis"]["model_revision"],
        json!("observed"),
        "the run carries an observed @version; an unscoped query reports it observed"
    );
    assert_eq!(
        plain_openai["model_revision_status_counts"]["observed"],
        json!(1)
    );
    assert_eq!(
        plain_openai["distinct_observed_revisions"],
        json!(["2026-03-10"]),
        "the observed revision is surfaced, not silently merged"
    );
    let plain_anthropic = &payload["candidates"][1];
    assert_eq!(plain_anthropic["adjudication_status"], json!("verified"));
    assert_eq!(
        plain_anthropic["samples"][0]["eval_run_id"],
        json!(unpinned)
    );
    assert_eq!(
        plain_anthropic["samples"][0]["identity_basis"]["model_revision"],
        json!("unrecorded"),
        "no @version on the observed model and none requested: unrecorded, never confirmed"
    );
    assert_eq!(
        plain_anthropic["model_revision_status_counts"]["unrecorded"],
        json!(1)
    );
}

/// Corrections append: the CURRENT adjudication is the sample, the
/// superseded advisory is preserved separately, and an idempotent
/// event_key replay creates no second sample.
#[tokio::test]
async fn correction_keeps_one_sample_and_preserves_historical_advisory() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );
    let run = full_run(
        &server,
        "corrected-run",
        "anthropic/claude-sonnet",
        "anthropic/claude-sonnet",
        Some("claude_code_task_tool"),
        Some("openai/gpt-5"),
        "partially_useful",
        true,
        Some("older advisory: request the failing command output"),
        "first-event",
    )
    .await;

    // Correction: NEW event_key appends.
    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(run.clone()),
                native_child_id: None,
                actor: "leader".to_string(),
                verifier_model: Some("openai/gpt-5".to_string()),
                usefulness: "useful".to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: Some("accepted".to_string()),
                next_prompt_delta: Some(
                    "newer advisory: require a discriminator test run before claiming the fix"
                        .to_string(),
                ),
                evidence_usable: true,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence-2".to_string(),
                event_key: Some("second-event".to_string()),
                rubric: None,
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("correction should succeed");

    // Idempotent replay of the SAME event_key: no new event.
    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(run.clone()),
                native_child_id: None,
                actor: "leader".to_string(),
                verifier_model: Some("openai/gpt-5".to_string()),
                usefulness: "useful".to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: Some("accepted".to_string()),
                next_prompt_delta: Some(
                    "newer advisory: require a discriminator test run before claiming the fix"
                        .to_string(),
                ),
                evidence_usable: true,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence-2".to_string(),
                event_key: Some("second-event".to_string()),
                rubric: None,
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("idempotent replay should succeed");

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
        },
    )
    .await;
    let cand = &payload["candidates"][0];
    assert_eq!(cand["samples_total"], json!(1), "one sample per run, ever");
    let sample = &cand["samples"][0];
    assert_eq!(sample["adjudication"]["event_count"], json!(2));
    assert_eq!(sample["adjudication"]["is_overturn"], json!(true));
    assert_eq!(sample["outcome"]["usefulness"], json!("useful"));
    assert!(
        sample["next_prompt_delta"]
            .as_str()
            .is_some_and(|text| text.starts_with("newer advisory")),
        "the CURRENT adjudication's advisory is the sample's advisory"
    );
    let historical = sample["historical_advisories"].as_array().unwrap();
    assert_eq!(historical.len(), 1, "the superseded advisory is preserved");
    assert_eq!(historical[0]["superseded"], json!(true));
    assert!(historical[0]["next_prompt_delta"]
        .as_str()
        .is_some_and(|text| text.starts_with("older advisory")));
}

/// Bounded input contract: 0 and 7 candidates, missing model/harness, and
/// duplicate ids are explicit errors.
#[tokio::test]
async fn candidate_set_bounds_and_required_fields_are_enforced() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    let cases: Vec<(&str, CandidateProjectionParams, &str)> = vec![
        (
            "empty candidate set",
            CandidateProjectionParams {
                candidates: Vec::new(),
                ..Default::default()
            },
            "1..=6",
        ),
        (
            "missing model",
            CandidateProjectionParams {
                candidates: vec![CandidateProjectionCandidate {
                    candidate_id: "c".to_string(),
                    model: String::new(),
                    role: None,
                    harness: "claude_code_task_tool".to_string(),
                    model_revision: None,
                }],
                ..Default::default()
            },
            "model identity is required",
        ),
        (
            "missing harness",
            CandidateProjectionParams {
                candidates: vec![CandidateProjectionCandidate {
                    candidate_id: "c".to_string(),
                    model: "openai/gpt-5".to_string(),
                    role: None,
                    harness: "   ".to_string(),
                    model_revision: None,
                }],
                ..Default::default()
            },
            "harness is required",
        ),
        (
            "duplicate candidate_id",
            CandidateProjectionParams {
                candidates: vec![
                    candidate("dup", "openai/gpt-5", "claude_code_task_tool"),
                    candidate("dup", "anthropic/claude-sonnet", "claude_code_task_tool"),
                ],
                ..Default::default()
            },
            "duplicate candidate_id",
        ),
    ];
    for (name, params, expected) in cases {
        let err = server
            .tachi_agent_eval(Parameters(TachiAgentEvalParams {
                candidate_projection: Some(params),
                ..agent_eval_params("candidate_projection")
            }))
            .await
            .expect_err(name);
        assert!(err.contains(expected), "{name}: unexpected error {err}");
    }

    // Seven candidates is over the bound.
    let seven: Vec<CandidateProjectionCandidate> = (0..7)
        .map(|index| {
            candidate(
                &format!("c{index}"),
                "openai/gpt-5",
                "claude_code_task_tool",
            )
        })
        .collect();
    let err = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            candidate_projection: Some(CandidateProjectionParams {
                candidates: seven,
                ..Default::default()
            }),
            ..agent_eval_params("candidate_projection")
        }))
        .await
        .expect_err("seven candidates must be rejected");
    assert!(err.contains("at most 6"), "unexpected error: {err}");

    // Six candidates is the documented bound and succeeds on an empty ledger.
    let six: Vec<CandidateProjectionCandidate> = (0..6)
        .map(|index| {
            candidate(
                &format!("c{index}"),
                "openai/gpt-5",
                "claude_code_task_tool",
            )
        })
        .collect();
    let payload = projection(
        &server,
        CandidateProjectionParams {
            candidates: six,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(payload["candidates_supplied"], json!(6));
    assert_eq!(payload["provisional_limits"]["max_candidates"], json!(6));
    for cand in payload["candidates"].as_array().unwrap() {
        assert_eq!(cand["adjudication_status"], json!("insufficient"));
    }
}

/// A helper for the v34 tests: register with the requested task/role, observe
/// with the OBSERVED identity dimensions (role/revision), adjudicate usable.
#[allow(clippy::too_many_arguments)]
async fn v34_run(
    server: &crate::MemoryServer,
    native_id: &str,
    requested_task_type: Option<&str>,
    requested_role: Option<&str>,
    effective_model: &str,
    effective_model_revision: Option<&str>,
    effective_role: Option<&str>,
    event_key: &str,
) -> String {
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(MirrorEvalRegisterParams {
                frozen_contract_ref: "kckylechen1/tachi#1888".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                harness: Some("claude_code_task_tool".to_string()),
                native_child_id: Some(native_id.to_string()),
                requested_profile: Some("explore".to_string()),
                requested_model: Some("anthropic/claude-sonnet".to_string()),
                requested_agent: Some("claude".to_string()),
                requested_task_type: requested_task_type.map(str::to_string),
                requested_role: requested_role.map(str::to_string),
            }),
            ..agent_eval_params("register")
        }))
        .await
        .expect("register should succeed");
    let registered: Value = serde_json::from_str(&resp).expect("register JSON");
    let eval_run_id = registered["eval_run_id"].as_str().unwrap().to_string();

    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            observe: Some(MirrorEvalObserveParams {
                eval_run_id: Some(eval_run_id.clone()),
                native_child_id: None,
                terminal_outcome: "success".to_string(),
                duration_ms: None,
                cost_tokens: None,
                cost_usd: None,
                result_ref: None,
                artifacts: Vec::new(),
                effective_model: Some(effective_model.to_string()),
                effective_backend: None,
                effective_harness: Some("claude_code_task_tool".to_string()),
                effective_role: effective_role.map(str::to_string),
                effective_model_revision: effective_model_revision.map(str::to_string),
            }),
            ..agent_eval_params("observe")
        }))
        .await
        .expect("observe should succeed");

    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(eval_run_id.clone()),
                native_child_id: None,
                actor: "leader".to_string(),
                // A lineage distinct from every fixture producer model, so
                // the runs are never self-eval and each test's discriminator
                // is the one under test.
                verifier_model: Some("zhipuai-coding-plan/glm-5".to_string()),
                usefulness: "useful".to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: None,
                next_prompt_delta: Some("v34 advisory".to_string()),
                evidence_usable: true,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence".to_string(),
                event_key: Some(event_key.to_string()),
                rubric: None,
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("adjudicate should succeed");
    eval_run_id
}

/// Role confirmation uses ONLY the observed effective_role: a requested_role
/// that differs from the observed one never confirms or denies anything, an
/// unobserved role excludes (visible, historical) instead of counting
/// compatible, and an explicitly different observed role is a mismatch.
#[tokio::test]
async fn role_confirmation_requires_observed_effective_role() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // requested_role deliberately DIFFERS from the observed role: only the
    // observed one may confirm.
    let confirmed = v34_run(
        &server,
        "v34-role-ok",
        None,
        Some("requested-impl"),
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "role-ok-event",
    )
    .await;
    // Legacy run: observed before effective_role existed.
    let legacy = v34_run(
        &server,
        "v34-role-legacy",
        None,
        Some("implementation"),
        "anthropic/claude-sonnet",
        None,
        None,
        "role-legacy-event",
    )
    .await;

    let mut impl_candidate = candidate("impl", "anthropic/claude-sonnet", "claude_code_task_tool");
    impl_candidate.role = Some("implementation".to_string());
    let mut wrong_role = candidate(
        "explorer",
        "anthropic/claude-sonnet",
        "claude_code_task_tool",
    );
    wrong_role.role = Some("explorer".to_string());
    let mut requested_role_lookalike = candidate(
        "lookalike",
        "anthropic/claude-sonnet",
        "claude_code_task_tool",
    );
    requested_role_lookalike.role = Some("requested-impl".to_string());

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![impl_candidate, wrong_role, requested_role_lookalike],
        },
    )
    .await;

    let impl_cand = &payload["candidates"][0];
    assert_eq!(impl_cand["adjudication_status"], json!("verified"));
    assert_eq!(impl_cand["samples_total"], json!(1));
    assert_eq!(impl_cand["samples"][0]["eval_run_id"], json!(confirmed));
    assert_eq!(
        impl_cand["samples"][0]["identity"]["role"],
        json!("implementation")
    );
    assert_eq!(
        impl_cand["samples"][0]["identity_basis"]["role"],
        json!("observed")
    );
    assert_eq!(
        impl_cand["excluded_counts"]["role_unobserved"],
        json!(1),
        "the legacy run (no observed role) is excluded with a reason, never compatible"
    );

    let wrong = &payload["candidates"][1];
    assert_eq!(wrong["adjudication_status"], json!("insufficient"));
    assert_eq!(wrong["excluded_counts"]["role_mismatch"], json!(1));
    assert_eq!(wrong["excluded_counts"]["role_unobserved"], json!(1));

    // A candidate role equal to the run's REQUESTED role is NOT confirmation:
    // the observed role is 'implementation', so this is a mismatch.
    let lookalike = &payload["candidates"][2];
    assert_eq!(lookalike["adjudication_status"], json!("insufficient"));
    assert_eq!(
        lookalike["excluded_counts"]["role_mismatch"],
        json!(1),
        "requested_role must never act as a role fallback"
    );
    let _ = legacy;
}

/// Task scoping keys on the register-time requested_task_type under an
/// explicit register_requested basis; unrecorded tasks exclude with counts,
/// and mismatches are never compatible advice.
#[tokio::test]
async fn task_scoping_uses_register_requested_task_type() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    let fixed = v34_run(
        &server,
        "v34-task-fix",
        Some("fix_request"),
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "task-fix-event",
    )
    .await;
    let unrecorded = v34_run(
        &server,
        "v34-task-none",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "task-none-event",
    )
    .await;

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: Some("fix_request".to_string()),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
            window_days: Some(365), // deterministic window under the fixed cutoff
        },
    )
    .await;
    assert_eq!(payload["task_type"]["requested"], json!("fix_request"));
    assert_eq!(payload["task_type"]["basis"], json!("register_requested"));
    assert_eq!(payload["task_type"]["rows_recording_task_type"], json!(1));

    let cand = &payload["candidates"][0];
    assert_eq!(cand["adjudication_status"], json!("verified"));
    assert_eq!(cand["samples"][0]["eval_run_id"], json!(fixed));
    assert_eq!(
        cand["samples"][0]["identity"]["task_type"],
        json!("fix_request")
    );
    assert_eq!(
        cand["samples"][0]["identity_basis"]["task_type"],
        json!("register_requested")
    );
    assert_eq!(
        cand["excluded_counts"]["task_type_unrecorded"],
        json!(1),
        "the unrecorded-task run is excluded with a reason, never compatible"
    );

    // An explicitly different task is a mismatch for every run.
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: Some("review_request".to_string()),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
            window_days: Some(365), // deterministic window under the fixed cutoff
        },
    )
    .await;
    let cand = &payload["candidates"][0];
    assert_eq!(cand["adjudication_status"], json!("insufficient"));
    assert_eq!(cand["excluded_counts"]["task_type_mismatch"], json!(1));
    assert_eq!(cand["excluded_counts"]["task_type_unrecorded"], json!(1));
    let _ = unrecorded;
}

/// The v34 revision column is authoritative; the legacy model-string suffix
/// is a fallback only where the column is absent.
#[tokio::test]
async fn revision_column_is_authoritative_suffix_is_legacy_fallback() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // Column-only: no @suffix on the model string at all.
    let column_run = v34_run(
        &server,
        "v34-rev-column",
        None,
        None,
        "openai/gpt-5",
        Some("2026-03-10"),
        Some("implementation"),
        "rev-column-event",
    )
    .await;

    let mut revision_candidate = candidate("rev", "openai/gpt-5", "claude_code_task_tool");
    revision_candidate.model_revision = Some("2026-03-10".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            candidates: vec![revision_candidate],
            task_type: None,
            window_days: Some(365),
        },
    )
    .await;
    let rev = &payload["candidates"][0];
    assert_eq!(rev["adjudication_status"], json!("verified"));
    assert_eq!(rev["samples"][0]["eval_run_id"], json!(column_run));
    assert_eq!(
        rev["samples"][0]["identity_basis"]["model_revision"],
        json!("confirmed")
    );
    assert_eq!(
        rev["samples"][0]["identity_basis"]["model_revision_basis"],
        json!("observed_column")
    );

    // A DIFFERENT explicit revision mismatches the column-authoritative run.
    let mut wrong = candidate("rev-wrong", "openai/gpt-5", "claude_code_task_tool");
    wrong.model_revision = Some("2026-09-01".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            candidates: vec![wrong],
            task_type: None,
            window_days: Some(365),
        },
    )
    .await;
    assert_eq!(
        payload["candidates"][0]["adjudication_status"],
        json!("insufficient")
    );
    assert_eq!(
        payload["candidates"][0]["excluded_counts"]["revision_mismatch"],
        json!(1)
    );
}

/// A candidate model string's @suffix IS its declared revision when the
/// separate field is omitted (no silent discard), and disagreeing
/// representations fail validation. NEW observe inputs carrying conflicting
/// revision representations are rejected before any write.
#[tokio::test]
async fn model_suffix_declares_revision_and_conflicts_are_refused() {
    let server = make_server();
    // Deterministic time fixture: pin the projection instant to a fixed cutoff so the
    // register->adjudicate->project flow is deterministic (real-clock reads
    // could land in the same millisecond as the writes and flake under the
    // exact [since, generated_at) bound). Fixture rows are written at real
    // 2026 time, strictly before the 2027-01-01 cutoff; window_days 365
    // keeps them inside [since, cutoff).
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // Observed revision 2026-03-10 (column). A candidate whose model string
    // pins @2026-09-01 with no separate field must NOT collect it.
    let pinned = v34_run(
        &server,
        "v34-suffix-run",
        None,
        None,
        "openai/gpt-5",
        Some("2026-03-10"),
        Some("implementation"),
        "suffix-run-event",
    )
    .await;
    let suffix_only = candidate("suffix", "openai/gpt-5@2026-09-01", "claude_code_task_tool");
    let payload = projection(
        &server,
        CandidateProjectionParams {
            candidates: vec![suffix_only],
            task_type: None,
            window_days: Some(365),
        },
    )
    .await;
    let cand = &payload["candidates"][0];
    assert_eq!(
        cand["adjudication_status"],
        json!("insufficient"),
        "the suffix-declared revision must act as the declared revision, not be discarded"
    );
    assert_eq!(cand["excluded_counts"]["revision_mismatch"], json!(1));

    // Agreeing suffix + separate field is fine.
    let mut agreeing = candidate("agree", "openai/gpt-5@2026-03-10", "claude_code_task_tool");
    agreeing.model_revision = Some("2026-03-10".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            candidates: vec![agreeing],
            task_type: None,
            window_days: Some(365),
        },
    )
    .await;
    assert_eq!(
        payload["candidates"][0]["adjudication_status"],
        json!("verified")
    );
    assert_eq!(
        payload["candidates"][0]["samples"][0]["eval_run_id"],
        json!(pinned)
    );

    // Disagreeing suffix + separate field fails validation outright.
    let mut disagree = candidate(
        "disagree",
        "openai/gpt-5@2026-09-01",
        "claude_code_task_tool",
    );
    disagree.model_revision = Some("2026-03-10".to_string());
    let err = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            candidate_projection: Some(CandidateProjectionParams {
                candidates: vec![disagree],
                ..Default::default()
            }),
            ..agent_eval_params("candidate_projection")
        }))
        .await
        .expect_err("disagreeing revision representations must fail validation");
    assert!(err.contains("conflicts with"), "got: {err}");

    // NEW observe inputs with conflicting representations are rejected and
    // write nothing.
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(MirrorEvalRegisterParams {
                frozen_contract_ref: "kckylechen1/tachi#1888".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                harness: Some("claude_code_task_tool".to_string()),
                native_child_id: Some("v34-observe-conflict".to_string()),
                requested_profile: None,
                requested_model: None,
                requested_agent: None,
                requested_task_type: None,
                requested_role: None,
            }),
            ..agent_eval_params("register")
        }))
        .await
        .expect("register should succeed");
    let registered: Value = serde_json::from_str(&resp).expect("register JSON");
    let conflict_run_id = registered["eval_run_id"].as_str().unwrap().to_string();
    let err = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            observe: Some(MirrorEvalObserveParams {
                eval_run_id: Some(conflict_run_id.clone()),
                native_child_id: None,
                terminal_outcome: "success".to_string(),
                duration_ms: None,
                cost_tokens: None,
                cost_usd: None,
                result_ref: None,
                artifacts: Vec::new(),
                effective_model: Some("openai/gpt-5@2026-03-10".to_string()),
                effective_backend: None,
                effective_harness: Some("claude_code_task_tool".to_string()),
                effective_role: None,
                effective_model_revision: Some("2026-09-01".to_string()),
            }),
            ..agent_eval_params("observe")
        }))
        .await
        .expect_err("conflicting revision representations must be refused at observe");
    assert!(err.contains("conflicting model revision"), "got: {err}");
    let observations: i64 = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM mirror_eval_observations WHERE eval_run_id = ?1",
                    [&conflict_run_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_eq!(observations, 0, "the refused observation must not land");
}

/// Fixture helper: move a stored fact's timestamp
/// to a fixed instant relative to the pinned 2027-01-01T00:00:00.000Z
/// cutoff (deterministic constants — no sleeps, no clock reads).
fn force_created_at(server: &crate::MemoryServer, table: &str, eval_run_id: &str, ts: &str) {
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    &format!("UPDATE {table} SET created_at = ?1 WHERE eval_run_id = ?2"),
                    rusqlite::params![ts, eval_run_id],
                )
                .map_err(|e| e.to_string())
        })
        .expect("force fixture timestamp");
}

/// A candidate that DECLARES a model revision, matched
/// against an otherwise fully eligible run observed WITHOUT any revision,
/// must report adjudication verified but compatibility UNRESOLVED — never
/// an unqualified "verified". The historical advisory stays available,
/// whole, and labeled.
#[tokio::test]
async fn revision_unresolved_evidence_is_historical_not_verified_compatibility() {
    let server = make_server();
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // Eligible on every hard gate, but observed with NO revision (no
    // effective_model_revision, no @suffix).
    let historical = v34_run(
        &server,
        "bug1-historical",
        Some("fix_request"),
        Some("implementation"),
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug1-historical-event",
    )
    .await;

    let mut pinned = candidate("c1", "anthropic/claude-sonnet", "claude_code_task_tool");
    pinned.role = Some("implementation".to_string());
    pinned.model_revision = Some("2026-03-10".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: Some("fix_request".to_string()),
            window_days: Some(365),
            candidates: vec![pinned],
        },
    )
    .await;

    let cand = &payload["candidates"][0];
    assert_eq!(cand["adjudication_status"], json!("verified"));
    assert_eq!(
        cand["compatibility_status"],
        json!("unresolved"),
        "a declared revision with no observed revision is historical, never confirmed"
    );
    assert_eq!(cand["confirmed_compatible_samples"], json!(0));
    assert_eq!(cand["unresolved_samples"], json!(1));
    assert_eq!(cand["samples_total"], json!(1));
    let sample = &cand["samples"][0];
    assert_eq!(sample["eval_run_id"], json!(historical));
    assert_eq!(sample["compatibility"]["overall"], json!("unresolved"));
    assert_eq!(
        sample["compatibility"]["model_revision"],
        json!("unresolved")
    );
    assert_eq!(sample["compatibility"]["role"], json!("confirmed"));
    assert_eq!(sample["compatibility"]["task_type"], json!("confirmed"));
    assert_eq!(
        sample["identity_basis"]["model_revision"],
        json!("unresolved")
    );
    // The historical advisory is still whole and labeled.
    assert_eq!(sample["next_prompt_delta"], json!("v34 advisory"));
    assert!(payload["advisory_only"] == json!(true));
    // No unqualified verified anywhere: the old `evidence_status` key is gone.
    assert_eq!(cand.get("evidence_status"), None);
}

/// Full confirmation requires EVERY dimension scoped and
/// matching; a mix of confirmed and unresolved rows is explicitly `mixed`;
/// dimensions the query left unscoped are labeled, never confirmed.
#[tokio::test]
async fn compatibility_fully_confirmed_requires_every_dimension_and_mixed_is_explicit() {
    let server = make_server();
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    let confirmed = v34_run(
        &server,
        "bug1-confirmed",
        Some("fix_request"),
        None,
        "anthropic/claude-sonnet",
        Some("2026-03-10"),
        Some("implementation"),
        "bug1-confirmed-event",
    )
    .await;
    // Same identity but observed with no revision: unresolved for a
    // revision-pinned query.
    let historical = v34_run(
        &server,
        "bug1-legacy",
        Some("fix_request"),
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug1-legacy-event",
    )
    .await;

    // Fully scoped query: role + task + revision all declared.
    let mut pinned = candidate("c1", "anthropic/claude-sonnet", "claude_code_task_tool");
    pinned.role = Some("implementation".to_string());
    pinned.model_revision = Some("2026-03-10".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: Some("fix_request".to_string()),
            window_days: Some(365),
            candidates: vec![pinned],
        },
    )
    .await;
    let cand = &payload["candidates"][0];
    assert_eq!(cand["adjudication_status"], json!("verified"));
    assert_eq!(cand["compatibility_status"], json!("mixed"));
    assert_eq!(cand["confirmed_compatible_samples"], json!(1));
    assert_eq!(cand["unresolved_samples"], json!(1));
    let by_run = |cand: &Value, id: &str| {
        cand["samples"]
            .as_array()
            .unwrap()
            .iter()
            .find(|sample| sample["eval_run_id"] == json!(id))
            .unwrap()
            .clone()
    };
    let confirmed_sample = by_run(cand, &confirmed);
    assert_eq!(
        confirmed_sample["compatibility"]["overall"],
        json!("fully_confirmed")
    );
    assert_eq!(
        confirmed_sample["compatibility"]["model_revision"],
        json!("confirmed")
    );
    assert_eq!(
        confirmed_sample["compatibility"]["role"],
        json!("confirmed")
    );
    assert_eq!(
        confirmed_sample["compatibility"]["task_type"],
        json!("confirmed")
    );
    let historical_sample = by_run(cand, &historical);
    assert_eq!(
        historical_sample["compatibility"]["overall"],
        json!("unresolved")
    );

    // Unscoped query: eligible rows pass the hard gates but role/task/
    // revision are visibly `unscoped`, never confirmed.
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![candidate(
                "u1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
        },
    )
    .await;
    let unscoped = &payload["candidates"][0];
    assert_eq!(unscoped["adjudication_status"], json!("verified"));
    assert_eq!(unscoped["compatibility_status"], json!("unscoped"));
    assert_eq!(unscoped["confirmed_compatible_samples"], json!(0));
    let sample = &unscoped["samples"][0];
    assert_eq!(sample["compatibility"]["overall"], json!("unscoped"));
    assert_eq!(sample["compatibility"]["role"], json!("unscoped"));
    assert_eq!(sample["compatibility"]["task_type"], json!("unscoped"));
    // Uniform unscoped labeling; the observed-vs-unrecorded nuance stays
    // in identity_basis.model_revision ("observed"/"unrecorded").
    assert_eq!(sample["compatibility"]["model_revision"], json!("unscoped"));
}

/// A candidate whose ONLY eligible row is fully scoped-and-matching
/// reports candidate-level `fully_confirmed`.
#[tokio::test]
async fn single_fully_confirmed_run_reports_fully_confirmed() {
    let server = make_server();
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    v34_run(
        &server,
        "bug1-solo",
        Some("fix_request"),
        None,
        "anthropic/claude-sonnet",
        Some("2026-03-10"),
        Some("implementation"),
        "bug1-solo-event",
    )
    .await;

    let mut pinned = candidate("c1", "anthropic/claude-sonnet", "claude_code_task_tool");
    pinned.role = Some("implementation".to_string());
    pinned.model_revision = Some("2026-03-10".to_string());
    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: Some("fix_request".to_string()),
            window_days: Some(365),
            candidates: vec![pinned],
        },
    )
    .await;
    let cand = &payload["candidates"][0];
    assert_eq!(cand["adjudication_status"], json!("verified"));
    assert_eq!(cand["compatibility_status"], json!("fully_confirmed"));
    assert_eq!(cand["confirmed_compatible_samples"], json!(1));
    assert_eq!(cand["unresolved_samples"], json!(0));
}

/// A fact stamped at/after the projection instant never
/// enters evidence — a future run INSIDE the old one-second tolerance, a
/// future observation, a future CURRENT adjudication (with an older usable
/// event proving no silent fallback), and a future superseded advisory.
#[tokio::test]
async fn future_facts_never_enter_evidence() {
    let server = make_server();
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );

    // (a) Future RUN inside the old +1s tolerance: the cohort SQL must
    // exclude it under the exact cutoff.
    let future_run = v34_run(
        &server,
        "bug2-future-run",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug2-future-run-event",
    )
    .await;
    force_created_at(
        &server,
        "mirror_eval_runs",
        &future_run,
        "2027-01-01T00:00:00.500Z",
    );

    // (b) In-window run with a FUTURE observation.
    let future_obs = v34_run(
        &server,
        "bug2-future-obs",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug2-future-obs-event",
    )
    .await;
    force_created_at(
        &server,
        "mirror_eval_observations",
        &future_obs,
        "2027-01-01T00:00:00.500Z",
    );

    // (c) In-window run whose CURRENT (latest) adjudication is future while
    // its OLDER usable event stays in-window — no silent fallback may
    // promote the older event to current.
    let future_adj = v34_run(
        &server,
        "bug2-future-adj",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug2-future-adj-first-event",
    )
    .await;
    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(future_adj.clone()),
                native_child_id: None,
                actor: "leader".to_string(),
                verifier_model: Some("zhipuai-coding-plan/glm-5".to_string()),
                usefulness: "useful".to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: None,
                next_prompt_delta: Some("future correction advisory".to_string()),
                evidence_usable: true,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence".to_string(),
                event_key: Some("bug2-future-adj-second-event".to_string()),
                rubric: None,
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("second adjudication should succeed");
    // Move ONLY the latest event (the current one) into the future.
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE mirror_eval_adjudications SET created_at = ?1 \
                     WHERE eval_run_id = ?2 AND insertion_seq = \
                       (SELECT MAX(insertion_seq) FROM mirror_eval_adjudications WHERE eval_run_id = ?2)",
                    rusqlite::params!["2027-01-01T00:00:00.500Z", future_adj],
                )
                .map_err(|e| e.to_string())
        })
        .expect("force future current adjudication");

    // (d) In-window run whose CURRENT event is in-window and whose ONLY
    // superseded event is future: the sample survives, the future advice is
    // dropped with a visible count, and the current advice is the one shown.
    let future_superseded = v34_run(
        &server,
        "bug2-future-superseded",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug2-superseded-first-event",
    )
    .await;
    // A second (current) in-window event carrying its own advisory.
    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(future_superseded.clone()),
                native_child_id: None,
                actor: "leader".to_string(),
                verifier_model: Some("zhipuai-coding-plan/glm-5".to_string()),
                usefulness: "useful".to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: None,
                next_prompt_delta: Some("current in-window advisory".to_string()),
                evidence_usable: true,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence".to_string(),
                event_key: Some("bug2-superseded-second-event".to_string()),
                rubric: None,
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("second adjudication should succeed");
    // Force ONLY the first (now superseded) event into the future.
    let superseded_id = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT adjudication_id FROM mirror_eval_adjudications \
                     WHERE eval_run_id = ?1 AND event_key = ?2",
                    rusqlite::params![future_superseded, "bug2-superseded-first-event"],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|e| e.to_string())
        })
        .unwrap();
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE mirror_eval_adjudications SET created_at = ?1 WHERE adjudication_id = ?2",
                    rusqlite::params!["2027-01-01T00:00:00.500Z", superseded_id],
                )
                .map_err(|e| e.to_string())
        })
        .unwrap();

    // One clean in-window control run: must appear.
    let clean = v34_run(
        &server,
        "bug2-clean",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "bug2-clean-event",
    )
    .await;

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
        },
    )
    .await;
    assert_eq!(
        payload["window"]["until"],
        json!("2027-01-01T00:00:00.000Z")
    );
    assert_eq!(payload["generated_at"], json!("2027-01-01T00:00:00.000Z"));

    let cand = &payload["candidates"][0];
    assert_eq!(
        cand["samples_total"],
        json!(2),
        "only the clean and the future-superseded runs"
    );
    let ids: Vec<&str> = cand["samples"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|sample| sample["eval_run_id"].as_str())
        .collect();
    assert!(ids.contains(&clean.as_str()), "the clean run is present");
    assert!(
        ids.contains(&future_superseded.as_str()),
        "the future-SUPERSEDED run is present (its current event is in-window)"
    );
    assert!(
        !ids.contains(&future_run.as_str()),
        "a future run never enters, tolerance or not"
    );
    assert!(
        !ids.contains(&future_obs.as_str()),
        "a future observation never enters"
    );
    assert!(
        !ids.contains(&future_adj.as_str()),
        "a future CURRENT adjudication excludes the run"
    );

    // The exclusion reasons are visible and counted.
    assert_eq!(cand["excluded_counts"]["observation_future"], json!(1));
    assert_eq!(
        cand["excluded_counts"]["current_adjudication_future"],
        json!(1)
    );

    // The future-superseded run: current advice kept, future advice dropped
    // with a visible count, and no fallback promotion of it.
    let superseded_sample = cand["samples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sample| sample["eval_run_id"] == json!(future_superseded))
        .unwrap();
    assert_eq!(
        superseded_sample["next_prompt_delta"],
        json!("current in-window advisory"),
        "the CURRENT in-window event's advisory is the sample's advisory"
    );
    assert_eq!(
        superseded_sample["future_superseded_advisories_dropped"],
        json!(1),
        "the future superseded advisory is dropped, counted, never listed"
    );
    assert_eq!(superseded_sample["historical_advisories"], json!([]));
}

/// The run cohort is admitted on the ACTUAL instant, RFC3339-authoritative:
/// an offset-valid run at the exact lower bound is included (a lexical
/// filter would omit its "2025-..." string); a date-only `created_at` that
/// SQLite's julianday preselect accepts is rejected here as
/// `run_timestamp_malformed`; an offset run whose instant is at/after the
/// cutoff never reaches evidence at all (the store's instant-aware preselect
/// drops it before classification).
#[tokio::test]
async fn run_cohort_admission_is_instant_based_and_rfc3339_authoritative() {
    let server = make_server();
    let _cutoff = crate::agent_eval::candidate_projection::test_hooks::CutoffGuard::set(
        "2027-01-01T00:00:00.000Z",
    );
    // window_days 365 => since = 2026-01-01T00:00:00.000Z.

    // Offset form whose instant is EXACTLY `since` (2026-01-01T00:00Z):
    // lower-inclusive admission, despite the lexically-earlier string.
    let at_lower = v34_run(
        &server,
        "cohort-lower-offset",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "cohort-lower-event",
    )
    .await;
    force_created_at(
        &server,
        "mirror_eval_runs",
        &at_lower,
        "2025-12-31T19:00:00-05:00",
    );

    // Date-only form: julianday parses it (instant 2026-06-01, inside the
    // window) so the store returns the row, but RFC3339 cannot establish
    // it — the projection must reject it as malformed, visibly counted.
    let date_only = v34_run(
        &server,
        "cohort-date-only",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "cohort-date-only-event",
    )
    .await;
    force_created_at(&server, "mirror_eval_runs", &date_only, "2026-06-01");

    // Offset form whose instant (2027-01-01T00:30Z) is at/after the cutoff
    // even though the string sorts before it: never enters evidence. The
    // store's instant-aware preselect drops it before classification, so it
    // appears neither in samples nor in the classifier's counts.
    let offset_future = v34_run(
        &server,
        "cohort-future-offset",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "cohort-future-event",
    )
    .await;
    force_created_at(
        &server,
        "mirror_eval_runs",
        &offset_future,
        "2026-12-31T23:30:00-01:00",
    );

    let clean = v34_run(
        &server,
        "cohort-clean",
        None,
        None,
        "anthropic/claude-sonnet",
        None,
        Some("implementation"),
        "cohort-clean-event",
    )
    .await;

    let payload = projection(
        &server,
        CandidateProjectionParams {
            task_type: None,
            window_days: Some(365),
            candidates: vec![candidate(
                "c1",
                "anthropic/claude-sonnet",
                "claude_code_task_tool",
            )],
        },
    )
    .await;

    let cand = &payload["candidates"][0];
    let sample_ids: Vec<&str> = cand["samples"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|sample| sample["eval_run_id"].as_str())
        .collect();
    assert!(
        sample_ids.contains(&at_lower.as_str()),
        "an offset-valid run at the exact lower-bound instant is included"
    );
    assert!(sample_ids.contains(&clean.as_str()));
    assert!(
        !sample_ids.contains(&date_only.as_str()),
        "a date-only created_at is malformed for admission even though the store preselects it"
    );
    assert!(
        !sample_ids.contains(&offset_future.as_str()),
        "an offset run whose instant is at/after the cutoff never enters evidence"
    );
    // The malformed row is visible in the classifier's counts; the
    // offset-future row was dropped by the store's instant-aware preselect
    // and so is not part of rows_considered at all.
    assert_eq!(cand["excluded_counts"]["run_timestamp_malformed"], json!(1));
    assert_eq!(cand["samples_total"], json!(2));
    assert_eq!(payload["rows_considered"], json!(3));
}
