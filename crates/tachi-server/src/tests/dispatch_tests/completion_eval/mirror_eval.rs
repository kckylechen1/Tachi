//! #1066 First-class mirror eval intake for harness-native subagents:
//! end-to-end coverage through the actual `tachi_agent_eval`/`tachi_complete`
//! MCP tool surface. Structural/idempotency/conflict unit coverage for the
//! write primitives lives in `memcore::db::mirror_eval`; eligibility-gate
//! unit coverage for the completion projection lives in
//! `complete_ops::mirror_eval_projection`. This file proves the wiring
//! between params -> handler -> memcore -> JSON, and the full closed loop
//! (register/observe/adjudicate -> complete -> aggregate_live) actually
//! works end-to-end — a class of bug unit tests in isolation cannot catch.

use super::*;
use crate::tool_params::{
    MirrorEvalAdjudicateParams, MirrorEvalGetParams, MirrorEvalObserveParams,
    MirrorEvalRegisterParams,
};

fn agent_eval_params(action: &str) -> TachiAgentEvalParams {
    TachiAgentEvalParams {
        action: action.to_string(),
        fixture_path: None,
        limit: None,
        register: None,
        observe: None,
        adjudicate: None,
        get: None,
    }
}

fn register_payload(native_id: &str, requested_model: &str) -> MirrorEvalRegisterParams {
    MirrorEvalRegisterParams {
        frozen_contract_ref: "kckylechen1/tachi#1066".to_string(),
        execution_origin: "host_native_subagent".to_string(),
        lifecycle_owner: "host".to_string(),
        harness: Some("claude_code_task_tool".to_string()),
        native_child_id: Some(native_id.to_string()),
        requested_profile: Some("explore".to_string()),
        requested_model: Some(requested_model.to_string()),
        requested_agent: Some("claude".to_string()),
    }
}

async fn register(server: &crate::MemoryServer, native_id: &str, model: &str) -> Value {
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(register_payload(native_id, model)),
            ..agent_eval_params("register")
        }))
        .await
        .expect("register should succeed");
    serde_json::from_str(&resp).expect("register JSON")
}

async fn observe(server: &crate::MemoryServer, eval_run_id: &str, effective_model: Option<&str>) {
    server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            observe: Some(MirrorEvalObserveParams {
                eval_run_id: Some(eval_run_id.to_string()),
                native_child_id: None,
                terminal_outcome: "success".to_string(),
                duration_ms: Some(3_000),
                cost_tokens: Some(900),
                cost_usd: None,
                result_ref: Some("PR#1".to_string()),
                artifacts: vec!["diff.patch".to_string()],
                effective_model: effective_model.map(|m| m.to_string()),
                effective_backend: None,
                effective_harness: None,
            }),
            ..agent_eval_params("observe")
        }))
        .await
        .expect("observe should succeed");
}

#[allow(clippy::too_many_arguments)]
async fn adjudicate(
    server: &crate::MemoryServer,
    eval_run_id: &str,
    actor: &str,
    verifier_model: Option<&str>,
    usefulness: &str,
    evidence_usable: bool,
    event_key: &str,
) -> Value {
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            adjudicate: Some(MirrorEvalAdjudicateParams {
                eval_run_id: Some(eval_run_id.to_string()),
                native_child_id: None,
                actor: actor.to_string(),
                verifier_model: verifier_model.map(|m| m.to_string()),
                usefulness: usefulness.to_string(),
                failure_mode: None,
                first_review_findings: Vec::new(),
                plan_delta: Some("accepted".to_string()),
                next_prompt_delta: None,
                evidence_usable,
                used_in_final_claim: true,
                human_override: false,
                evidence_ref: "run-evidence".to_string(),
                event_key: Some(event_key.to_string()),
            }),
            ..agent_eval_params("adjudicate")
        }))
        .await
        .expect("adjudicate should succeed");
    serde_json::from_str(&resp).expect("adjudicate JSON")
}

async fn get(server: &crate::MemoryServer, eval_run_id: &str) -> Value {
    let resp = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            get: Some(MirrorEvalGetParams {
                eval_run_id: Some(eval_run_id.to_string()),
                native_child_id: None,
            }),
            ..agent_eval_params("get")
        }))
        .await
        .expect("get should succeed");
    serde_json::from_str(&resp).expect("get JSON")
}

/// AC-2 through the actual tool surface: replaying the same native id with
/// the same content is idempotent (same eval_run_id, no duplicate row); the
/// same native id with different content is an explicit conflict.
#[tokio::test]
async fn register_replay_idempotent_conflict_via_tool_surface() {
    let server = make_server();
    let first = register(&server, "wire-native-1", "anthropic/claude-sonnet").await;
    let replay = register(&server, "wire-native-1", "anthropic/claude-sonnet").await;
    assert_eq!(first["eval_run_id"], replay["eval_run_id"]);

    let conflict = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            register: Some(register_payload("wire-native-1", "openai/gpt-5")),
            ..agent_eval_params("register")
        }))
        .await
        .expect_err("differing payload under the same native id must be rejected");
    assert!(conflict.contains("conflict"), "got: {conflict}");
}

/// AC-3 through the tool surface: after register + observe only, the run is
/// NOT adjudicated — observe has no path to write judgment.
#[tokio::test]
async fn observe_alone_never_adjudicates_via_tool_surface() {
    let server = make_server();
    let registered = register(&server, "wire-native-2", "anthropic/claude-sonnet").await;
    let eval_run_id = registered["eval_run_id"].as_str().unwrap();

    observe(&server, eval_run_id, Some("anthropic/claude-sonnet")).await;

    let view = get(&server, eval_run_id).await;
    assert_eq!(view["is_adjudicated"], serde_json::json!(false));
    assert_eq!(view["adjudications"], serde_json::json!([]));
}

/// AC-7: cross-model independence requires BOTH sides to carry a known,
/// differing effective engine; an unknown identity on either side can never
/// satisfy it.
#[tokio::test]
async fn cross_model_independence_requires_two_known_differing_engines() {
    let server = make_server();

    // Cross-model: producer=claude, verifier=gpt -> independent, not self-eval.
    let cross = register(&server, "wire-native-cross", "anthropic/claude-sonnet").await;
    let cross_id = cross["eval_run_id"].as_str().unwrap();
    observe(&server, cross_id, Some("anthropic/claude-sonnet")).await;
    let cross_adj = adjudicate(
        &server,
        cross_id,
        "leader",
        Some("openai/gpt-5"),
        "useful",
        true,
        "cross-event",
    )
    .await;
    assert_eq!(cross_adj["cross_model_independent"], serde_json::json!(true));
    assert_eq!(cross_adj["self_eval"], serde_json::json!(false));

    // Self-eval: producer and verifier are the same lineage (claude family).
    let same = register(&server, "wire-native-self", "anthropic/claude-sonnet").await;
    let same_id = same["eval_run_id"].as_str().unwrap();
    observe(&server, same_id, Some("anthropic/claude-sonnet")).await;
    let same_adj = adjudicate(
        &server,
        same_id,
        "self",
        Some("anthropic/claude-opus"),
        "useful",
        true,
        "self-event",
    )
    .await;
    assert_eq!(same_adj["cross_model_independent"], serde_json::json!(false));
    assert_eq!(same_adj["self_eval"], serde_json::json!(true));

    // Unknown identity on either side: never independent, never self-eval.
    let unknown = register(&server, "wire-native-unknown", "").await;
    let unknown_id = unknown["eval_run_id"].as_str().unwrap();
    observe(&server, unknown_id, None).await;
    let unknown_adj = adjudicate(
        &server,
        unknown_id,
        "leader",
        None,
        "useful",
        true,
        "unknown-event",
    )
    .await;
    assert_eq!(
        unknown_adj["cross_model_independent"],
        serde_json::json!(false),
        "unknown native model identity must never satisfy cross-model independence"
    );
    assert_eq!(unknown_adj["self_eval"], serde_json::json!(false));

    let unknown_view = get(&server, unknown_id).await;
    assert_eq!(
        unknown_view["cross_model_independent"],
        serde_json::json!(false)
    );
}

/// AC-4 / AC-5 end-to-end: `tachi_complete(eval_run_ids=[...])` projects only
/// the adjudicated + evidence-usable + non-self-eval run into aggregation;
/// an evidence_usable=false run, an unadjudicated run, and a wholly unknown
/// eval_run_id are all silently excluded and never fail the completion.
#[tokio::test]
async fn complete_projects_only_eligible_eval_run_ids_into_aggregation() {
    let server = make_server();

    let eligible = register(&server, "wire-eligible", "anthropic/claude-sonnet").await;
    let eligible_id = eligible["eval_run_id"].as_str().unwrap().to_string();
    observe(&server, &eligible_id, Some("anthropic/claude-sonnet")).await;
    adjudicate(
        &server,
        &eligible_id,
        "leader",
        Some("openai/gpt-5"),
        "useful",
        true,
        "eligible-event",
    )
    .await;

    let not_usable = register(&server, "wire-not-usable", "anthropic/claude-sonnet").await;
    let not_usable_id = not_usable["eval_run_id"].as_str().unwrap().to_string();
    observe(&server, &not_usable_id, Some("anthropic/claude-sonnet")).await;
    adjudicate(
        &server,
        &not_usable_id,
        "leader",
        Some("openai/gpt-5"),
        "failed",
        false,
        "not-usable-event",
    )
    .await;

    let unadjudicated = register(&server, "wire-unadjudicated", "anthropic/claude-sonnet").await;
    let unadjudicated_id = unadjudicated["eval_run_id"].as_str().unwrap().to_string();
    observe(&server, &unadjudicated_id, Some("anthropic/claude-sonnet")).await;

    let completed = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("mirror-eval-projection-001".to_string()),
            task: "Review mirror eval intake evidence".to_string(),
            agent: "codex-controller".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: None,
            risk: None,
            duration_ms: Some(1000),
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: vec![
                eligible_id,
                not_usable_id,
                unadjudicated_id,
                "totally-unknown-eval-run-id".to_string(),
            ],
            feedback_rules_applied: Vec::new(),
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            flow_id: None,
            dispatch_id: None,
            issue_ref: None,
            pr_ref: None,
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }))
        .await
        .expect("completion must never fail on ineligible/unknown eval_run_ids");
    let completed: Value = serde_json::from_str(&completed).expect("completion JSON");
    assert!(completed["eval_entry"]["id"].as_str().is_some());

    let aggregate_live = server
        .tachi_agent_eval(Parameters(TachiAgentEvalParams {
            action: "aggregate_live".to_string(),
            fixture_path: None,
            limit: Some(50),
            register: None,
            observe: None,
            adjudicate: None,
            get: None,
        }))
        .await
        .expect("aggregate_live should succeed");
    let aggregate: Value = serde_json::from_str(&aggregate_live).expect("aggregate_live JSON");
    assert!(
        aggregate["subagent_scores"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["agent"] == "claude"
                && row["role"] == "explore"
                && row["useful_rate"] == 1.0)),
        "the eligible mirror eval run must feed subagent scores: {aggregate:#}"
    );
    assert!(
        !aggregate["subagent_scores"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["role"] == "explore"
                && row["useful_rate"] == 0.0)),
        "the evidence_usable=false / unadjudicated runs must not feed subagent scores: \
         {aggregate:#}"
    );
}
