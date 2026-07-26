use super::*;
use crate::MemoryServer;

async fn mint_loadout_v3_fixture(server: &MemoryServer, fixture: &str) -> String {
    for idx in 0..10 {
        let agent = if idx < 5 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-v3-{fixture}-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: agent.to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:planning-ux-review".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some(format!("Seed {fixture} loadout identity fixture.")),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some(format!("flow-loadout-v3-{fixture}")),
                issue_ref: Some("kckylechen1/tachi#1431".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
            }))
            .await
            .expect("seed loadout identity fixture");
    }

    let mut proposal_params = task_params("proposals");
    proposal_params.limit = Some(50);
    let raw = server
        .tachi_task(Parameters(proposal_params))
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["operation"] == json!("promote_observed_skill_to_signature")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("minted loadout proposal id")
        .to_string()
}

async fn approve_loadout_v3_fixture(server: &MemoryServer, proposal_id: &str) {
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.to_string());
    review.review_status = Some("approved".to_string());
    server
        .tachi_task(Parameters(review))
        .await
        .expect("loadout proposal approval should succeed");
}

fn read_loadout_state(server: &MemoryServer, namespace: &str, key: &str) -> Option<(String, u32)> {
    server
        .with_global_store_read(|store| {
            store
                .get_state_kv(namespace, key)
                .map_err(|err| err.to_string())
        })
        .expect("read loadout fixture state")
}

fn install_loadout_trigger(server: &MemoryServer, sql: &str) {
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute_batch(sql)
                .map_err(|err| err.to_string())
        })
        .expect("install loadout transaction trigger");
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
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
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
    assert_eq!(proposal["schema_version"], json!(3));
    assert_eq!(
        proposal["policy_version"],
        json!("2026-07-loadout-evolution-v3")
    );
    assert_eq!(proposal["target"], json!("profile_card_overlay"));
    assert_eq!(
        proposal["identity_payload"]["kind"],
        json!("loadout_evolution")
    );
    assert_eq!(
        proposal["identity_payload"]["apply_payload"]["profile"],
        proposal["profile"]
    );
    assert_eq!(
        proposal["identity_payload"]["apply_payload"]["skill_id"],
        proposal["skill_id"]
    );
    assert_eq!(
        proposal["identity_payload"]["apply_payload"]["proposed_patch"],
        proposal["proposed_patch"]
    );
    assert_eq!(
        proposal["identity_payload"]["evidence_review"],
        proposal["evidence"]
    );
    assert!(
        proposal["content_digest"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64),
        "minted loadout proposal must expose its SHA-256 content digest: {proposal:?}"
    );
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    assert!(
        proposal_id.starts_with("loadout_evolution:v3:"),
        "loadout proposal id must be content-addressed: {proposal_id}"
    );
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

    let overlay_before_second_apply = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay before second apply");
    let mut second_apply = task_params("apply_proposals");
    second_apply.proposal_id = Some(proposal_id.clone());
    second_apply.confirm = true;
    let err = server
        .tachi_task(Parameters(second_apply))
        .await
        .expect_err("applied proposal should require a fresh approved proposal");
    assert!(err.contains("must be approved before apply"), "{err}");
    let overlay_after_second_apply = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay after second apply");
    assert_eq!(
        overlay_before_second_apply, overlay_after_second_apply,
        "a refused second apply must not mutate the already projected overlay"
    );

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

    // tachi#1173 item 2 slimmed action='profiles' rows to name/backend/model/role
    // by default; this assertion needs the full mbit_card/skill_loadout, so
    // request the verbose escape hatch explicitly (#1182 consumer sweep).
    let mut profiles_params = task_params("profiles");
    profiles_params.verbose = Some(true);
    let profiles_raw = server
        .tachi_task(Parameters(profiles_params))
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
    // tachi#1201 item 2: mbit_card is no longer embedded by default; this
    // assertion needs it, so request the explicit escape hatch.
    recommend_params.include_card = Some(true);
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

/// Discrimination: the shipped `review_proposal` / `apply_proposals` facade
/// must bind the exact loadout payload a human approved. On the pre-#1431
/// implementation, changing only the top-level skill id after approval was
/// accepted and projected into the durable overlay.
#[tokio::test]
async fn loadout_apply_refuses_tampered_payload_without_overlay_mutation() {
    let server = make_server();
    for idx in 0..10 {
        let agent = if idx < 5 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-tamper-plan-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: agent.to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:planning-ux-review".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed loadout payload-tamper fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-tamper".to_string()),
                issue_ref: Some("kckylechen1/tachi#1431".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
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
    let proposal_id = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["operation"] == json!("promote_observed_skill_to_signature")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("minted loadout proposal id")
        .to_string();

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    server
        .tachi_task(Parameters(review))
        .await
        .expect("approve loadout proposal");

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("loadout proposal row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["skill_id"] = json!("skill:attacker-controlled");
            store
                .set_state(
                    tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize tampered proposal"),
                )
                .map_err(|e| e.to_string())
        })
        .expect("tamper only the display payload after approval");

    let before_proposal = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("read proposal before refused apply");
    let before_overlay = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay before refused apply");

    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("a payload changed after approval must refuse");
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );

    let after_proposal = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("read proposal after refused apply");
    let after_overlay = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay after refused apply");
    assert_eq!(
        before_proposal, after_proposal,
        "a refused apply must not mutate the proposal row"
    );
    assert_eq!(
        before_overlay, after_overlay,
        "a refused apply must leave overlay bytes and state version unchanged"
    );
}

#[tokio::test]
async fn legacy_loadout_rows_stay_listable_but_refuse_review_and_apply() {
    let server = make_server();
    let pending_id = "loadout_evolution:legacy:pending";
    let approved_id = "loadout_evolution:legacy:approved";
    server
        .with_global_store(|store| {
            for (proposal_id, status) in [(pending_id, "pending"), (approved_id, "approved")] {
                store
                    .set_state(
                        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                        proposal_id,
                        &json!({
                            "proposal_id": proposal_id,
                            "kind": "loadout_evolution",
                            "status": status,
                            "profile": "claude_plan",
                            "operation": "promote_observed_skill_to_signature",
                            "skill_id": "skill:legacy-loadout",
                            "proposed_patch": { "add_signature_skills": ["skill:legacy-loadout"] },
                            "evidence": { "source": "legacy-fixture" },
                        })
                        .to_string(),
                    )
                    .map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(())
        })
        .expect("seed legacy loadout rows");

    let mut list = task_params("proposals");
    list.limit = Some(50);
    let listed_raw = server
        .tachi_task(Parameters(list))
        .await
        .expect("legacy rows remain listable");
    let listed: serde_json::Value = serde_json::from_str(&listed_raw).expect("proposal list JSON");
    for proposal_id in [pending_id, approved_id] {
        let row = listed["proposals"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["proposal_id"] == json!(proposal_id))
            })
            .expect("legacy loadout row remains listable");
        assert_eq!(row["legacy_unbound_proposal"], json!(true), "{row:?}");
    }

    let review_before = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, pending_id)
                .map_err(|e| e.to_string())
        })
        .expect("read legacy review row");
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(pending_id.to_string());
    review.review_status = Some("approved".to_string());
    let review_err = server
        .tachi_task(Parameters(review))
        .await
        .expect_err("legacy loadout row must not be reviewable");
    assert!(
        review_err.contains("legacy_unbound_proposal"),
        "expected loud legacy refusal, got: {review_err}"
    );
    let review_after = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, pending_id)
                .map_err(|e| e.to_string())
        })
        .expect("read legacy review row after refusal");
    assert_eq!(
        review_before, review_after,
        "a refused legacy review must not mutate its row"
    );

    let apply_before = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, approved_id)
                .map_err(|e| e.to_string())
        })
        .expect("read legacy apply row");
    let overlay_before = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay before legacy apply");
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(approved_id.to_string());
    apply.confirm = true;
    let apply_err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("legacy loadout row must not be applicable");
    assert!(
        apply_err.contains("legacy_unbound_proposal"),
        "expected loud legacy refusal, got: {apply_err}"
    );
    let apply_after = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, approved_id)
                .map_err(|e| e.to_string())
        })
        .expect("read legacy apply row after refusal");
    let overlay_after = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay after legacy apply");
    assert_eq!(
        apply_before, apply_after,
        "a refused legacy apply must not mutate its proposal row"
    );
    assert_eq!(
        overlay_before, overlay_after,
        "a refused legacy apply must leave overlay bytes and state version unchanged"
    );
}

#[tokio::test]
async fn loadout_review_refuses_evidence_drift_without_partial_mutation() {
    let server = make_server();
    for idx in 0..10 {
        let agent = if idx < 5 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("loadout-evidence-drift-plan-{idx}")),
                task: "Plan a dispatch loadout evolution slice".to_string(),
                agent: agent.to_string(),
                outcome: "success".to_string(),
                task_type: Some("plan_request".to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:planning-ux-review".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some("Seed loadout evidence-drift fixture.".to_string()),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some("flow-loadout-evidence-drift".to_string()),
                issue_ref: Some("kckylechen1/tachi#1431".to_string()),
                pr_ref: None,
                evidence_refs: vec![
                    "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
                ],
                tests_run: vec!["cargo test -p tachi-server dispatch".to_string()],
                diff_present: Some(false),
                scope: Some("project".to_string()),
                project: None,
                format: None,
                signatures: Vec::new(),
                rulings: Vec::new(),
                adjudication: None,
                eval_run_ids: Vec::new(),
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
    let proposal_id = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("loadout_evolution")
                    && proposal["operation"] == json!("promote_observed_skill_to_signature")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("minted loadout proposal id")
        .to_string();

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("loadout proposal row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["evidence"]["source"] = json!("attacker-controlled-evidence");
            store
                .set_state(
                    tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize tampered proposal"),
                )
                .map_err(|e| e.to_string())
        })
        .expect("tamper only reviewer-visible evidence before review");

    let before_proposal = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("read proposal before refused review");
    let before_overlay = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay before refused review");

    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = server
        .tachi_task(Parameters(review))
        .await
        .expect_err("evidence drift before review must refuse");
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );

    let after_proposal = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("read proposal after refused review");
    let after_overlay = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay after refused review");
    assert_eq!(
        before_proposal, after_proposal,
        "a refused review must not mutate the proposal row"
    );
    assert_eq!(
        before_overlay, after_overlay,
        "a refused review must leave overlay bytes and state version unchanged"
    );
}

#[tokio::test]
async fn loadout_v3_review_refuses_baseline_display_tamper_without_mutation() {
    let server = make_server();
    let proposal_id = mint_loadout_v3_fixture(&server, "baseline-display-tamper").await;

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|err| err.to_string())?
                .expect("loadout proposal row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["current_loadout"]["signature_skills"] =
                json!(["skill:attacker-controlled-baseline"]);
            store
                .set_state(
                    tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize tampered proposal"),
                )
                .map_err(|err| err.to_string())
        })
        .expect("tamper reviewer-visible baseline");

    let proposal_before = read_loadout_state(
        &server,
        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        &proposal_id,
    );
    let overlay_before = read_loadout_state(
        &server,
        tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        "claude_plan",
    );
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = server
        .tachi_task(Parameters(review))
        .await
        .expect_err("baseline display tamper must refuse review");
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );
    assert_eq!(
        proposal_before,
        read_loadout_state(
            &server,
            tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            &proposal_id,
        ),
        "refused baseline-tamper review must preserve proposal bytes and version"
    );
    assert_eq!(
        overlay_before,
        read_loadout_state(
            &server,
            tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
            "claude_plan",
        ),
        "refused baseline-tamper review must preserve overlay bytes and version"
    );
}

#[tokio::test]
async fn loadout_v3_review_refuses_live_overlay_drift_without_mutation() {
    let server = make_server();
    let proposal_id = mint_loadout_v3_fixture(&server, "overlay-drift-review").await;
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
                    "claude_plan",
                    r#"{"kind":"profile_card_loadout_overlay","profile":"claude_plan","source_proposal_ids":["concurrent-review-writer"]}"#,
                )
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
        .expect("drift live overlay before review");

    let proposal_before = read_loadout_state(
        &server,
        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        &proposal_id,
    );
    let overlay_before = read_loadout_state(
        &server,
        tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        "claude_plan",
    );
    let mut review = task_params("review_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = server
        .tachi_task(Parameters(review))
        .await
        .expect_err("overlay drift before review must refuse");
    assert!(
        err.contains("source_state_drift"),
        "expected source_state_drift refusal, got: {err}"
    );
    assert_eq!(
        proposal_before,
        read_loadout_state(
            &server,
            tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            &proposal_id,
        ),
        "refused overlay-drift review must preserve proposal bytes and version"
    );
    assert_eq!(
        overlay_before,
        read_loadout_state(
            &server,
            tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
            "claude_plan",
        ),
        "refused overlay-drift review must preserve overlay bytes and version"
    );
}

#[tokio::test]
async fn loadout_v3_apply_refuses_live_overlay_drift_without_mutation() {
    let server = make_server();
    let proposal_id = mint_loadout_v3_fixture(&server, "overlay-drift-apply").await;
    approve_loadout_v3_fixture(&server, &proposal_id).await;
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
                    "claude_plan",
                    r#"{"kind":"profile_card_loadout_overlay","profile":"claude_plan","source_proposal_ids":["concurrent-apply-writer"]}"#,
                )
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
        .expect("drift live overlay before apply");

    let proposal_before = read_loadout_state(
        &server,
        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        &proposal_id,
    );
    let overlay_before = read_loadout_state(
        &server,
        tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        "claude_plan",
    );
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("overlay drift before apply must refuse");
    assert!(
        err.contains("source_state_drift"),
        "expected source_state_drift refusal, got: {err}"
    );
    assert_eq!(
        proposal_before,
        read_loadout_state(
            &server,
            tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            &proposal_id,
        ),
        "refused overlay-drift apply must preserve proposal bytes and version"
    );
    assert_eq!(
        overlay_before,
        read_loadout_state(
            &server,
            tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
            "claude_plan",
        ),
        "refused overlay-drift apply must preserve overlay bytes and version"
    );
}

#[tokio::test]
async fn loadout_v3_apply_refuses_stale_overlay_cas_without_lost_update() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
                    "claude_plan",
                    r#"{"kind":"profile_card_loadout_overlay","profile":"claude_plan","source_proposal_ids":["preexisting-overlay"]}"#,
                )
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
        .expect("seed overlay before proposal mint");
    let proposal_id = mint_loadout_v3_fixture(&server, "stale-overlay-cas").await;
    approve_loadout_v3_fixture(&server, &proposal_id).await;
    install_loadout_trigger(
        &server,
        &format!(
            r#"
            CREATE TRIGGER loadout_stale_overlay_after_proposal_cas
            AFTER UPDATE ON hard_state
            WHEN OLD.namespace = '{proposal_ns}' AND OLD.key = '{proposal_id}'
            BEGIN
                UPDATE hard_state
                SET version = version + 1
                WHERE namespace = '{overlay_ns}' AND key = 'claude_plan';
            END;
            "#,
            proposal_ns = tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            overlay_ns = tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        ),
    );

    let proposal_before = read_loadout_state(
        &server,
        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        &proposal_id,
    );
    let overlay_before = read_loadout_state(
        &server,
        tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        "claude_plan",
    );
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("stale overlay CAS must refuse apply");
    assert!(
        err.contains("stale_overlay_version"),
        "expected stale_overlay_version refusal, got: {err}"
    );
    assert_eq!(
        proposal_before,
        read_loadout_state(
            &server,
            tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            &proposal_id,
        ),
        "stale overlay CAS must roll back proposal stamp bytes and version"
    );
    assert_eq!(
        overlay_before,
        read_loadout_state(
            &server,
            tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
            "claude_plan",
        ),
        "stale overlay CAS must roll back the competing overlay version bump"
    );
}

#[tokio::test]
async fn loadout_v3_forced_overlay_write_failure_rolls_back_both_rows() {
    let server = make_server();
    let proposal_id = mint_loadout_v3_fixture(&server, "overlay-write-failure").await;
    approve_loadout_v3_fixture(&server, &proposal_id).await;
    install_loadout_trigger(
        &server,
        &format!(
            r#"
            CREATE TRIGGER loadout_force_overlay_insert_failure
            BEFORE INSERT ON hard_state
            WHEN NEW.namespace = '{overlay_ns}' AND NEW.key = 'claude_plan'
            BEGIN
                SELECT RAISE(ABORT, 'forced_overlay_write_failure');
            END;
            "#,
            overlay_ns = tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        ),
    );

    let proposal_before = read_loadout_state(
        &server,
        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        &proposal_id,
    );
    let overlay_before = read_loadout_state(
        &server,
        tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        "claude_plan",
    );
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("forced overlay write failure must refuse apply");
    assert!(
        err.contains("forced_overlay_write_failure"),
        "expected forced overlay write failure, got: {err}"
    );
    assert_eq!(
        proposal_before,
        read_loadout_state(
            &server,
            tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            &proposal_id,
        ),
        "overlay write failure must roll back proposal stamp bytes and version"
    );
    assert_eq!(
        overlay_before,
        read_loadout_state(
            &server,
            tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
            "claude_plan",
        ),
        "overlay write failure must preserve absent overlay state"
    );
}

#[tokio::test]
async fn loadout_v3_forced_proposal_stamp_failure_rolls_back_both_rows() {
    let server = make_server();
    let proposal_id = mint_loadout_v3_fixture(&server, "proposal-stamp-failure").await;
    approve_loadout_v3_fixture(&server, &proposal_id).await;
    install_loadout_trigger(
        &server,
        &format!(
            r#"
            CREATE TRIGGER loadout_force_proposal_stamp_failure
            BEFORE UPDATE ON hard_state
            WHEN OLD.namespace = '{proposal_ns}' AND OLD.key = '{proposal_id}'
            BEGIN
                SELECT RAISE(ABORT, 'forced_proposal_stamp_failure');
            END;
            "#,
            proposal_ns = tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        ),
    );

    let proposal_before = read_loadout_state(
        &server,
        tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
        &proposal_id,
    );
    let overlay_before = read_loadout_state(
        &server,
        tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
        "claude_plan",
    );
    let mut apply = task_params("apply_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = server
        .tachi_task(Parameters(apply))
        .await
        .expect_err("forced proposal stamp failure must refuse apply");
    assert!(
        err.contains("forced_proposal_stamp_failure"),
        "expected forced proposal stamp failure, got: {err}"
    );
    assert_eq!(
        proposal_before,
        read_loadout_state(
            &server,
            tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS,
            &proposal_id,
        ),
        "proposal stamp failure must preserve proposal bytes and version"
    );
    assert_eq!(
        overlay_before,
        read_loadout_state(
            &server,
            tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
            "claude_plan",
        ),
        "proposal stamp failure must preserve overlay bytes and version"
    );
}
