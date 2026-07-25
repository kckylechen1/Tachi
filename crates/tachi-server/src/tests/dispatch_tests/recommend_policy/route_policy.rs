use super::*;
use sha2::{Digest, Sha256};

async fn seed_route_policy_inputs(server: &crate::MemoryServer, suffix: &str) {
    for (profile, outcome, cost_usd, quality_score) in [
        ("opencode_builder", "success", 0.01, 0.80),
        ("opencode_builder", "failure", 0.01, 0.20),
        ("glm_51_impl", "success", 2.00, 0.98),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("route-source-{suffix}-{profile}-{outcome}")),
                task: "Route proposal source-revision fixture".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:route-source-revision".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost_usd),
                quality_score: Some(quality_score),
                notes: None,
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some(format!("flow-route-source-{suffix}")),
                issue_ref: Some("kckylechen1/tachi#1425".to_string()),
                pr_ref: None,
                evidence_refs: vec!["route-source-revision".to_string()],
                tests_run: vec!["targeted route source revision test".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed route policy input");
    }
}

async fn generate_route_policy_proposal(server: &crate::MemoryServer) -> serde_json::Value {
    let mut params = task_params("proposals");
    params.limit = Some(50);
    let body = server
        .tachi_task(Parameters(params))
        .await
        .expect("generate route policy proposal");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("route proposal JSON");
    parsed["proposals"]
        .as_array()
        .and_then(|proposals| {
            proposals
                .iter()
                .find(|proposal| proposal["kind"] == json!("route_policy"))
        })
        .cloned()
        .expect("route policy proposal")
}

fn read_route_policy_row(server: &crate::MemoryServer, proposal_id: &str) -> (String, u32) {
    server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load route proposal row")
        .expect("route proposal row")
}

fn test_route_content_digest(identity_payload: &serde_json::Value) -> String {
    let canonical = tachi_dispatch::policy::canonical_json(identity_payload).to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn tachi_task_route_simulate_compares_policy_variants_from_live_eval() {
    let (server, _temp_home) = make_server_with_temp_home();

    for (task_id, profile, agent, outcome, duration_ms, cost_usd, quality_score) in [
        (
            "simulate-cheap-success",
            "opencode_builder",
            "custom",
            "success",
            100_000,
            0.01,
            0.75,
        ),
        (
            "simulate-cheap-failure",
            "opencode_builder",
            "custom",
            "failure",
            100_000,
            0.01,
            0.20,
        ),
        (
            "simulate-quality-success",
            "glm_51_impl",
            "custom",
            "success",
            800_000,
            2.00,
            0.98,
        ),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "Implement dispatch policy replay".to_string(),
                agent: agent.to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(duration_ms),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost_usd),
                quality_score: Some(quality_score),
                notes: Some("Seed route simulation fixture.".to_string()),
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-route-sim".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed eval row");
    }

    let mut params = task_params("route_simulate");
    params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("route_simulate should succeed");
    let sim: serde_json::Value = serde_json::from_str(&raw).expect("route simulation JSON");

    assert_eq!(sim["action"], json!("route_simulate"));
    assert_eq!(sim["read_only"], json!(true));
    assert_eq!(sim["row_count"], json!(3));
    let policies = sim["policies"].as_array().expect("policies");
    assert_eq!(policies.len(), 3);
    let cost_sensitive = policies
        .iter()
        .find(|policy| policy["policy"] == json!("cost_sensitive"))
        .expect("cost_sensitive policy");
    let quality_first = policies
        .iter()
        .find(|policy| policy["policy"] == json!("quality_first"))
        .expect("quality_first policy");
    let cost_choice = cost_sensitive["route_choices"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice["task_type"] == json!("fix_request"))
        })
        .expect("cost-sensitive fix route");
    let quality_choice = quality_first["route_choices"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice["task_type"] == json!("fix_request"))
        })
        .expect("quality-first fix route");

    assert_eq!(cost_choice["profile"], json!("opencode_builder"));
    assert_eq!(quality_choice["profile"], json!("glm_impl"));
    assert!(
        cost_sensitive["caveats"]
            .as_array()
            .is_some_and(|caveats| caveats.iter().any(|caveat| caveat
                .as_str()
                .is_some_and(|s| s.contains("low sample count")))),
        "low sample caveat should be visible: {sim:#}"
    );
}

/// tachi#1200 item 1 (judgmental test named by the issue): a `tachi_complete`
/// call that carries `dispatch_id` but omits `profile` must still land its
/// eval row where `route_simulate` can match it — via the auto-inject from
/// the dispatch's own kanban card, not by the caller re-supplying it.
/// `simulate_route_policy` filters live rows on an EXACT `EvalRow.profile`
/// match against a known dispatch profile name
/// (`crates/tachi-dispatch/src/routing.rs`); a row with `profile: None` is
/// silently dropped from every policy's route_choices for its task_type, not
/// merely degraded — so before this fix, `route_choices` has NO entry at all
/// for "fix_request" here.
#[tokio::test]
async fn tachi_task_route_simulate_matches_dispatch_completed_without_explicit_profile() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260717T000003Z-custom-route-sim-linkage";
    seed_dispatch_run(&server, dispatch_id);
    let profile = "opencode_builder";

    // Seed the kanban card the way a real dispatch launch would
    // (`dispatch_ops::kanban_helpers::init_kanban_task`).
    crate::memory_search_ops::handle_save_memory(
        &server,
        crate::tool_params::SaveMemoryParams {
            text: "Dispatch Task\nAgent: custom\nTask: route sim linkage fixture".to_string(),
            summary: "Kanban: route sim linkage fixture".to_string(),
            path: format!("/kanban/tasks/{dispatch_id}"),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec!["kanban".to_string(), "dispatch".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            project_explicit: false,
            retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "type": "a2a_task",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
                "agent": "custom",
                "profile": profile,
                "eval_ledger_id": null,
            })),
            emit_continuity: false,
        },
    )
    .await
    .expect("seed kanban card with profile on file");

    // Direct `tachi_complete` tool call (dispatch_facade.rs's entry point,
    // NOT the `tachi_task(action='complete')` bridge) — the caller
    // deliberately omits `profile`, matching the real-world dogfood gap.
    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("route-sim-linkage".to_string()),
            task: "Implement route sim linkage from kanban profile".to_string(),
            agent: "custom".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: Some("medium".to_string()),
            duration_ms: Some(100_000),
            skills_used: Vec::new(),
            cost_tokens: Some(1000),
            cost_usd: Some(0.01),
            quality_score: Some(0.8),
            notes: None,
            trajectory: None,
            diff: Some("diff --git a/x b/x".to_string()),
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: Some(dispatch_id.to_string()),
            flow_id: None,
            issue_ref: Some("kckylechen1/tachi#1200".to_string()),
            pr_ref: None,
            evidence_refs: vec!["crates/tachi-server/src/complete_ops.rs".to_string()],
            tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
            diff_present: Some(true),
            scope: Some("global".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
            eval_run_ids: Vec::new(),
        }))
        .await
        .expect("complete without explicit profile should still succeed");

    let mut params = task_params("route_simulate");
    params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("route_simulate should succeed");
    let sim: serde_json::Value = serde_json::from_str(&raw).expect("route simulation JSON");

    assert_eq!(sim["row_count"], json!(1));
    let current = sim["policies"]
        .as_array()
        .expect("policies")
        .iter()
        .find(|policy| policy["policy"] == json!("current"))
        .expect("current policy");
    let fix_route = current["route_choices"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice["task_type"] == json!("fix_request"))
        })
        .unwrap_or_else(|| {
            panic!(
                "no matching leader/profile eval rows; policy comparison is evidence-empty \
                 (profile linkage did not reach route_simulate): {sim:#}"
            )
        });
    assert_eq!(fix_route["profile"], json!(profile), "{sim:#}");
    assert_eq!(fix_route["samples"], json!(1), "{sim:#}");
    assert!(
        !sim["caveats"]
            .as_array()
            .is_some_and(|caveats| caveats.iter().any(|caveat| caveat
                .as_str()
                .is_some_and(|s| s.contains("none have leader profile ids")))),
        "the missing-profile-id caveat must be gone once linkage is backfilled: {sim:#}"
    );
}

#[tokio::test]
async fn tachi_task_route_policy_proposals_require_review_before_apply() {
    let (server, _temp_home) = make_server_with_temp_home();

    for (task_id, profile, outcome, cost_usd, quality_score) in [
        (
            "proposal-cheap-success",
            "opencode_builder",
            "success",
            0.01,
            0.75,
        ),
        (
            "proposal-cheap-failure",
            "opencode_builder",
            "failure",
            0.01,
            0.20,
        ),
        (
            "proposal-quality-success",
            "glm_51_impl",
            "success",
            2.00,
            0.98,
        ),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "Implement dispatch policy proposal flow".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost_usd),
                quality_score: Some(quality_score),
                notes: Some("Seed route policy proposal fixture.".to_string()),
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-route-policy-proposals".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| items.first())
        .expect("at least one proposal");
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal_id")
        .to_string();
    assert_eq!(proposal["status"], json!("pending"));
    assert_eq!(proposal["requires_human_approval"], json!(true));

    let mut premature_apply = task_params("apply_proposals");
    premature_apply.proposal_id = Some(proposal_id.clone());
    premature_apply.confirm = true;
    let err = server
        .tachi_task(Parameters(premature_apply))
        .await
        .expect_err("pending proposal must not apply");
    assert!(err.contains("must be approved before apply"));

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("Human approved cost-sensitive route rule.".to_string());
    let reviewed_raw = server
        .tachi_task(Parameters(review))
        .await
        .expect("review should succeed");
    let reviewed: serde_json::Value = serde_json::from_str(&reviewed_raw).expect("review JSON");
    assert_eq!(reviewed["proposal"]["status"], json!("approved"));

    let mut missing_confirm = task_params("apply_proposals");
    missing_confirm.proposal_id = Some(proposal_id.clone());
    let err = server
        .tachi_task(Parameters(missing_confirm))
        .await
        .expect_err("apply requires confirm");
    assert!(err.contains("confirm=true"));

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply))
        .await
        .expect("apply should succeed");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply JSON");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["proposal"]["status"], json!("applied"));
    assert_eq!(
        applied["rule_namespace"],
        json!("dispatch_route_policy_rules")
    );
}

// ─── v2 proposal-safety discrimination tests ─────────────────────────────────
//
// These are end-to-end tests through tachi_task / tachi_complete. They are
// written but NOT run by this lane — the leader runs the full suite in the
// delivery worktree. Each test names the production path it bites and the
// red->green property it discriminates.

/// Discrimination: regenerating route-policy proposals after the underlying
/// eval evidence changed (here: a different fallback/current profile wins)
/// MUST mint a fresh v2 proposal id and a fresh pending row, never inheriting
/// the prior approval.
///
/// Production path: `handle_route_policy_proposals` -> v2 content-addressed
/// id derived from `route_policy_v2_identity_payload` (which binds the
/// apply payload's `fallback_to_current_profile` and the evidence row/limit).
/// Pre-fix red: proposals used a deterministic id keyed only on
/// (policy, task_type, proposed_profile), so flipping the *fallback* profile
/// (or the evidence row count) kept the same id and silently inherited the
/// prior approval.
/// Post-fix green: changing the fallback/evidence rotates the SHA-256 id,
/// and the regenerate path finds no prior row at the new id, so the new
/// proposal starts pending.
///
/// ── Admission conditions for a `route_policy` proposal (read before editing
/// the seeds below; getting them wrong yields ZERO proposals and an
/// "at least one route_policy proposal" panic that looks like a product bug) ──
///
/// There is NO sample-count threshold on proposal *generation*. The only
/// admission rule is in `build_route_policy_proposals`
/// (`crates/tachi-dispatch/src/policy.rs:312-319`): for each variant policy
/// (`cost_sensitive`, `quality_first` — `handlers.rs:138-142`) and each of its
/// route choices, the task_type must also appear in the `current` policy's
/// choices AND the variant's winning profile must DIFFER from the current
/// policy's winning profile. Same winner => `continue` => no proposal. So the
/// seeds must make a variant policy DISAGREE with `current`; seeding "a
/// baseline profile plus a cheaper one" is not enough if `current` already
/// ranks the cheaper one first.
///
/// Row admission into the simulation (`simulate_route_policy`,
/// `crates/tachi-dispatch/src/routing.rs:820-836`): scope must be `leader`
/// (always true for a `tachi_complete` leader row — `eval.rs:391-392`), and the
/// profile must resolve to a canonical `DISPATCH_PROFILES` name (aliases are
/// canonicalized in `route_performance_rows`, `routing.rs:178-206`, which is
/// why `glm_51_impl` shows up as `glm_impl`).
///
/// Ranking (`route_policy_score`, `routing.rs:893-946`) is a weighted sum over
/// success/quality/verification/cost/latency/failure — `cost_usd`,
/// `quality_score` and `duration_ms` all reach it from `tachi_complete`
/// (`complete_ops/eval_record.rs:168-183` -> `agent_eval/live.rs:56-59` ->
/// `eval.rs` matrix). The "low sample count" string the sibling
/// `route_simulate` test asserts is an advisory caveat only
/// (`routing.rs:1054-1056`), NOT a gate. The one real sample threshold,
/// `MIN_ROUTE_POLICY_RULE_SAMPLES` (=2), gates *applying an approved rule* at
/// recommend time (`routing.rs:711-715`), not proposal generation.
///
/// Scope of what this end-to-end test can prove: `evidence` embeds the current
/// choice and the row count (`policy.rs:332-339`), so a fallback flip ALWAYS
/// drags the evidence with it — no live seeding can change the fallback while
/// holding evidence fixed. The isolated properties are pinned at unit level in
/// `tachi-dispatch` (`route_policy_identity_payload_rotates_when_fallback_changes`,
/// `policy.rs:1147`, and `..._when_evidence_changes`, `policy.rs:1215`); this
/// test pins the end-to-end conjunction plus the observable fallback flip.
#[tokio::test]
async fn route_regen_with_changed_fallback_evidence_gets_new_pending_id() {
    let (server, _temp_home) = make_server_with_temp_home();

    let seed_one = |task_id: &str, profile: &str, outcome: &str, cost: f64, quality: f64| {
        // Borrow the server and own the &str arguments before building the
        // future. The closure is invoked repeatedly, so it must stay `Fn`: an
        // `async move` block that captured `server` directly would move it out
        // of the closure's environment and make the closure `FnOnce`. Owning
        // the arguments here also keeps any borrow of the caller's &str from
        // escaping into the returned future.
        let server = &server;
        let task_id = task_id.to_string();
        let profile = profile.to_string();
        let outcome = outcome.to_string();
        async move {
            server
                .tachi_complete(Parameters(TachiCompleteParams {
                    task_id: Some(task_id),
                    task: "Implement dispatch policy".to_string(),
                    agent: "custom".to_string(),
                    outcome: outcome.to_string(),
                    task_type: Some("fix_request".to_string()),
                    profile: Some(profile.to_string()),
                    risk: Some("medium".to_string()),
                    duration_ms: Some(100_000),
                    skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                    cost_tokens: Some(1000),
                    cost_usd: Some(cost),
                    quality_score: Some(quality),
                    notes: None,
                    trajectory: None,
                    diff: Some("diff --git a/x b/x".to_string()),
                    worktree: None,
                    subagents: Vec::new(),
                    feedback_rules_applied: Vec::new(),
                    dispatch_id: None,
                    flow_id: Some("flow-regen-fallback".to_string()),
                    issue_ref: Some("kckylechen1/tachi#194".to_string()),
                    pr_ref: None,
                    evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                    tests_run: vec!["cargo test".to_string()],
                    diff_present: Some(true),
                    scope: Some("project".to_string()),
                    project: None,
                    format: None,
                    signatures: Vec::new(),
                    rulings: Vec::new(),
                    adjudication: None,
                    eval_run_ids: Vec::new(),
                }))
                .await
                .expect("seed eval row")
        }
    };

    // Phase A seeds — the same shape the sibling tests in this file
    // (`..._proposals_require_review_before_apply`,
    // `route_terminal_state_cannot_be_rereviewed`, ...) already use to get a
    // route_policy proposal: a cheap+flaky profile plus an expensive+excellent
    // one, so the `current` policy and the `cost_sensitive` variant pick
    // DIFFERENT winners for fix_request. The expensive profile's cost is 3.00
    // rather than the siblings' 2.00 purely to widen the phase-B margin below.
    //
    // Scores per `route_policy_score` (`routing.rs:893-946`), all rows at
    // duration 100_000ms (= 1.667 latency-minutes) and verification present:
    //   opencode_builder (2 samples, success_rate 0.50, quality 0.50,
    //     failure_rate 0.50, cost 0.01):  current 33.7 | cost_sensitive 24.7
    //   glm_impl (1 sample, success 1.00, quality 0.98, cost 3.00):
    //     current 55.8 | cost_sensitive -37.7
    // => current picks glm_impl, cost_sensitive picks opencode_builder, and
    // the disagreement is what admits the proposal (`policy.rs:317`). Do NOT
    // "simplify" these to all-success rows: if every policy agrees on one
    // winner, generation emits nothing and the assertions below panic.
    seed_one(
        "regen-fallback-A1",
        "opencode_builder",
        "success",
        0.01,
        0.80,
    )
    .await;
    seed_one(
        "regen-fallback-A2",
        "opencode_builder",
        "failure",
        0.01,
        0.20,
    )
    .await;
    seed_one("regen-fallback-A3", "glm_51_impl", "success", 3.00, 0.98).await;

    let mut proposals = task_params("proposals");
    proposals.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposals))
        .await
        .expect("proposals A");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON A");
    let first = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|proposal| proposal["kind"] == json!("route_policy"))
        })
        .expect("at least one route_policy proposal in phase A");
    let first_id = first["proposal_id"].as_str().expect("id").to_string();
    assert!(
        first_id.starts_with("route_policy:v3:"),
        "v2 id format expected, got: {first_id}"
    );
    assert_eq!(first["status"], json!("pending"));
    assert_eq!(first["schema_version"], json!(3));
    // Pin the construction itself, not just its shape: the phase-A proposal is
    // the cost_sensitive variant proposing opencode_builder while `current`
    // still routes fix_request to glm_impl. If this ever fails, the seeds no
    // longer produce the intended disagreement and every later assertion in
    // this test is measuring something else.
    assert_eq!(
        first["policy_rule"]["fallback_to_current_profile"],
        json!("glm_impl"),
        "phase-A fallback must be the current-policy winner: {first:#}"
    );
    assert_eq!(
        first["policy_rule"]["prefer_profile"],
        json!("opencode_builder"),
        "phase-A proposal must be the cost_sensitive challenger: {first:#}"
    );

    // Approve the v1-id-rotation proposal so we can prove the regenerated
    // proposal under a different id does NOT inherit this approval.
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(first_id.clone());
    review.review_status = Some("approved".to_string());
    let _ = server
        .tachi_task(Parameters(review))
        .await
        .expect("review A");

    // Phase B: flip the CURRENT-policy winner for fix_request from `glm_impl`
    // to `claude_plan`, which is exactly what the apply payload's
    // `fallback_to_current_profile` records. `claude_plan` is seeded cheaper
    // and near-as-good as glm_impl, so it wins `current`
    // (45*1 + 25*0.95 + 18 - 10*1.55 - 1.667 = 69.6 vs glm_impl 55.8), while
    // staying far too expensive for `cost_sensitive`
    // (45 + 15 + 10*0.95 - 35*1.55 - 2.5 = 12.8 vs opencode_builder 24.7).
    // So cost_sensitive still challenges with opencode_builder and the
    // regenerated proposal carries a DIFFERENT fallback than phase A.
    seed_one("regen-fallback-B1", "claude_plan", "success", 1.55, 0.95).await;
    seed_one("regen-fallback-B2", "claude_plan", "success", 1.55, 0.95).await;
    seed_one("regen-fallback-B3", "claude_plan", "success", 1.55, 0.95).await;

    let mut proposals_b = task_params("proposals");
    proposals_b.limit = Some(50);
    let raw_b = server
        .tachi_task(Parameters(proposals_b))
        .await
        .expect("proposals B");
    let parsed_b: serde_json::Value = serde_json::from_str(&raw_b).expect("proposals JSON B");
    let route_proposals_b: Vec<&serde_json::Value> = parsed_b["proposals"]
        .as_array()
        .expect("proposals")
        .iter()
        .filter(|proposal| proposal["kind"] == json!("route_policy"))
        .collect();
    assert!(
        !route_proposals_b.is_empty(),
        "expected at least one route_policy proposal in phase B"
    );

    // At least one phase-B id must be different from the phase-A id (the
    // identity rotated because the fallback/current profile changed). And
    // whichever phase-B proposal shares the phase-A id (if any) keeps the
    // approval; any new id must be pending.
    let any_new_id = route_proposals_b
        .iter()
        .find(|proposal| proposal["proposal_id"].as_str() != Some(first_id.as_str()));
    let new = any_new_id.expect(
        "expected at least one phase-B proposal with a new v2 id after the fallback flipped; \
         if every proposal kept the same id, the identity is not bound to the apply payload",
    );
    let new_id = new["proposal_id"].as_str().expect("id").to_string();
    assert_ne!(new_id, first_id, "id must rotate when fallback changes");
    assert_eq!(
        new["status"],
        json!("pending"),
        "the rotated-id proposal must NOT inherit the prior approval"
    );
    assert_eq!(new["schema_version"], json!(3));
    // The rotation is observably driven by the fallback flip, not merely by
    // "some field somewhere changed": the regenerated proposal records the NEW
    // current-policy winner (`claude_plan`) where phase A recorded `glm_impl`.
    assert_eq!(
        new["policy_rule"]["fallback_to_current_profile"],
        json!("claude_plan"),
        "phase-B fallback must be the new current-policy winner: {new:#}"
    );
    assert_ne!(
        new["policy_rule"]["fallback_to_current_profile"],
        first["policy_rule"]["fallback_to_current_profile"],
        "the fallback the two proposals bind must actually differ"
    );
}

/// Discrimination: once a route-policy proposal is in a terminal state
/// (rejected OR applied), it cannot be reviewed again. The review path must
/// refuse loudly with the terminal-state error.
///
/// Production path: `handle_route_policy_review` -> pending-state guard.
/// Pre-fix red: review used `set_state` (no CAS, no terminal guard), so a
/// rejected proposal could be re-approved, or an applied one re-rejected.
/// Post-fix green: only `pending -> approved | rejected` is permitted.
#[tokio::test]
async fn route_terminal_state_cannot_be_rereviewed() {
    let (server, _temp_home) = make_server_with_temp_home();

    for (task_id, profile, outcome, cost, quality) in [
        ("terminal-1", "opencode_builder", "success", 0.01, 0.80),
        ("terminal-2", "opencode_builder", "failure", 0.01, 0.20),
        ("terminal-3", "glm_51_impl", "success", 2.00, 0.98),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "Implement dispatch policy terminal test".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost),
                quality_score: Some(quality),
                notes: None,
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-terminal-test".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposals = task_params("proposals");
    proposals.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposals))
        .await
        .expect("proposals");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let first = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|proposal| proposal["kind"] == json!("route_policy"))
        })
        .expect("at least one route_policy proposal");
    let proposal_id = first["proposal_id"].as_str().expect("id").to_string();

    // Move the proposal to terminal "rejected".
    let mut reject = task_params("review_proposal");
    reject.proposal_id = Some(proposal_id.clone());
    reject.review_status = Some("rejected".to_string());
    let _ = server.tachi_task(Parameters(reject)).await.expect("reject");

    // Re-review the rejected row: must refuse with a terminal-state error.
    let mut re_approve = task_params("review_proposal");
    re_approve.proposal_id = Some(proposal_id.clone());
    re_approve.review_status = Some("approved".to_string());
    let err = server
        .tachi_task(Parameters(re_approve))
        .await
        .expect_err("a rejected proposal must not be re-reviewable");
    assert!(
        err.contains("terminal state"),
        "expected a terminal-state refusal, got: {err}"
    );
}

/// Discrimination: two applies of the same approved proposal must yield
/// exactly one terminal receipt. The first apply succeeds (status -> applied,
/// rule row created); the second apply is refused because the status is no
/// longer `approved`. The hard_state CAS on the apply path guarantees even a
/// truly concurrent pair of applies produces exactly one terminal write.
///
/// Production path: `handle_route_policy_apply` -> status==approved guard
/// AND the `set_state_if_version` CAS inside the atomic transaction.
/// Pre-fix red: apply used `set_state` (no CAS), so a concurrent pair could
/// both read `approved`, both write `applied`, and stamp two apply_at
/// receipts on the same row (last-write-wins, no single-winner guarantee).
/// Post-fix green: the CAS makes one apply the winner and the other a
/// `stale_state_version` refusal (or, for a sequential pair, an
/// `must be approved before apply` refusal because the first already moved
/// the status to `applied`).
#[tokio::test]
async fn route_two_applies_yield_one_terminal_receipt() {
    let (server, _temp_home) = make_server_with_temp_home();

    for (task_id, profile, outcome, cost, quality) in [
        ("two-apply-1", "opencode_builder", "success", 0.01, 0.80),
        ("two-apply-2", "opencode_builder", "failure", 0.01, 0.20),
        ("two-apply-3", "glm_51_impl", "success", 2.00, 0.98),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "two-apply fixture".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost),
                quality_score: Some(quality),
                notes: None,
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-two-apply".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposals = task_params("proposals");
    proposals.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposals))
        .await
        .expect("proposals");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let first = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|proposal| proposal["kind"] == json!("route_policy"))
        })
        .expect("at least one route_policy proposal");
    let proposal_id = first["proposal_id"].as_str().expect("id").to_string();

    let mut approve = task_params("review_proposal");
    approve.proposal_id = Some(proposal_id.clone());
    approve.review_status = Some("approved".to_string());
    let _ = server
        .tachi_task(Parameters(approve))
        .await
        .expect("approve");

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply.clone()))
        .await
        .expect("apply #1");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply #1 JSON");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["proposal"]["status"], json!("applied"));

    // Second apply: must refuse because status is now `applied`, not
    // `approved`. Exactly one terminal receipt exists.
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("second apply must be refused; status is no longer approved");
    assert!(
        err.contains("must be approved before apply"),
        "expected 'must be approved before apply' refusal on the second apply, got: {err}"
    );

    // Exactly one rule row in the route-rule namespace for this proposal id.
    let rules = server
        .with_global_store_read(|store| {
            store
                .list_state(tachi_dispatch::ROUTE_POLICY_RULE_NS)
                .map_err(|e| e.to_string())
        })
        .expect("list rules");
    let matching = rules.iter().filter(|row| row.key == proposal_id).count();
    assert_eq!(matching, 1, "exactly one rule row for the applied proposal");
}

/// Discrimination: the top-level `policy_rule` (and `evidence`) fields on a
/// route-policy proposal row are DISPLAY copies — the exact fields
/// `handle_route_policy_proposals`/`handle_route_policy_review` return to a
/// human reviewer — distinct from the digest-bound
/// `identity_payload.apply_payload`/`evidence_review` copies. `content_digest`
/// only proves `identity_payload` is internally self-consistent; it says
/// nothing about whether the display copy still matches it. A row whose
/// display copy was tampered *without* touching `identity_payload`/
/// `content_digest` still passes the digest check, so a human approving
/// based on the (tampered) display copy has not actually approved the bound
/// content — refusing loudly is the only sound response; silently applying
/// the bound (correct) value would mean this code decided, on the human's
/// behalf, that they "really meant" the untampered version.
///
/// Production path: `handle_route_policy_apply` ->
/// `route_policy_display_drift` (canonical_json_eq against
/// `identity_payload.apply_payload`/`evidence_review`), checked immediately
/// after the content_digest re-validation and before the row is mutated or
/// persisted to `DISPATCH_POLICY_PROPOSAL_NS`/`ROUTE_POLICY_RULE_NS`.
/// Pre-fix red (cross-vendor review, #1424/#1425): tampering ONLY the
/// top-level `policy_rule.prefer_profile` (leaving
/// `identity_payload`/`content_digest` byte-for-byte untouched) passed the
/// digest check with no further cross-check, so apply proceeded — under the
/// interim (write-side-sync-only) fix it proceeded silently against the
/// ORIGINAL value; under the fully unfixed pre-#1424 code the attacker's
/// value would have been persisted verbatim into `ROUTE_POLICY_RULE_NS`,
/// where every subsequent routing decision would read it.
/// Post-fix green: apply is REFUSED with `display_copy_drift`; no rule row
/// is ever written, and the proposal row is byte-identical before and after
/// the refused call.
#[tokio::test]
async fn route_apply_refuses_tampered_unbound_top_level_policy_rule() {
    let (server, _temp_home) = make_server_with_temp_home();

    for (task_id, profile, outcome, cost, quality) in [
        (
            "unbound-tamper-1",
            "opencode_builder",
            "success",
            0.01,
            0.80,
        ),
        (
            "unbound-tamper-2",
            "opencode_builder",
            "failure",
            0.01,
            0.20,
        ),
        ("unbound-tamper-3", "glm_51_impl", "success", 2.00, 0.98),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "route policy unbound-copy tamper fixture".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost),
                quality_score: Some(quality),
                notes: None,
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-unbound-tamper".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposals = task_params("proposals");
    proposals.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposals))
        .await
        .expect("proposals");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let first = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|proposal| proposal["kind"] == json!("route_policy"))
        })
        .expect("at least one route_policy proposal");
    let proposal_id = first["proposal_id"].as_str().expect("id").to_string();

    let mut approve = task_params("review_proposal");
    approve.proposal_id = Some(proposal_id.clone());
    approve.review_status = Some("approved".to_string());
    let _ = server
        .tachi_task(Parameters(approve))
        .await
        .expect("approve");

    // Tamper ONLY the unbound top-level `policy_rule.prefer_profile` field —
    // identity_payload / content_digest are left byte-for-byte untouched, so
    // the digest check at apply must still pass; the NEW display-drift check
    // is what must catch this.
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("json");
            value["policy_rule"]["prefer_profile"] = json!("attacker_controlled_profile");
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state(
                    tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                    &proposal_id,
                    &next,
                )
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .expect("tamper unbound top-level policy_rule");

    let (before_raw, before_version) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load proposal row")
        .expect("proposal row exists");

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("a drifted display copy must refuse to apply, even though the digest matches");
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );

    // No rule row was ever written for a refused apply.
    let rule_row = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::ROUTE_POLICY_RULE_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("query rule row");
    assert!(
        rule_row.is_none(),
        "a refused apply must never write a rule row into ROUTE_POLICY_RULE_NS: {rule_row:?}"
    );

    // The proposal row itself is byte-identical before and after the refusal.
    let (after_raw, after_version) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load proposal row")
        .expect("proposal row exists");
    assert_eq!(
        before_version, after_version,
        "a refused apply must not bump the row's state_version"
    );
    assert_eq!(
        before_raw, after_raw,
        "a refused apply must not mutate the stored row"
    );
}

/// Discrimination: the same display/bound drift must also be caught at
/// REVIEW time, not just apply — a human recording an approve/reject
/// decision is reading the display copy
/// (`handle_route_policy_proposals`/`handle_route_policy_review`'s response
/// echoes the row's top-level `policy_rule`/`evidence`), so if it drifted
/// before review, the decision being recorded does not cover the bound
/// content either.
///
/// Production path: `handle_route_policy_review` ->
/// `route_policy_display_drift`, checked immediately after the
/// content_digest re-validation and before the pending/terminal-state guard
/// and the status/review mutation.
/// Pre-fix red: review only re-validated `content_digest` (identity_payload
/// self-consistency); a display copy tampered before review — even one an
/// attacker softened specifically to slip past a human who would have
/// rejected the real payload — passed straight through to a recorded
/// approval.
/// Post-fix green: review is REFUSED with `display_copy_drift` and the row
/// (including state_version and status) is byte-identical before and after.
#[tokio::test]
async fn route_review_refuses_tampered_unbound_top_level_policy_rule() {
    let (server, _temp_home) = make_server_with_temp_home();

    for (task_id, profile, outcome, cost, quality) in [
        (
            "review-unbound-tamper-1",
            "opencode_builder",
            "success",
            0.01,
            0.80,
        ),
        (
            "review-unbound-tamper-2",
            "opencode_builder",
            "failure",
            0.01,
            0.20,
        ),
        (
            "review-unbound-tamper-3",
            "glm_51_impl",
            "success",
            2.00,
            0.98,
        ),
    ] {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(task_id.to_string()),
                task: "route policy review-time unbound-copy tamper fixture".to_string(),
                agent: "custom".to_string(),
                outcome: outcome.to_string(),
                task_type: Some("fix_request".to_string()),
                profile: Some(profile.to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(100_000),
                skills_used: vec!["skill:superpowers-executing-plans".to_string()],
                cost_tokens: Some(1000),
                cost_usd: Some(cost),
                quality_score: Some(quality),
                notes: None,
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-review-unbound-tamper".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/tachi-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposals = task_params("proposals");
    proposals.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposals))
        .await
        .expect("proposals");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let first = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|proposal| proposal["kind"] == json!("route_policy"))
        })
        .expect("at least one route_policy proposal");
    let proposal_id = first["proposal_id"].as_str().expect("id").to_string();

    // Tamper the display copy BEFORE any review — while the row is still
    // pending — leaving identity_payload/content_digest untouched.
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("json");
            value["policy_rule"]["prefer_profile"] = json!("softened_but_wrong_profile");
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state(
                    tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                    &proposal_id,
                    &next,
                )
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .expect("tamper unbound top-level policy_rule before review");

    let (before_raw, before_version) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load proposal row")
        .expect("proposal row exists");

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = server.tachi_task(Parameters(review)).await.expect_err(
        "review of a proposal whose display copy drifted from its bound copy must refuse",
    );
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );

    let (after_raw, after_version) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load proposal row")
        .expect("proposal row exists");
    assert_eq!(
        before_version, after_version,
        "a refused review must not bump the row's state_version"
    );
    assert_eq!(
        before_raw, after_raw,
        "a refused review must not mutate the stored row"
    );
    let after_value: serde_json::Value = serde_json::from_str(&after_raw).expect("row json");
    assert_eq!(
        after_value["status"],
        json!("pending"),
        "status must remain pending; the refused review must not record a decision"
    );
}

/// Discrimination: route proposals bind the complete active rule namespace,
/// not only their own payload. A source-rule edit after generation must refuse
/// both review and apply; regeneration must mint a distinct pending identity.
#[tokio::test]
async fn route_source_rule_drift_refuses_review_apply_and_rotates_pending_identity() {
    let (server, _temp_home) = make_server_with_temp_home();
    seed_route_policy_inputs(&server, "source-drift").await;

    let first = generate_route_policy_proposal(&server).await;
    let first_id = first["proposal_id"].as_str().expect("first id").to_string();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::ROUTE_POLICY_RULE_NS,
                    "third-party-source-rule",
                    &json!({"policy_rule": {"prefer_profile": "glm_impl"}}).to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("mutate active route source");

    let (before_review, before_review_version) = read_route_policy_row(&server, &first_id);
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(first_id.clone());
    review.review_status = Some("approved".to_string());
    let err = server
        .tachi_task(Parameters(review))
        .await
        .expect_err("review after active-route drift must refuse");
    assert!(
        err.contains("source_state_drift"),
        "unexpected review error: {err}"
    );
    assert_eq!(
        read_route_policy_row(&server, &first_id),
        (before_review, before_review_version),
        "review refusal must leave the pending row untouched"
    );

    let regenerated = generate_route_policy_proposal(&server).await;
    let regenerated_id = regenerated["proposal_id"]
        .as_str()
        .expect("regenerated id")
        .to_string();
    assert_ne!(
        first_id, regenerated_id,
        "source revision must rotate the id"
    );
    assert_eq!(regenerated["status"], json!("pending"));

    let mut approve = task_params("review_proposal");
    approve.proposal_id = Some(regenerated_id.clone());
    approve.review_status = Some("approved".to_string());
    server
        .tachi_task(Parameters(approve))
        .await
        .expect("approve regenerated proposal");
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::ROUTE_POLICY_RULE_NS,
                    "third-party-source-rule",
                    &json!({"policy_rule": {"prefer_profile": "opencode_builder"}}).to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("mutate route source after approval");

    let (before_apply, before_apply_version) = read_route_policy_row(&server, &regenerated_id);
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(regenerated_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("approved proposal must refuse after route-source drift");
    assert!(
        err.contains("source_state_drift"),
        "unexpected apply error: {err}"
    );
    assert_eq!(
        read_route_policy_row(&server, &regenerated_id),
        (before_apply, before_apply_version),
        "apply refusal must leave the approved row untouched"
    );
    let written_rule = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::ROUTE_POLICY_RULE_NS, &regenerated_id)
                .map_err(|e| e.to_string())
        })
        .expect("query applied route rule");
    assert!(
        written_rule.is_none(),
        "refused apply must not write a rule"
    );
}

/// Discrimination: a stored row can recompute a matching content digest while
/// naming an obsolete policy version. Current-policy validation must still
/// reject it at review and apply, before either lifecycle transition.
#[tokio::test]
async fn route_stale_current_policy_with_recomputed_digest_refuses_review_and_apply() {
    let (server, _temp_home) = make_server_with_temp_home();
    seed_route_policy_inputs(&server, "stale-policy").await;
    let proposal = generate_route_policy_proposal(&server).await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();

    let mutate_policy = |status: Option<&str>| {
        server
            .with_global_store(|store| {
                let (raw, _version) = store
                    .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                    .map_err(|e| e.to_string())?
                    .expect("proposal row");
                let mut value: serde_json::Value = serde_json::from_str(&raw).expect("row JSON");
                value["policy_version"] = json!("retired-route-policy");
                value["identity_payload"]["policy_version"] = json!("retired-route-policy");
                value["content_digest"] =
                    json!(test_route_content_digest(&value["identity_payload"]));
                if let Some(status) = status {
                    value["status"] = json!(status);
                }
                store
                    .set_state(
                        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                        &proposal_id,
                        &serde_json::to_string(&value).expect("serialize stale row"),
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("store stale but self-consistent proposal");
    };

    mutate_policy(None);
    let (before_review, before_review_version) = read_route_policy_row(&server, &proposal_id);
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = server
        .tachi_task(Parameters(review))
        .await
        .expect_err("stale current policy must refuse review");
    assert!(
        err.contains("current_policy_mismatch"),
        "unexpected review error: {err}"
    );
    assert_eq!(
        read_route_policy_row(&server, &proposal_id),
        (before_review, before_review_version),
        "review refusal must not mutate a self-consistent stale row"
    );

    // Restore the generated row, approve it through the real path, then make
    // the same internally consistent stale-policy mutation before apply.
    let regenerated = generate_route_policy_proposal(&server).await;
    assert_eq!(regenerated["status"], json!("pending"));
    let mut approve = task_params("review_proposal");
    approve.proposal_id = Some(proposal_id.clone());
    approve.review_status = Some("approved".to_string());
    server
        .tachi_task(Parameters(approve))
        .await
        .expect("approve restored proposal");
    mutate_policy(Some("approved"));
    let (before_apply, before_apply_version) = read_route_policy_row(&server, &proposal_id);
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("stale current policy must refuse apply");
    assert!(
        err.contains("current_policy_mismatch"),
        "unexpected apply error: {err}"
    );
    assert_eq!(
        read_route_policy_row(&server, &proposal_id),
        (before_apply, before_apply_version),
        "apply refusal must not mutate or terminalize a self-consistent stale row"
    );
}
