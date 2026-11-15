use super::*;

/// tachi#1675 BUG-8 follow-up: this used to drive
/// `load_route_policy_rule_loadout` (a thin `list_state` + rebuild wrapper);
/// that wrapper was deleted once its only production caller
/// (`dispatch_profile::routing::recommendation`) inlined the equivalent
/// read-once-and-build sequence itself. The discriminating power is
/// unchanged — same seeded rows, same risk fixture, same assertions — only
/// the entry point moved to the ACTUAL production path: a direct
/// `list_state` read plus `tachi_dispatch::build_route_policy_rule_loadout`,
/// which is exactly what `recommendation.rs` calls today.
#[test]
fn build_route_policy_rule_loadout_classifies_persisted_rules() {
    let db_path = crate::utils::test_fixture_path(format!(
        "dispatch-route-policy-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("test memory server");
    let min_samples = tachi_dispatch::MIN_ROUTE_POLICY_RULE_SAMPLES;

    let ledger = tachi_dispatch::ROUTE_EVIDENCE_SOURCE_DECISION_FACT_LEDGER;
    for (id, task_type, prefer_profile, samples, evidence_source) in [
        (
            "route_policy:fix_request:opencode_terra_write",
            "fix_request",
            "opencode_terra_write",
            min_samples,
            ledger,
        ),
        (
            "route_policy:review_request:codex_astra_review",
            "review_request",
            "codex_astra_review",
            min_samples,
            ledger,
        ),
        (
            "route_policy:fix_request:glm_51_impl_sparse",
            "fix_request",
            "glm_impl",
            0,
            ledger,
        ),
        (
            "route_policy:fix_request:codex_sol_review_blocked",
            "fix_request",
            "codex_sol_review",
            min_samples,
            ledger,
        ),
        // A formerly approved rule cannot resurrect a retired profile.
        (
            "route_policy:fix_request:retired_opencode_builder",
            "fix_request",
            "opencode_builder",
            min_samples,
            ledger,
        ),
        // tachi#1675 PR4 BUG-1: identical to the applied rule above in every
        // structural respect — applied, approved, task-type matched, enough
        // samples, a known and unblocked profile — and refused anyway,
        // because its evidence was mined from the `/eval` memory base the
        // cutover retired. This is the exact shape
        // the retired proposal builder minted (`evidence.source =
        // "live_memory_eval"`).
        (
            "route_policy:fix_request:kimi_arch_eval_sourced",
            "fix_request",
            "kimi_arch",
            min_samples,
            "live_memory_eval",
        ),
        // ...and an undeclared row, the case an allowlist must also refuse.
        (
            "route_policy:fix_request:deepseek_explore_undeclared",
            "fix_request",
            "deepseek_explore",
            min_samples,
            "",
        ),
    ] {
        let mut evidence = serde_json::json!({
            "proposed": {
                "samples": samples,
            },
        });
        if !evidence_source.is_empty() {
            evidence["source"] = serde_json::json!(evidence_source);
        }
        let rule = serde_json::json!({
            "proposal_id": id,
            "kind": "route_policy",
            "status": "applied",
            "review": {
                "status": "approved",
            },
            "policy": "cost_sensitive",
            "task_type": task_type,
            "proposed_profile": prefer_profile,
            "score_delta": 12.5,
            "policy_rule": {
                "when_task_type": task_type,
                "prefer_profile": prefer_profile,
                "policy": "cost_sensitive",
                "fallback_to_current_profile": "claude_plan",
            },
            "evidence": evidence,
        });
        server
            .with_global_store(|store| {
                store
                    .set_state(ROUTE_POLICY_RULE_NS, id, &rule.to_string())
                    .map_err(|err| err.to_string())
            })
            .expect("seed route policy rule");
    }

    let risk = DispatchRisk {
        task_type: "fix_request".to_string(),
        risk: "critical".to_string(),
        reasons: vec!["test fixture".to_string()],
        required_profiles: Vec::new(),
        blocked_profiles: vec!["codex_sol_review".to_string()],
    };

    // The actual production sequence (recommendation.rs post-BUG-8): one
    // `list_state` read, mapped to `RoutePolicyRuleRecord`, fed straight into
    // `tachi_dispatch::build_route_policy_rule_loadout` — no intermediate
    // wrapper.
    let rows = server
        .with_global_store_read(|store| {
            store
                .list_state(ROUTE_POLICY_RULE_NS)
                .map_err(|e| e.to_string())
        })
        .expect("list route policy rules");
    let records = rows
        .into_iter()
        .map(|row| RoutePolicyRuleRecord {
            proposal_id: row.key,
            value_json: row.value_json,
        })
        .collect::<Vec<_>>();
    let loadout = tachi_dispatch::build_route_policy_rule_loadout(&records, &risk);

    assert_eq!(loadout.applied.len(), 1, "{loadout:#?}");
    assert_eq!(
        loadout.applied[0].proposal_id,
        "route_policy:fix_request:opencode_terra_write"
    );
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:review_request:codex_astra_review"
        && rule.reason.contains("task_type_mismatch:review_request")));
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:fix_request:glm_51_impl_sparse"
        && rule.reason.contains("insufficient_samples:0<")));
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:fix_request:codex_sol_review_blocked"
        && rule
            .reason
            .contains("blocked_by_risk_classifier:codex_sol_review")));

    assert!(
        loadout.skipped.iter().any(|rule| rule.proposal_id
            == "route_policy:fix_request:retired_opencode_builder"
            && rule.reason == "unknown_prefer_profile:opencode_builder"),
        "persisted approval must not redirect a retired model profile: {loadout:#?}"
    );

    // tachi#1675 PR4 BUG-1: the `/eval`-mined rule and the undeclared one are
    // refused, and refused for the RIGHT reason — a structurally perfect rule
    // cannot steer a route on evidence the cutover retired.
    assert!(
        loadout.skipped.iter().any(|rule| rule.proposal_id
            == "route_policy:fix_request:kimi_arch_eval_sourced"
            && rule.reason == "retired_evidence_source:live_memory_eval"),
        "an /eval-mined rule must be skipped as retired_evidence_source: {loadout:#?}"
    );
    assert!(
        loadout.skipped.iter().any(|rule| rule.proposal_id
            == "route_policy:fix_request:deepseek_explore_undeclared"
            && rule.reason == "retired_evidence_source:undeclared"),
        "a rule that declares no evidence source must be refused, not admitted: {loadout:#?}"
    );
    assert!(
        !loadout
            .applied
            .iter()
            .any(|rule| rule.prefer_profile == "kimi_arch"
                || rule.prefer_profile == "deepseek_explore"),
        "neither refused rule may reach the applied loadout: {loadout:#?}"
    );
}
