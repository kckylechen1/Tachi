use super::*;

#[test]
fn route_policy_simulation_sinks_non_finite_scores() {
    let rows = vec![
        AgentPerformanceMatrixRow {
            scope: "leader".to_string(),
            profile: Some("claude_plan".to_string()),
            role: Some("planner".to_string()),
            agent: "claude".to_string(),
            task_type: "review_request".to_string(),
            samples: 10,
            success_rate: Some(0.95),
            verification_rate: 1.0,
            avg_quality_score: Some(f64::NAN),
            ..Default::default()
        },
        AgentPerformanceMatrixRow {
            scope: "leader".to_string(),
            profile: Some("codex_55_review".to_string()),
            role: Some("reviewer".to_string()),
            agent: "codex".to_string(),
            task_type: "review_request".to_string(),
            samples: 10,
            success_rate: Some(0.90),
            verification_rate: 1.0,
            avg_quality_score: Some(0.90),
            ..Default::default()
        },
    ];

    let summary = simulate_route_policy("quality_first", &rows, None);

    assert_eq!(summary.route_choices.len(), 1);
    assert_eq!(summary.route_choices[0].profile, "codex_55_review");
    assert!(summary.route_choices[0].score.is_finite());
}

#[test]
fn load_route_policy_rule_loadout_classifies_persisted_rules() {
    let db_path = crate::utils::test_fixture_path(format!(
        "dispatch-route-policy-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("test memory server");
    let min_samples = tachi_dispatch::MIN_ROUTE_POLICY_RULE_SAMPLES;

    for (id, task_type, prefer_profile, samples) in [
        (
            "route_policy:fix_request:opencode_builder",
            "fix_request",
            "opencode_builder",
            min_samples,
        ),
        (
            "route_policy:review_request:codex_55_review",
            "review_request",
            "codex_55_review",
            min_samples,
        ),
        (
            "route_policy:fix_request:glm_51_impl_sparse",
            "fix_request",
            "glm_impl",
            0,
        ),
        (
            "route_policy:fix_request:codex_53_fast_blocked",
            "fix_request",
            "codex_53_fast",
            min_samples,
        ),
    ] {
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
            "evidence": {
                "source": "test",
                "proposed": {
                    "samples": samples,
                },
            },
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
        blocked_profiles: vec!["codex_53_fast".to_string()],
    };

    let loadout = load_route_policy_rule_loadout(&server, &risk).expect("load route policy rules");

    assert_eq!(loadout.applied.len(), 1, "{loadout:#?}");
    assert_eq!(
        loadout.applied[0].proposal_id,
        "route_policy:fix_request:opencode_builder"
    );
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:review_request:codex_55_review"
        && rule.reason.contains("task_type_mismatch:review_request")));
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:fix_request:glm_51_impl_sparse"
        && rule.reason.contains("insufficient_samples:0<")));
    assert!(loadout.skipped.iter().any(|rule| rule.proposal_id
        == "route_policy:fix_request:codex_53_fast_blocked"
        && rule
            .reason
            .contains("blocked_by_risk_classifier:codex_53_fast")));
}

// ─── v3 proposal-safety discrimination tests ─────────────────────────────────
//
// These tests pin the route-policy side of the v3 content-addressed proposal
// safety contract. They are written but not run by this lane — the leader
// runs the full suite in the delivery worktree. Each names the production
// path it bites and the red->green property it discriminates.

/// Discrimination: a legacy (pre-v3) route_policy proposal — even one that
/// was approved under the old schema — must be refused at apply with
/// `legacy_unbound_proposal`, never silently inheriting the old approval.
///
/// Production path: `handle_route_policy_apply` -> schema_version guard.
/// Pre-fix red: the apply path only checked `status == "approved"`, so a
/// legacy approved row applied without any content-addressed binding.
/// Post-fix green: the schema_version guard refuses the legacy row loudly.
#[test]
fn legacy_approved_route_policy_proposal_cannot_be_applied() {
    let db_path = crate::utils::test_fixture_path(format!(
        "dispatch-legacy-route-apply-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("test memory server");

    // Seed a legacy approved proposal directly: no `schema_version`, no
    // `identity_payload`, no `content_digest` — exactly the shape pre-v3
    // persistence would have left behind.
    let proposal_id = "route_policy:cost_sensitive:fix_request:opencode_builder";
    let legacy = serde_json::json!({
        "proposal_id": proposal_id,
        "kind": "route_policy",
        "status": "approved",
        "review": {"status": "approved"},
        "policy": "cost_sensitive",
        "task_type": "fix_request",
        "policy_rule": {
            "when_task_type": "fix_request",
            "prefer_profile": "opencode_builder",
            "policy": "cost_sensitive",
            "fallback_to_current_profile": "claude_plan",
        },
        "evidence": {"source": "legacy"},
    });
    server
        .with_global_store(|store| {
            store
                .set_state(
                    DISPATCH_POLICY_PROPOSAL_NS,
                    proposal_id,
                    &legacy.to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed legacy approved proposal");

    // The route-rule namespace starts empty so we can prove no rule landed.
    let rules_before = server
        .with_global_store_read(|store| {
            store
                .list_state(ROUTE_POLICY_RULE_NS)
                .map_err(|e| e.to_string())
        })
        .expect("list rules before");
    assert!(rules_before.is_empty(), "no rule should exist yet");

    let err = crate::tune_ops::route_policy::handle_route_policy_apply(
        &server,
        proposal_id,
        true, // confirm
    )
    .expect_err("legacy approved proposal must refuse to apply");
    assert!(
        err.contains("legacy_unbound_proposal"),
        "expected legacy_unbound_proposal refusal, got: {err}"
    );

    // No rule row appeared, and the proposal is still in its prior approved
    // state — the refusal left both sides unchanged.
    let rules_after = server
        .with_global_store_read(|store| {
            store
                .list_state(ROUTE_POLICY_RULE_NS)
                .map_err(|e| e.to_string())
        })
        .expect("list rules after");
    assert!(
        rules_after.is_empty(),
        "no rule row may be created by a legacy refusal"
    );
    let (raw, _) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load")
        .expect("proposal row present");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(parsed["status"], serde_json::json!("approved"));
}

/// Discrimination: a route_policy apply must use a real SQLite transaction
/// so a failure between the proposal-status CAS and the route-rule write
/// rolls back BOTH rows. This test exercises the transaction primitive
/// directly: it seeds an approved v3 proposal, opens a transaction, succeeds
/// the proposal write, deliberately fails the rule write, and verifies the
/// proposal row's status is unchanged (rollback held).
///
/// Production path: `handle_route_policy_apply` -> `store.connection_mut()
/// .transaction()` wrapping both the proposal CAS and the rule write.
/// Pre-fix red: the two writes were independent `set_state` calls, so a late
/// failure could leave the proposal `applied` with no rule row.
/// Post-fix green: the transaction rolls back both writes on any branch that
/// returns Err before `tx.commit()`.
#[test]
fn route_policy_apply_transaction_rolls_back_on_mid_transaction_failure() {
    let db_path = crate::utils::test_fixture_path(format!(
        "dispatch-route-tx-rollback-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("test memory server");

    let proposal_id = "route_policy:v3:rollbackfixture";
    let identity_payload = serde_json::json!({
        "policy_version": "test-version",
        "target": "route_policy_rule",
        "apply_payload": {
            "when_task_type": "fix_request",
            "prefer_profile": "opencode_builder",
            "policy": "cost_sensitive",
            "fallback_to_current_profile": "claude_plan",
        },
        "evidence_review": {"source": "test"},
    });
    let proposal = serde_json::json!({
        "proposal_id": proposal_id,
        "legacy_proposal_id": "route_policy:cost_sensitive:fix_request:opencode_builder",
        "kind": "route_policy",
        "schema_version": 3,
        "policy_version": "test-version",
        "target": "route_policy_rule",
        "identity_payload": identity_payload.clone(),
        "content_digest": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        "status": "approved",
        "review": {"status": "approved"},
        "policy_rule": {
            "when_task_type": "fix_request",
            "prefer_profile": "opencode_builder",
            "policy": "cost_sensitive",
            "fallback_to_current_profile": "claude_plan",
        },
        "evidence": {"source": "test"},
    });
    server
        .with_global_store(|store| {
            store
                .set_state(
                    DISPATCH_POLICY_PROPOSAL_NS,
                    proposal_id,
                    &proposal.to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed approved v3 proposal");

    // Snapshot the version so the test can simulate a "stale version" mid-tx
    // failure inside the transaction. We deliberately use a wrong expected
    // version on the second write to force the rollback path.
    let (_raw, version) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load")
        .expect("row");

    // Simulate the transaction body that handle_route_policy_apply runs:
    // open tx -> first write (CAS) -> second write fails -> drop tx (rollback).
    // The real apply path's tx does: CAS proposal approved->applied, then
    // UPSERT rule row, then commit. We simulate a mid-tx failure by passing
    // a bogus expected_version to a CAS that runs AFTER a successful write
    // inside the same tx, and we assert that the earlier write is rolled
    // back when the tx returns Err without committing.
    let bogus_version = version + 1_000_000;
    let tx_result: Result<(), String> = server.with_global_store(|store| {
        let tx = store
            .connection_mut()
            .transaction()
            .map_err(|e| format!("open tx: {e}"))?;
        // First write: a fresh UPSERT to the rule namespace. This write
        // SUCCEEDS inside the tx body; the test's discriminating question is
        // whether it persists when a later step in the same tx errors.
        memcore::db::set_state(
            &tx,
            ROUTE_POLICY_RULE_NS,
            proposal_id,
            &proposal.to_string(),
        )
        .map_err(|e| format!("rule write: {e}"))?;
        // Second write: a CAS with a deliberately bogus expected_version. The
        // CAS affects 0 rows, we surface that as an Err, and the tx (still
        // uncommitted) drops at the end of the closure → SQLite rolls back
        // both the CAS row and the rule write above.
        let cas_ok = memcore::db::set_state_if_version(
            &tx,
            DISPATCH_POLICY_PROPOSAL_NS,
            proposal_id,
            &proposal.to_string(),
            bogus_version,
        )
        .map_err(|e| format!("proposal CAS: {e}"))?;
        if !cas_ok {
            return Err("simulated_mid_tx_cas_failure".to_string());
        }
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        Ok(())
    });
    let err = tx_result.expect_err("the simulated mid-tx CAS failure must surface");
    assert!(
        err.contains("simulated_mid_tx_cas_failure"),
        "expected our injected failure, got: {err}"
    );

    // Neither row changed. The rule write was rolled back; the proposal row
    // is still approved at its original version.
    let rules = server
        .with_global_store_read(|store| {
            store
                .list_state(ROUTE_POLICY_RULE_NS)
                .map_err(|e| e.to_string())
        })
        .expect("list rules");
    assert!(
        rules.is_empty(),
        "the rule write inside the failed transaction must have been rolled back"
    );
    let (raw, current_version) = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("load")
        .expect("row");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(
        parsed["status"],
        serde_json::json!("approved"),
        "proposal status must remain 'approved' after rollback"
    );
    assert_eq!(
        current_version, version,
        "proposal version must be unchanged after rollback"
    );
}

/// Discrimination: the hard_state version CAS that gates every review/apply
/// transition must refuse a stale snapshot. The full review() function reads
/// the version fresh, so its CAS cannot fail in a single-threaded test
/// without a concurrent writer; this test pins the primitive
/// (`set_state_if_version`) the review/apply paths compose, with a
/// deliberately stale `expected_version`.
///
/// Production path: `handle_route_policy_review` and
/// `handle_route_policy_apply` both use `set_state_if_version` for their
/// pending->reviewed and approved->applied transitions.
/// Pre-fix red: both paths used `set_state` (no CAS), so a concurrent
/// reviewer/applicator could overwrite each other silently.
/// Post-fix green: the CAS refuses the stale writer; the caller sees
/// `stale_state_version` and must reload+retry.
#[test]
fn route_policy_review_cas_refuses_on_stale_state_version() {
    let db_path = crate::utils::test_fixture_path(format!(
        "dispatch-stale-version-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("test memory server");

    let ns = DISPATCH_POLICY_PROPOSAL_NS;
    let key = "stale-version-fixture";
    let pending = r#"{"status":"pending","schema_version":2}"#;

    let v1 = server
        .with_global_store(|store| store.set_state(ns, key, pending).map_err(|e| e.to_string()))
        .expect("seed pending row");
    assert_eq!(v1, 1, "first write is version 1");

    // Simulate a concurrent writer bumping the version out from under us.
    server
        .with_global_store(|store| store.set_state(ns, key, pending).map_err(|e| e.to_string()))
        .expect("bump");

    // CAS with the stale snapshot's version (v1): must refuse, not overwrite.
    let cas_ok = server
        .with_global_store(|store| {
            store
                .set_state_if_version(ns, key, r#"{"status":"approved","schema_version":2}"#, v1)
                .map_err(|e| e.to_string())
        })
        .expect("CAS call should not error");
    assert!(
        !cas_ok,
        "CAS with a stale expected_version must refuse, not silently overwrite"
    );

    // The row stayed pending — the stale CAS did not mutate it.
    let (raw, current_version) = server
        .with_global_store_read(|store| store.get_state_kv(ns, key).map_err(|e| e.to_string()))
        .expect("load")
        .expect("row present");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(
        parsed["status"],
        serde_json::json!("pending"),
        "the refused CAS must leave the row's status untouched"
    );
    assert_eq!(
        current_version, 2,
        "the version reflects the concurrent bump, not the refused CAS"
    );
}
