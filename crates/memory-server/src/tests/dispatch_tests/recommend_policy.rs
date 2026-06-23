use super::super::make_server;
use super::task_params;
use crate::tool_params::{
    TachiAgentsParams, TachiCompleteParams, TachiSkillParams, TachiSubagentEvalParams,
};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;
#[tokio::test]
async fn tachi_task_recommend_uses_live_eval_and_dispatch_profiles() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("recommend-review-001".to_string()),
            task: "Review dispatch profile implementation".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(1200),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.95),
            notes: Some("Found no blockers after verification.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-194".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval profile routing change".to_string());
    params.risk = Some("high".to_string());
    params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(
        rec["recommended_profile"],
        serde_json::json!("codex_55_review")
    );
    assert_eq!(rec["risk"], serde_json::json!("high"));
    assert!(rec["fallback_chain"]
        .as_array()
        .is_some_and(|chain| chain.iter().any(|item| item == "codex_55_review")));
    assert!(
        rec["live_eval"]["matched_samples"].as_u64().unwrap_or(0) >= 1,
        "expected live eval evidence in recommendation: {rec:#}"
    );
    assert!(
        rec["live_eval"]["performance_matrix_hits"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "expected live performance matrix evidence in recommendation: {rec:#}"
    );
    assert!(rec["reason"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| reason
            .as_str()
            .is_some_and(|s| s.contains("live_useful_rate")))));
    assert!(rec["resolved_skills"]
        .as_array()
        .is_some_and(|skills| skills
            .iter()
            .any(|skill| skill == "skill:superpowers-requesting-code-review")));
    assert_eq!(
        rec["resolved_skill_loadout"]["passive_traits"][0],
        serde_json::json!("strict_on_missing_tests")
    );
    let codex_candidate = rec["candidates"]
        .as_array()
        .and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["profile"] == "codex_55_review")
        })
        .expect("codex candidate should be present");
    assert_eq!(codex_candidate["performance_samples"], serde_json::json!(1));
    assert_eq!(
        codex_candidate["human_override_rate"],
        serde_json::json!(0.0)
    );
    assert_eq!(codex_candidate["avg_retry_count"], serde_json::json!(0.0));
    assert_eq!(codex_candidate["avg_latency_ms"], serde_json::json!(1200.0));
}

#[tokio::test]
async fn tachi_task_recommend_falls_back_to_builtin_profiles_without_eval_rows() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a low-risk documentation update".to_string());
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed without eval rows");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert!(rec["recommended_profile"].as_str().is_some());
    assert!(
        rec.as_object()
            .is_some_and(|obj| obj.contains_key("recommended_model")),
        "recommend should surface model choice: {rec:#}"
    );
    assert!(rec["recommended_transport"].as_str().is_some());
    assert_eq!(rec["live_eval"]["row_count"], serde_json::json!(0));
    assert!(
        rec["evidence_note"]
            .as_str()
            .is_some_and(|note| note.contains("low_sample_fallback")),
        "expected fallback note: {rec:#}"
    );
    assert!(rec["mbit_card"].is_object());
}

#[tokio::test]
async fn tachi_task_recommend_surfaces_kimi_ux_for_agent_experience_tasks() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some(
        "Run an agent-facing UX 大满贯 test for tachi_arena and summarize tool surface friction"
            .to_string(),
    );
    params.limit = Some(20);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    let candidates = rec["candidates"].as_array().expect("candidates");
    let kimi_ux = candidates
        .iter()
        .find(|candidate| candidate["profile"] == serde_json::json!("kimi_ux"))
        .expect("kimi_ux candidate");
    assert!(
        kimi_ux["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| {
                reason
                    .as_str()
                    .is_some_and(|s| s.contains("role_matches_agent_facing_ux"))
            })),
        "kimi_ux should explain UX routing fit: {kimi_ux:#}"
    );
    assert!(
        candidates
            .iter()
            .take(3)
            .any(|candidate| candidate["profile"] == serde_json::json!("kimi_ux")),
        "kimi_ux should be near the top for agent-facing UX tasks: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_route_simulate_compares_policy_variants_from_live_eval() {
    let server = make_server();

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
                evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
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
    assert_eq!(quality_choice["profile"], json!("glm_51_impl"));
    assert!(
        cost_sensitive["caveats"]
            .as_array()
            .is_some_and(|caveats| caveats.iter().any(|caveat| caveat
                .as_str()
                .is_some_and(|s| s.contains("low sample count")))),
        "low sample caveat should be visible: {sim:#}"
    );
}

#[tokio::test]
async fn tachi_task_route_policy_proposals_require_review_before_apply() {
    let server = make_server();

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
                evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
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

#[tokio::test]
async fn tachi_task_proposals_include_reviewable_loadout_evolution_candidates() {
    let server = make_server();

    for idx in 0..10 {
        let agent = if idx < 5 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-proposal-plan-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: agent.to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec![
                    "skill:superpowers-writing-plans".to_string(),
                    "skill:planning-ux-review".to_string(),
                ],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed loadout evolution proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-evolution-proposal".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed loadout eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    assert!(proposals["proposal_kinds"]
        .as_array()
        .expect("proposal kinds")
        .contains(&json!("loadout_evolution")));
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["skill_id"] == json!("skill:superpowers-writing-plans")
        }),
        "existing profile skills should not generate loadout evolution proposals: {proposal_items:?}"
    );
    let proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["skill_id"] == json!("skill:planning-ux-review")
            })
        })
        .expect("loadout evolution proposal");
    assert_eq!(proposal["status"], json!("pending"));
    assert_eq!(proposal["requires_human_approval"], json!(true));
    assert_eq!(
        proposal["operation"],
        json!("promote_observed_skill_to_signature")
    );
    assert_eq!(proposal["evidence"]["profile_samples"], json!(10));
    assert_eq!(proposal["evidence"]["skill_hits"], json!(10));
    assert_eq!(
        proposal["proposed_patch"]["add_signature_skills"][0],
        json!("skill:planning-ux-review")
    );
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let passive_proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_backed_passive_trait")
                    && proposal["trait_id"] == json!("evidence_backed_planning")
            })
        })
        .expect("passive trait evolution proposal");
    assert_eq!(
        proposal_items
            .iter()
            .filter(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_backed_passive_trait")
                    && proposal["trait_id"] == json!("evidence_backed_planning")
            })
            .count(),
        1,
        "duplicate agent/model matrix rows should not emit duplicate passive trait proposals: {proposal_items:?}"
    );
    assert_eq!(
        passive_proposal["proposed_patch"]["add_passive_traits"][0],
        json!("evidence_backed_planning")
    );
    let passive_proposal_id = passive_proposal["proposal_id"]
        .as_str()
        .expect("passive proposal id")
        .to_string();
    let evidence_proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_contract_required")
                    && proposal["evidence_id"] == json!("acceptance_criteria")
            })
        })
        .expect("evidence contract evolution proposal");
    assert_eq!(
        proposal_items
            .iter()
            .filter(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_contract_required")
                    && proposal["evidence_id"] == json!("acceptance_criteria")
            })
            .count(),
        1,
        "duplicate agent/model matrix rows should not emit duplicate evidence contract proposals: {proposal_items:?}"
    );
    assert_eq!(
        evidence_proposal["proposed_patch"]["add_evidence_required"][0],
        json!("acceptance_criteria")
    );
    let evidence_proposal_id = evidence_proposal["proposal_id"]
        .as_str()
        .expect("evidence proposal id")
        .to_string();

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("Human approved loadout evolution candidate.".to_string());
    let reviewed_raw = server
        .tachi_task(Parameters(review))
        .await
        .expect("review should succeed");
    let reviewed: serde_json::Value = serde_json::from_str(&reviewed_raw).expect("review JSON");
    assert_eq!(reviewed["proposal"]["status"], json!("approved"));
    assert_eq!(reviewed["proposal"]["kind"], json!("loadout_evolution"));

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply))
        .await
        .expect("approved loadout evolution should project");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply JSON");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["profile_card_mutated"], json!(true));
    assert_eq!(
        applied["projection_namespace"],
        json!("dispatch_profile_card_overlays")
    );
    assert_eq!(applied["proposal"]["status"], json!("applied"));
    assert_eq!(
        applied["proposal"]["projection"]["status"],
        json!("applied_profile_card_overlay")
    );

    let mut second_apply = task_params("apply_proposals");
    second_apply.proposal_id = Some(proposal_id.clone());
    second_apply.confirm = true;
    let err = server
        .tachi_task(Parameters(second_apply))
        .await
        .expect_err("applied proposal should require a fresh approved proposal");
    assert!(err.contains("must be approved before apply"), "{err}");

    let mut passive_review = task_params("review_proposal");
    passive_review.proposal_id = Some(passive_proposal_id.clone());
    passive_review.review_status = Some("approved".to_string());
    passive_review.notes = Some("Human approved passive trait projection.".to_string());
    let passive_reviewed_raw = server
        .tachi_task(Parameters(passive_review))
        .await
        .expect("passive review should succeed");
    let passive_reviewed: serde_json::Value =
        serde_json::from_str(&passive_reviewed_raw).expect("passive review JSON");
    assert_eq!(
        passive_reviewed["proposal"]["operation"],
        json!("add_evidence_backed_passive_trait")
    );

    let mut passive_apply = task_params("apply_proposals");
    passive_apply.proposal_id = Some(passive_proposal_id);
    passive_apply.confirm = true;
    let passive_applied_raw = server
        .tachi_task(Parameters(passive_apply))
        .await
        .expect("approved passive trait should project");
    let passive_applied: serde_json::Value =
        serde_json::from_str(&passive_applied_raw).expect("passive apply JSON");
    assert_eq!(
        passive_applied["proposal"]["projection"]["added_passive_traits"][0],
        json!("evidence_backed_planning")
    );

    let mut evidence_review = task_params("review_proposal");
    evidence_review.proposal_id = Some(evidence_proposal_id.clone());
    evidence_review.review_status = Some("approved".to_string());
    evidence_review.notes = Some("Human approved evidence contract projection.".to_string());
    let evidence_reviewed_raw = server
        .tachi_task(Parameters(evidence_review))
        .await
        .expect("evidence review should succeed");
    let evidence_reviewed: serde_json::Value =
        serde_json::from_str(&evidence_reviewed_raw).expect("evidence review JSON");
    assert_eq!(
        evidence_reviewed["proposal"]["operation"],
        json!("add_evidence_contract_required")
    );

    let mut evidence_apply = task_params("apply_proposals");
    evidence_apply.proposal_id = Some(evidence_proposal_id);
    evidence_apply.confirm = true;
    let evidence_applied_raw = server
        .tachi_task(Parameters(evidence_apply))
        .await
        .expect("approved evidence contract should project");
    let evidence_applied: serde_json::Value =
        serde_json::from_str(&evidence_applied_raw).expect("evidence apply JSON");
    assert_eq!(
        evidence_applied["proposal"]["projection"]["added_evidence_required"][0],
        json!("acceptance_criteria")
    );

    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("plan a dispatch loadout evolution slice".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
            profile: Some("claude_plan".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(2),
            pack_limit: Some(1),
            include_section: Some(false),
        }))
        .await
        .expect("loadout should include projected skill");
    let loadout: serde_json::Value = serde_json::from_str(&loadout_raw).expect("loadout JSON");
    assert!(loadout["resolved_skills"]
        .as_array()
        .expect("resolved skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(loadout["skill_loadout"]["projected_signature_skills"]
        .as_array()
        .expect("projected signature skills")
        .contains(&json!("skill:planning-ux-review")));
    assert_eq!(
        loadout["skill_loadout"]["projection"]["status"],
        json!("applied_overlay")
    );
    assert!(loadout["skill_loadout"]["passive_traits"]
        .as_array()
        .expect("passive traits")
        .contains(&json!("evidence_backed_planning")));
    assert!(loadout["skill_loadout"]["projected_passive_traits"]
        .as_array()
        .expect("projected passive traits")
        .contains(&json!("evidence_backed_planning")));
    assert!(
        loadout["mbit_card"]["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("mbit projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(loadout["evidence_required"]
        .as_array()
        .expect("loadout evidence required")
        .contains(&json!("acceptance_criteria")));
    assert!(loadout["evidence_contract"]["projected_required"]
        .as_array()
        .expect("loadout projected evidence")
        .contains(&json!("acceptance_criteria")));
    assert!(
        loadout["mbit_card"]["evidence_contract"]["projected_required"]
            .as_array()
            .expect("mbit projected evidence")
            .contains(&json!("acceptance_criteria"))
    );

    let profiles_raw = server
        .tachi_task(Parameters(task_params("profiles")))
        .await
        .expect("profiles should include projected loadout");
    let profiles: serde_json::Value = serde_json::from_str(&profiles_raw).expect("profiles JSON");
    let claude_profile = profiles["dispatch_profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan profile");
    assert!(claude_profile["skill_loadout"]["signature_skills"]
        .as_array()
        .expect("signature skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(
        claude_profile["mbit_card"]["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("profile mbit projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(claude_profile["evidence_contract"]["projected_required"]
        .as_array()
        .expect("profile projected evidence")
        .contains(&json!("acceptance_criteria")));
    assert!(
        claude_profile["mbit_card"]["evidence_contract"]["projected_required"]
            .as_array()
            .expect("profile mbit projected evidence")
            .contains(&json!("acceptance_criteria"))
    );

    let mut recommend_params = task_params("recommend");
    recommend_params.task = Some("Plan a dispatch loadout evolution slice".to_string());
    recommend_params.limit = Some(50);
    let recommend_raw = server
        .tachi_task(Parameters(recommend_params))
        .await
        .expect("recommend should include projected loadout");
    let recommend: serde_json::Value =
        serde_json::from_str(&recommend_raw).expect("recommend JSON");
    assert!(recommend["resolved_skills"]
        .as_array()
        .expect("recommend resolved skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(
        recommend["resolved_skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("recommend projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(
        recommend["mbit_card"]["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("recommend mbit projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(recommend["evidence_required"]
        .as_array()
        .expect("recommend evidence required")
        .contains(&json!("acceptance_criteria")));
    assert!(recommend["evidence_contract"]["projected_required"]
        .as_array()
        .expect("recommend projected evidence")
        .contains(&json!("acceptance_criteria")));
    assert!(
        recommend["mbit_card"]["evidence_contract"]["projected_required"]
            .as_array()
            .expect("recommend mbit projected evidence")
            .contains(&json!("acceptance_criteria"))
    );

    let agents_raw = server
        .tachi_agents(Parameters(TachiAgentsParams {
            action: "profiles".to_string(),
            intent: None,
            task: None,
        }))
        .await
        .expect("legacy agents registry should include projected loadout");
    let agents: serde_json::Value = serde_json::from_str(&agents_raw).expect("agents JSON");
    let agent_claude_profile = agents["dispatch_profiles"]
        .as_array()
        .expect("agent profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan in agent registry");
    assert!(agent_claude_profile["skill_loadout"]["signature_skills"]
        .as_array()
        .expect("agent signature skills")
        .contains(&json!("skill:planning-ux-review")));
    assert!(
        agent_claude_profile["skill_loadout"]["projected_passive_traits"]
            .as_array()
            .expect("agent projected passive traits")
            .contains(&json!("evidence_backed_planning"))
    );
    assert!(
        agent_claude_profile["evidence_contract"]["projected_required"]
            .as_array()
            .expect("agent projected evidence")
            .contains(&json!("acceptance_criteria"))
    );
}

#[tokio::test]
async fn tachi_task_proposals_requires_loadout_evolution_sample_threshold() {
    let server = make_server();

    for idx in 0..9 {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-proposal-below-threshold-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: "claude".to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec![
                    "skill:superpowers-writing-plans".to_string(),
                    "skill:planning-ux-review".to_string(),
                ],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed below-threshold loadout proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-evolution-threshold".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed below-threshold loadout eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["skill_id"] == json!("skill:planning-ux-review")
        }),
        "loadout evolution proposal should require at least 10 profile samples: {proposal_items:?}"
    );
}

#[tokio::test]
async fn tachi_task_proposals_project_card_weakness_and_demotion_targets() {
    let server = make_server();

    for idx in 0..3 {
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("card-risk-plan-failure-{idx}")),
                task: "Plan a dispatch card risk slice".to_string(),
                agent: "claude".to_string(),
                outcome: "failure".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:superpowers-writing-plans".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.20),
                notes: Some("Seed card weakness and demotion proposal fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-card-risk-evolution".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: Vec::new(),
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed card risk eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    let weakness_proposal = proposal_items
        .iter()
        .find(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["operation"] == json!("add_card_weakness")
                && proposal["weakness_id"] == json!("plan_request")
        })
        .expect("card weakness proposal");
    let demotion_proposal = proposal_items
        .iter()
        .find(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["profile"] == json!("claude_plan")
                && proposal["operation"] == json!("mark_skill_demotion_target")
                && proposal["skill_id"] == json!("skill:superpowers-writing-plans")
        })
        .expect("skill demotion proposal");
    assert_eq!(
        weakness_proposal["proposed_patch"]["add_weak_against"][0],
        json!("plan_request")
    );
    assert_eq!(
        demotion_proposal["proposed_patch"]["demotion_targets"][0],
        json!("skill:superpowers-writing-plans")
    );

    for proposal in [weakness_proposal, demotion_proposal] {
        let proposal_id = proposal["proposal_id"].as_str().expect("proposal id");
        let mut review = task_params("review_proposal");
        review.proposal_id = Some(proposal_id.to_string());
        review.review_status = Some("approved".to_string());
        review.notes = Some("Human approved card risk projection.".to_string());
        server
            .tachi_task(Parameters(review))
            .await
            .expect("review should succeed");

        let mut apply = task_params("apply_proposals");
        apply.proposal_id = Some(proposal_id.to_string());
        apply.confirm = true;
        server
            .tachi_task(Parameters(apply))
            .await
            .expect("card risk projection should apply");
    }

    let profiles_raw = server
        .tachi_task(Parameters(task_params("profiles")))
        .await
        .expect("profiles should include card risk projections");
    let profiles: serde_json::Value = serde_json::from_str(&profiles_raw).expect("profiles JSON");
    let claude_profile = profiles["dispatch_profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan profile");
    assert!(
        claude_profile["mbit_card"]["stats"]["risk_control"]
            .as_i64()
            .expect("risk_control stat")
            > 0
    );
    assert!(claude_profile["weak_against"]
        .as_array()
        .expect("merged weak_against")
        .contains(&json!("plan_request")));
    assert!(claude_profile["mbit_card"]["projected_weak_against"]
        .as_array()
        .expect("projected weak_against")
        .contains(&json!("plan_request")));
    assert!(claude_profile["mbit_card"]["demotion_targets"]
        .as_array()
        .expect("demotion targets")
        .contains(&json!("skill:superpowers-writing-plans")));

    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("Plan a dispatch card risk slice".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
            profile: Some("claude_plan".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(2),
            pack_limit: Some(1),
            include_section: Some(false),
        }))
        .await
        .expect("loadout should include projected weakness");
    let loadout: serde_json::Value = serde_json::from_str(&loadout_raw).expect("loadout JSON");
    assert_eq!(
        loadout["weak_against"], loadout["mbit_card"]["weak_against"],
        "top-level loadout weak_against should match the MBIT card"
    );
    assert!(loadout["weak_against"]
        .as_array()
        .expect("loadout weak_against")
        .contains(&json!("plan_request")));

    let mut recommend_params = task_params("recommend");
    recommend_params.task = Some("Plan a dispatch card risk slice".to_string());
    recommend_params.limit = Some(50);
    let recommend_raw = server
        .tachi_task(Parameters(recommend_params))
        .await
        .expect("recommend should include weak_against penalty");
    let recommend: serde_json::Value =
        serde_json::from_str(&recommend_raw).expect("recommend JSON");
    let claude_candidate = recommend["candidates"]
        .as_array()
        .expect("candidate list")
        .iter()
        .find(|candidate| candidate["profile"] == json!("claude_plan"))
        .expect("claude_plan candidate");
    assert!(claude_candidate["reasons"]
        .as_array()
        .expect("candidate reasons")
        .contains(&json!("weak_against_signal:plan_request")));
}

#[tokio::test]
async fn tachi_task_recommend_consumes_approved_route_policy_rules() {
    let server = make_server();

    for (task_id, profile, outcome, cost_usd, quality_score) in [
        (
            "rule-loader-cheap-success",
            "opencode_builder",
            "success",
            0.01,
            0.75,
        ),
        (
            "rule-loader-cheap-failure",
            "opencode_builder",
            "failure",
            0.01,
            0.20,
        ),
        (
            "rule-loader-quality-success",
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
                notes: Some("Seed route policy rule loader fixture.".to_string()),
                trajectory: None,
                diff: Some("diff --git a/x b/x".to_string()),
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-route-policy-loader".to_string()),
                issue_ref: Some("kckylechen1/tachi#194".to_string()),
                pr_ref: None,
                evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
                tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
                diff_present: Some(true),
                scope: Some("project".to_string()),
                project: None,
            }))
            .await
            .expect("seed eval row");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let proposals_raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value =
        serde_json::from_str(&proposals_raw).expect("proposals JSON");
    let proposal_id = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["policy"] == json!("cost_sensitive")
                    && proposal["task_type"] == json!("fix_request")
                    && proposal["proposed_profile"] == json!("opencode_builder")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("cost-sensitive opencode proposal")
        .to_string();

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    server
        .tachi_task(Parameters(review))
        .await
        .expect("review should succeed");

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let applied_raw = server
        .tachi_task(Parameters(apply))
        .await
        .expect("apply should succeed");
    let applied: serde_json::Value = serde_json::from_str(&applied_raw).expect("apply JSON");
    assert_eq!(applied["routing_mutated"], json!(true));

    let mut recommend = task_params("recommend");
    recommend.task = Some("fix dispatch policy proposal flow bug".to_string());
    recommend.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(recommend))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["recommended_profile"], json!("opencode_builder"));
    assert!(rec["evidence_note"]
        .as_str()
        .is_some_and(|note| note.contains("route_policy_weighted")));
    assert!(rec["route_policy_rules"]["applied"]
        .as_array()
        .is_some_and(|rules| rules
            .iter()
            .any(|rule| rule["proposal_id"] == json!(proposal_id))));
    let opencode_candidate = rec["candidates"]
        .as_array()
        .and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["profile"] == json!("opencode_builder"))
        })
        .expect("opencode candidate should be present");
    assert!(
        opencode_candidate["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("approved_route_policy_rule")))),
        "approved route policy rule should be visible in candidate reasons: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_skips_route_policy_rules_blocked_by_risk() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    "dispatch_route_policy_rules",
                    "route_policy:test_request:codex_53_fast",
                    &json!({
                        "proposal_id": "route_policy:test_request:codex_53_fast",
                        "kind": "route_policy",
                        "status": "applied",
                        "review": {
                            "status": "approved",
                            "reviewed_at": Utc::now().to_rfc3339(),
                        },
                        "policy": "cost_sensitive",
                        "task_type": "test_request",
                        "proposed_profile": "codex_53_fast",
                        "score_delta": 99.0,
                        "policy_rule": {
                            "when_task_type": "test_request",
                            "prefer_profile": "codex_53_fast",
                            "policy": "cost_sensitive",
                            "fallback_to_current_profile": "codex_55_review",
                        },
                        "evidence": {
                            "source": "test",
                            "row_count": 5,
                            "proposed": {
                                "samples": 5
                            }
                        }
                    })
                    .to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed blocked route policy rule");

    let mut params = task_params("recommend");
    params.task = Some("test vault crypto migration regression".to_string());
    params.doc_paths =
        vec!["docs/engineering/architecture/agent-credential-surfaces.md".to_string()];
    params.spec_paths = vec!["crates/memory-server/src/vault_crypto.rs".to_string()];
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_ne!(rec["recommended_profile"], json!("codex_53_fast"));
    assert!(rec["route_policy_rules"]["applied"]
        .as_array()
        .is_some_and(|rules| rules.is_empty()));
    assert!(rec["route_policy_rules"]["skipped"]
        .as_array()
        .is_some_and(|rules| rules.iter().any(|rule| rule["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("blocked_by_risk_classifier")))));
}

#[tokio::test]
async fn tachi_task_recommend_escalates_risk_from_doc_and_spec_paths() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.doc_paths =
        vec!["docs/engineering/architecture/agent-credential-surfaces.md".to_string()];
    params.spec_paths = vec!["crates/memory-server/src/vault_crypto.rs".to_string()];
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed with file context");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["risk"], serde_json::json!("high"));
    assert!(rec["risk_reasons"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| reason
            .as_str()
            .is_some_and(|s| s == "touches vault/secrets boundary"))));
    assert!(rec["blocked_profiles"]
        .as_array()
        .is_some_and(|profiles| profiles.iter().any(|profile| profile == "codex_53_fast")));
    assert!(
        rec["recommended_profile"] != serde_json::json!("codex_53_fast"),
        "high-risk path context must not recommend the fast lane: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_routes_low_risk_review_to_fast_checker_without_live_rows() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("review low-risk docs wording".to_string());
    params.risk = Some("low".to_string());
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["task_type"], json!("review_request"));
    assert_eq!(rec["risk"], json!("low"));
    assert_eq!(rec["recommended_profile"], json!("codex_53_fast"));
    assert!(rec["required_profiles"]
        .as_array()
        .expect("required profiles")
        .contains(&json!("codex_53_fast")));
    assert!(rec["blocked_profiles"]
        .as_array()
        .expect("blocked profiles")
        .is_empty());
}

#[tokio::test]
async fn tachi_task_recommend_surfaces_human_override_and_retry_penalties() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("recommend-override-retry-001".to_string()),
            task: "Review dispatch/eval routing regression".to_string(),
            agent: "leader".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(9000),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: Some(2000),
            cost_usd: Some(0.05),
            quality_score: Some(0.85),
            notes: Some("Reviewer needed human correction and retries.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "senior_reviewer".to_string(),
                agent: "codex".to_string(),
                model: None,
                task: Some("review dispatch policy".to_string()),
                task_type: Some("review_request".to_string()),
                outcome: Some("useful".to_string()),
                usefulness_score: Some(0.7),
                failure_mode: None,
                verification_impact: Some("modified".to_string()),
                verification_present: true,
                evaluator: Some("leader".to_string()),
                plan_delta: Some("modified".to_string()),
                human_override: true,
                retry_count: 3,
                notes: None,
                latency_ms: Some(8000),
                input_tokens: Some(1500),
                output_tokens: Some(500),
                cost_tokens: Some(2000),
                cost_usd: Some(0.05),
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-194".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec!["human review".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval routing change".to_string());
    params.risk = Some("high".to_string());
    params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let codex_candidate = rec["candidates"]
        .as_array()
        .and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["profile"] == "codex_55_review")
        })
        .expect("codex candidate should be present");

    assert_eq!(codex_candidate["performance_samples"], serde_json::json!(2));
    assert!(
        codex_candidate["human_override_rate"]
            .as_f64()
            .is_some_and(|rate| rate > 0.0),
        "human override telemetry should be surfaced: {codex_candidate:#}"
    );
    assert!(
        codex_candidate["avg_retry_count"]
            .as_f64()
            .is_some_and(|count| count > 0.0),
        "retry telemetry should be surfaced: {codex_candidate:#}"
    );
    assert!(
        codex_candidate["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("perf_human_override_rate")))),
        "human override should affect routing reasons: {codex_candidate:#}"
    );
    assert!(
        codex_candidate["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("perf_avg_retry_count")))),
        "retry should affect routing reasons: {codex_candidate:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_does_not_apply_same_backend_wrong_role_subagent_evidence() {
    let server = make_server();

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("recommend-role-guard-001".to_string()),
            task: "Implement dispatch profile routing".to_string(),
            agent: "leader".to_string(),
            outcome: "success".to_string(),
            task_type: Some("review_request".to_string()),
            profile: None,
            risk: Some("high".to_string()),
            duration_ms: Some(1800),
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: Some("Executor was useful, but not review evidence.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "executor".to_string(),
                agent: "codex".to_string(),
                model: None,
                task: Some("draft implementation".to_string()),
                task_type: Some("review_request".to_string()),
                outcome: Some("useful".to_string()),
                usefulness_score: Some(1.0),
                failure_mode: None,
                verification_impact: Some("accepted".to_string()),
                verification_present: true,
                evaluator: Some("leader".to_string()),
                plan_delta: Some("accepted".to_string()),
                human_override: false,
                retry_count: 0,
                notes: None,
                latency_ms: None,
                input_tokens: None,
                output_tokens: None,
                cost_tokens: None,
                cost_usd: None,
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-role-guard".to_string()),
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec!["crates/memory-server/src/dispatch_profile.rs".to_string()],
            tests_run: vec!["cargo test -p memory-server dispatch".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("seed eval row");

    let mut params = task_params("recommend");
    params.task = Some("review dispatch/eval profile routing change".to_string());
    params.risk = Some("high".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let reviewer = rec["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["profile"] == serde_json::json!("codex_55_review"))
        .expect("codex reviewer candidate");

    assert_eq!(
        reviewer["live_samples"],
        serde_json::json!(0),
        "executor subagent evidence must not count as reviewer profile evidence: {rec:#}"
    );
    assert!(
        reviewer["reason"]
            .as_array()
            .is_none_or(|reasons| !reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|s| s.contains("live_subagent_evidence")))),
        "wrong-role subagent evidence leaked into reviewer reasons: {reviewer:#}"
    );
}
