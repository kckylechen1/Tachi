use super::*;
use crate::MemoryServer;

/// Seed 10 `plan_request` eval rows for `claude_plan` (5 per agent so the
/// performance matrix holds 2 rows of 5 samples each), mint proposals, and
/// return the current id of the minted `evidence_contract` proposal for
/// `claude_plan` / `acceptance_criteria`.
///
/// #1690 C3 re-anchor: the helper used to mint a
/// `promote_observed_skill_to_signature` proposal; that proposal family is
/// retired end-to-end, so the surviving evidence-contract proposal is what
/// fixtures mint and walk.
async fn mint_evidence_contract_v3_fixture(server: &MemoryServer, fixture: &str) -> String {
    for idx in 0..10 {
        let agent = if idx < 5 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("evidence-contract-v3-{fixture}-{idx}")),
                task: "Plan a dispatch policy evolution slice".to_string(),
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
                notes: Some(format!("Seed {fixture} evidence-contract fixture.")),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some(format!("flow-evidence-contract-v3-{fixture}")),
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
            .expect("seed evidence-contract fixture");
    }

    let mut proposal_params = tune_params("route_proposals");
    proposal_params.limit = Some(50);
    let raw = run_tune(server, proposal_params)
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("evidence_contract")
                    && proposal["operation"] == json!("add_evidence_contract_required")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("minted evidence-contract proposal id")
        .to_string()
}

async fn approve_evidence_contract_v3_fixture(server: &MemoryServer, proposal_id: &str) {
    let mut review = tune_params("route_review");
    review.proposal_id = Some(proposal_id.to_string());
    review.review_status = Some("approved".to_string());
    run_tune(server, review)
        .await
        .expect("evidence-contract proposal approval should succeed");
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

/// Seed N+1 eval rows for `claude_plan` (split across two agents so the
/// performance matrix holds 2 rows of >= threshold samples each) with the
/// given task type.
async fn seed_profile_eval_rows(
    server: &MemoryServer,
    fixture: &str,
    task_type: &str,
    rows: usize,
) {
    for idx in 0..rows {
        let agent = if idx < rows / 2 { "claude" } else { "claude-alt" };
        server
            .tachi_complete(Parameters(TachiCompleteParams {
                task_id: Some(format!("{fixture}-{task_type}-{idx}")),
                task: format!("Dispatch policy {task_type} slice"),
                agent: agent.to_string(),
                outcome: "success".to_string(),
                task_type: Some(task_type.to_string()),
                profile: Some("claude_plan".to_string()),
                risk: Some("medium".to_string()),
                duration_ms: Some(20_000),
                skills_used: vec!["skill:planning-ux-review".to_string()],
                cost_tokens: Some(1200),
                cost_usd: Some(0.03),
                quality_score: Some(0.92),
                notes: Some(format!("Seed {fixture} eval fixture.")),
                trajectory: None,
                diff: None,
                worktree: None,
                subagents: Vec::new(),
                feedback_rules_applied: Vec::new(),
                dispatch_id: None,
                flow_id: Some(format!("flow-{fixture}-{task_type}")),
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
            .expect("seed profile eval row");
    }
}

/// #1690 C3 discriminator K1(a): `route_proposals` output contains NO
/// loadout/skill-evolution proposal kind on a seeded eval store — while the
/// surviving evidence-contract proposals (packet-carry enforcement) still
/// mint from the same matrix. RED pre-repair: the seeded store minted
/// `kind=loadout_evolution` with `operation=promote_observed_skill_to_signature`;
/// GREEN post-repair: that kind and operation appear nowhere.
#[tokio::test]
async fn route_proposals_mints_no_loadout_evolution_kind() {
    let server = make_server();
    seed_profile_eval_rows(&server, "k1a", "plan_request", 10).await;

    let mut proposal_params = tune_params("route_proposals");
    proposal_params.limit = Some(50);
    let raw = run_tune(&server, proposal_params)
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    assert!(proposals["proposal_kinds"]
        .as_array()
        .expect("proposal kinds")
        .contains(&json!("evidence_contract")));
    assert!(
        !proposals["proposal_kinds"]
            .as_array()
            .expect("proposal kinds")
            .contains(&json!("loadout_evolution")),
        "the retired loadout_evolution proposal kind must not be advertised: {proposals}"
    );
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                || proposal["operation"] == json!("promote_observed_skill_to_signature")
        }),
        "no proposal may carry the retired loadout/skill-evolution kind or operation: {proposal_items:?}"
    );
    assert!(
        proposal_items
            .iter()
            .any(|proposal| proposal["kind"] == json!("evidence_contract")),
        "the surviving evidence-contract family must still mint: {proposal_items:?}"
    );
    assert!(
        !raw.contains("promote_observed_skill_to_signature"),
        "the retired operation token must not appear anywhere in the response: {raw}"
    );
}

/// The full surviving lifecycle: mint evidence-contract proposals on a seeded
/// eval store, review, apply into the profile/card overlay, verify the overlay
/// projection reaches the agents registry — and pin that the retired
/// skill-promotion surface is absent end to end.
///
/// #1690 C3 re-anchor of `tachi_task_proposals_include_reviewable_loadout_evolution_candidates`:
/// the skill-promotion half (observed-skill mining -> `projected_signature_skills`)
/// is retired by the issue delete list; its assertions are re-anchored to
/// evidence-contract (the enforcement half that survives) and to the ABSENCE
/// of the retired half.
#[tokio::test]
async fn tachi_task_proposals_include_reviewable_evidence_contract_candidates() {
    let server = make_server();

    seed_profile_eval_rows(&server, "loadout-proposal", "plan_request", 10).await;

    let mut proposal_params = tune_params("route_proposals");
    proposal_params.limit = Some(50);
    let raw = run_tune(&server, proposal_params)
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    assert!(proposals["proposal_kinds"]
        .as_array()
        .expect("proposal kinds")
        .contains(&json!("evidence_contract")));
    let proposal_items = proposals["proposals"].as_array().expect("proposal list");
    assert!(
        !proposal_items.iter().any(|proposal| {
            proposal["kind"] == json!("loadout_evolution")
                && proposal["operation"] == json!("promote_observed_skill_to_signature")
        }),
        "the retired observed-skill promotion proposals must not be minted: {proposal_items:?}"
    );
    let proposal = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("evidence_contract")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_contract_required")
                    && proposal["evidence_id"] == json!("acceptance_criteria")
            })
        })
        .expect("evidence contract proposal");
    assert_eq!(proposal["status"], json!("pending"));
    assert_eq!(proposal["requires_human_approval"], json!(true));
    assert_eq!(
        proposal["operation"],
        json!("add_evidence_contract_required")
    );
    assert_eq!(proposal["evidence"]["profile_samples"], json!(10));
    assert_eq!(
        proposal["proposed_patch"]["add_evidence_required"][0],
        json!("acceptance_criteria")
    );
    assert_eq!(proposal["schema_version"], json!(3));
    assert_eq!(
        proposal["policy_version"],
        json!("2026-07-evidence-contract-v3")
    );
    assert_eq!(proposal["target"], json!("profile_card_overlay"));
    assert_eq!(
        proposal["identity_payload"]["kind"],
        json!("evidence_contract")
    );
    assert_eq!(
        proposal["identity_payload"]["apply_payload"]["profile"],
        proposal["profile"]
    );
    assert_eq!(
        proposal["identity_payload"]["apply_payload"]["evidence_id"],
        proposal["evidence_id"]
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
        "minted evidence proposal must expose its SHA-256 content digest: {proposal:?}"
    );
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    assert!(
        proposal_id.starts_with("evidence_contract:v3:"),
        "evidence proposal id must be content-addressed: {proposal_id}"
    );
    assert_eq!(
        proposal_items
            .iter()
            .filter(|proposal| {
                proposal["kind"] == json!("evidence_contract")
                    && proposal["profile"] == json!("claude_plan")
                    && proposal["operation"] == json!("add_evidence_contract_required")
                    && proposal["evidence_id"] == json!("acceptance_criteria")
            })
            .count(),
        1,
        "duplicate agent/model matrix rows should not emit duplicate evidence contract proposals: {proposal_items:?}"
    );

    let mut review = tune_params("route_review");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("Human approved evidence contract candidate.".to_string());
    let reviewed_raw = run_tune(&server, review)
        .await
        .expect("review should succeed");
    let reviewed: serde_json::Value = serde_json::from_str(&reviewed_raw).expect("review JSON");
    assert_eq!(reviewed["proposal"]["status"], json!("approved"));
    assert_eq!(reviewed["proposal"]["kind"], json!("evidence_contract"));

    let mut apply = tune_params("route_apply");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let applied_raw = run_tune(&server, apply)
        .await
        .expect("approved evidence contract should project");
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
    assert_eq!(
        applied["proposal"]["projection"]["added_evidence_required"][0],
        json!("acceptance_criteria")
    );

    let overlay_before_second_apply = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read overlay before second apply");
    let mut second_apply = tune_params("route_apply");
    second_apply.proposal_id = Some(proposal_id.clone());
    second_apply.confirm = true;
    let err = run_tune(&server, second_apply)
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

    // #1431's source-revision binding is pinned by the dedicated
    // `*_refuses_live_overlay_drift_without_mutation` tests below (review and
    // apply). The pre-contraction sibling-staleness flow (apply one proposal,
    // then review ANOTHER from the same mint) is no longer constructible: the
    // sibling used to be the skill-promotion proposal, which the #1690 C3
    // contraction retired — with one proposal family per profile per mint and
    // the sample gate admitting a single task type per profile, two
    // same-profile proposals can no longer share one mint.

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
    // #1690 C3: the promoted-skill overlay write is retired — the seeded
    // `skill:planning-ux-review` never appears in signature skills, and the
    // loadout overlay half stays inert.
    assert!(
        !agent_claude_profile["skill_loadout"]["signature_skills"]
            .as_array()
            .expect("agent signature skills")
            .contains(&json!("skill:planning-ux-review")),
        "retired observed-skill promotion must not project into the registry: {agent_claude_profile}"
    );
    assert_eq!(
        agent_claude_profile["skill_loadout"]["projected_signature_skills"],
        json!([]),
        "the loadout overlay projection is retired; the history key stays empty: {agent_claude_profile}"
    );
    // The surviving enforcement half DOES project: the evidence contract's
    // `projected_required` carries the applied acceptance_criteria.
    assert!(
        agent_claude_profile["evidence_contract"]["projected_required"]
            .as_array()
            .expect("agent projected evidence")
            .contains(&json!("acceptance_criteria")),
        "applied evidence contract must project into the registry: {agent_claude_profile}"
    );
}

/// Discrimination: the shipped `review_proposal` / `apply_proposals` facade
/// must bind the exact evidence-contract payload a human approved. On the
/// pre-#1431 implementation, changing only the top-level evidence id after
/// approval was accepted and projected into the durable overlay.
///
/// #1690 C3 re-anchor: the tamper target moves from `skill_id` (retired
/// surface) to `evidence_id` — the surviving bindable apply field.
#[tokio::test]
async fn evidence_contract_apply_refuses_tampered_payload_without_overlay_mutation() {
    let server = make_server();
    seed_profile_eval_rows(&server, "evidence-tamper", "plan_request", 10).await;

    let mut proposal_params = tune_params("route_proposals");
    proposal_params.limit = Some(50);
    let raw = run_tune(&server, proposal_params)
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_id = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("evidence_contract")
                    && proposal["operation"] == json!("add_evidence_contract_required")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("minted evidence proposal id")
        .to_string();

    let mut review = tune_params("route_review");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    run_tune(&server, review)
        .await
        .expect("approve evidence proposal");

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("evidence proposal row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["evidence_id"] = json!("attacker-controlled-evidence");
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

    let mut apply = tune_params("route_apply");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = run_tune(&server, apply)
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

    let mut list = tune_params("route_proposals");
    list.limit = Some(50);
    let listed_raw = run_tune(&server, list)
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
    let mut review = tune_params("route_review");
    review.proposal_id = Some(pending_id.to_string());
    review.review_status = Some("approved".to_string());
    let review_err = run_tune(&server, review)
        .await
        .expect_err("legacy loadout row must not be reviewable");
    assert!(
        review_err.contains("unsupported_proposal_kind"),
        "expected loud retirement refusal, got: {review_err}"
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
    let mut apply = tune_params("route_apply");
    apply.proposal_id = Some(approved_id.to_string());
    apply.confirm = true;
    let apply_err = run_tune(&server, apply)
        .await
        .expect_err("legacy loadout row must not be applicable");
    assert!(
        apply_err.contains("does not support proposal kind"),
        "expected loud retirement refusal, got: {apply_err}"
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
async fn evidence_contract_review_refuses_evidence_drift_without_partial_mutation() {
    let server = make_server();
    seed_profile_eval_rows(&server, "evidence-drift", "plan_request", 10).await;

    let mut proposal_params = tune_params("route_proposals");
    proposal_params.limit = Some(50);
    let raw = run_tune(&server, proposal_params)
        .await
        .expect("proposals should succeed");
    let proposals: serde_json::Value = serde_json::from_str(&raw).expect("proposals JSON");
    let proposal_id = proposals["proposals"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|proposal| {
                proposal["kind"] == json!("evidence_contract")
                    && proposal["operation"] == json!("add_evidence_contract_required")
            })
        })
        .and_then(|proposal| proposal["proposal_id"].as_str())
        .expect("minted evidence proposal id")
        .to_string();

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("evidence proposal row");
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

    let mut review = tune_params("route_review");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = run_tune(&server, review)
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
async fn evidence_contract_v3_review_refuses_baseline_display_tamper_without_mutation() {
    let server = make_server();
    let proposal_id = mint_evidence_contract_v3_fixture(&server, "baseline-display-tamper").await;

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv(tachi_dispatch::DISPATCH_POLICY_PROPOSAL_NS, &proposal_id)
                .map_err(|err| err.to_string())?
                .expect("evidence proposal row");
            let mut value: serde_json::Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["current_evidence_contract"]["required"] =
                json!(["attacker-controlled-baseline"]);
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
    let mut review = tune_params("route_review");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = run_tune(&server, review)
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
async fn evidence_contract_v3_review_refuses_live_overlay_drift_without_mutation() {
    let server = make_server();
    let proposal_id = mint_evidence_contract_v3_fixture(&server, "overlay-drift-review").await;
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
    let mut review = tune_params("route_review");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = run_tune(&server, review)
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
async fn evidence_contract_v3_apply_refuses_live_overlay_drift_without_mutation() {
    let server = make_server();
    let proposal_id = mint_evidence_contract_v3_fixture(&server, "overlay-drift-apply").await;
    approve_evidence_contract_v3_fixture(&server, &proposal_id).await;
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
    let mut apply = tune_params("route_apply");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = run_tune(&server, apply)
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
