use super::*;

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
