use super::risk::classify_dispatch_risk;
use super::*;

pub(crate) fn handle_dispatch_recommendation(
    server: &MemoryServer,
    task: &str,
    risk_override: Option<&str>,
    limit: usize,
    file_paths: &[String],
) -> Result<String, String> {
    let risk = classify_dispatch_risk(task, risk_override, file_paths);
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let subagent_scores = aggregate_subagent_scores(&rows);
    let performance_matrix = aggregate_performance_matrix(&rows);
    // tachi#1675 BUG-8 (TOCTOU): read the route-policy state ONCE. The
    // candidate-scoring loadout below and `policy_source_revision`'s hash
    // (further down) MUST be built from the exact SAME snapshot — a helper
    // that internally does its own `list_state(ROUTE_POLICY_RULE_NS)` read
    // (this PR's first cut called one, `load_route_policy_rule_loadout`,
    // since deleted — see `dispatch_profile::policy`'s module doc) and
    // reading again later opens a window where a concurrent route-policy
    // write lands between the two reads: the recorded hash would then
    // describe a policy state that never actually produced this
    // recommendation. `route_policy_rows` is threaded through to both
    // consumers below; nothing after this point re-reads
    // `ROUTE_POLICY_RULE_NS`.
    let route_policy_rows = server.with_global_store_read(|store| {
        store
            .list_state(ROUTE_POLICY_RULE_NS)
            .map_err(|e| format!("list active route policy rules: {e}"))
    })?;
    let route_policy_records = route_policy_rows
        .iter()
        .map(|row| RoutePolicyRuleRecord {
            proposal_id: row.key.clone(),
            value_json: row.value_json.clone(),
        })
        .collect::<Vec<_>>();
    let route_policy_rules =
        tachi_dispatch::build_route_policy_rule_loadout(&route_policy_records, &risk);

    let candidates = tachi_dispatch::recommend_dispatch_profile_candidates(
        &risk,
        &tachi_dispatch::route_eval_rows(&rows),
        &tachi_dispatch::route_subagent_scores(&subagent_scores),
        &tachi_dispatch::route_performance_rows(&performance_matrix),
        &route_policy_rules,
        |profile| profile_weak_against_for_server(server, profile),
    )?;

    let best = candidates
        .first()
        .ok_or_else(|| "no dispatch profiles configured".to_string())?;
    let best_profile = resolve_dispatch_profile(&best.profile)
        .ok_or_else(|| format!("internal missing profile {}", best.profile))?;
    let (recommended_transport, transport_readiness) =
        recommended_transport_for_profile(best_profile);

    let profile_json = profile_json_for_server(server, best_profile)?;
    let mut payload = tachi_dispatch::build_dispatch_recommendation_response(
        task,
        &risk,
        best_profile,
        &candidates,
        &route_policy_rules,
        rows.len(),
        tachi_dispatch::RecommendationProfilePayload {
            recommended_transport,
            transport_readiness,
            evidence_required: json!(profile_evidence_required_for_server(server, best_profile)?),
            evidence_contract: profile_evidence_contract_json_for_server(server, best_profile)?,
            resolved_skills: profile_required_skill_ids_for_server(server, best_profile)?,
            resolved_skill_loadout: profile_skill_loadout_json_for_server(server, best_profile)?,
            mbit_card: profile_json
                .get("mbit_card")
                .cloned()
                .unwrap_or(Value::Null),
        },
    )?;

    // tachi#1675 PR1 Seam A: this is the ONLY moment the candidate set exists
    // in memory — persist it as a `route_recommendations` fact before
    // returning. Never deduplicated (every consult, including a byte-identical
    // repeat, is its own fact — design D2). This turns a previously pure read
    // path into a write; it is also reachable from the briefing plumbing
    // (`copilot_ops::feature_briefing::dispatch::feature_dispatch_recommendation`),
    // which is covered by this same write since both call sites funnel
    // through this one function.
    let recommendation_id = uuid::Uuid::new_v4().to_string();
    let occurred_at = memcore::now_utc_iso();
    // Same `route_policy_rows` snapshot the loadout above was built from —
    // no second read (BUG-8 fix).
    let policy_source_revision =
        crate::tune_ops::route_policy::route_policy_source_revision(&route_policy_rows);
    let new_recommendation = memcore::NewRouteRecommendation {
        recommendation_id: recommendation_id.clone(),
        task_type: Some(risk.task_type.clone()),
        risk: risk.risk.clone(),
        candidates: serde_json::to_value(&candidates).unwrap_or_else(|_| json!([])),
        recommended_profile: Some(best.profile.clone()),
        policy_source_revision: Some(policy_source_revision),
        rows_considered: rows.len() as u64,
        occurred_at,
    };
    server.with_global_store(|store| {
        memcore::insert_route_recommendation(store.connection(), &new_recommendation)
            .map_err(|e| e.to_string())
    })?;
    payload["recommendation_id"] = json!(recommendation_id);

    serde_json::to_string(&payload).map_err(|e| format!("serialize recommendation: {e}"))
}

pub(in crate::dispatch_profile) fn recommended_transport_for_profile(
    profile: &DispatchProfileDef,
) -> (String, Value) {
    if !profile_uses_opencode_adapter(profile) {
        return (
            "native_cli".to_string(),
            json!({ "requested": "native_cli", "readiness": "not_applicable" }),
        );
    }

    let requested = std::env::var("TACHI_OPENCODE_TRANSPORT")
        .unwrap_or_else(|_| "cli".to_string())
        .to_ascii_lowercase();
    if matches!(requested.as_str(), "serve" | "opencode_serve" | "server") {
        let server_url = std::env::var("TACHI_OPENCODE_SERVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4321".to_string());
        let status = crate::dispatch_ops::probe_harness_server_status(Some(&server_url));
        let attach_ready = status
            .get("attach_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let transport = if attach_ready {
            "opencode_serve"
        } else {
            "opencode_cli"
        };
        return (
            transport.to_string(),
            json!({
                "requested": "opencode_serve",
                "server_url": server_url,
                "fallback": if attach_ready { Value::Null } else { json!("opencode_cli") },
                "harness_server_status": status,
            }),
        );
    }

    (
        "opencode_cli".to_string(),
        json!({ "requested": "opencode_cli", "readiness": "cli" }),
    )
}

#[cfg(test)]
mod route_recommendation_ledger_tests {
    use super::*;

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "dispatch-recommendation-seam-a-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db_path, None).expect("test memory server")
    }

    fn rubric_table_row_count(server: &MemoryServer, table: &str) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            })
            .unwrap()
    }

    /// tachi#1675 PR1 Seam A: a `recommend` call persists a
    /// `route_recommendations` row and echoes `recommendation_id` in the
    /// response payload — the id Seam B's `recommendation_ref` is meant to
    /// carry.
    #[test]
    fn recommendation_call_writes_a_route_recommendations_row() {
        let server = test_server();
        let before = rubric_table_row_count(&server, "route_recommendations");

        let raw = handle_dispatch_recommendation(&server, "fix a bug in the parser", None, 50, &[])
            .expect("recommendation succeeds");
        let payload: Value = serde_json::from_str(&raw).expect("valid JSON");
        let recommendation_id = payload
            .get("recommendation_id")
            .and_then(Value::as_str)
            .expect("recommendation_id present in payload")
            .to_string();
        assert!(!recommendation_id.is_empty());

        let after = rubric_table_row_count(&server, "route_recommendations");
        assert_eq!(after, before + 1, "exactly one row written per call");

        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_recommendation(store.connection(), &recommendation_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .expect("row present");
        assert_eq!(row.recommendation_id, recommendation_id);
        assert!(!row.risk.is_empty());
        assert!(row.candidates.is_array());
        assert!(
            !row.candidates.as_array().unwrap().is_empty(),
            "candidates JSON carries the full scored candidate array"
        );
    }

    /// Every consult is a new fact — two calls with identical arguments write
    /// TWO rows, never deduplicated.
    #[test]
    fn repeated_recommendation_calls_are_never_deduplicated() {
        let server = test_server();
        let before = rubric_table_row_count(&server, "route_recommendations");

        handle_dispatch_recommendation(&server, "review a PR", None, 50, &[]).unwrap();
        handle_dispatch_recommendation(&server, "review a PR", None, 50, &[]).unwrap();

        let after = rubric_table_row_count(&server, "route_recommendations");
        assert_eq!(after, before + 2, "each consult is an independent fact");
    }

    /// Negative test: Seam A's write path never touches session_claims or
    /// agent_identities.
    #[test]
    fn recommendation_write_never_touches_session_or_identity_tables() {
        let server = test_server();
        let before_claims = rubric_table_row_count(&server, "session_claims");
        let before_identities = rubric_table_row_count(&server, "agent_identities");

        handle_dispatch_recommendation(&server, "plan a feature", None, 50, &[]).unwrap();

        assert_eq!(
            before_claims,
            rubric_table_row_count(&server, "session_claims")
        );
        assert_eq!(
            before_identities,
            rubric_table_row_count(&server, "agent_identities")
        );
    }

    /// BUG-8 (TOCTOU): `policy_source_revision` must hash the SAME
    /// route-policy snapshot that scored the candidates, not a second,
    /// independently-timed read. A live race between the two reads can't be
    /// constructed deterministically in a unit test (there is no seam to
    /// inject a concurrent write mid-call), so this locks down the
    /// observable, structural consequence of the fix instead: the recorded
    /// `policy_source_revision` must equal `route_policy_source_revision`
    /// computed over a snapshot this test reads directly (proving the
    /// production code's hash is a pure function of rows, not a
    /// re-derived/independently-timed read) AND `route_policy_source_revision`
    /// itself only ever accepts `rows: &[StateRow]` — it has no store handle
    /// of its own to open a second, later read with.
    #[test]
    fn policy_source_revision_hashes_the_rows_that_scored_the_candidates() {
        let server = test_server();
        let rule = serde_json::json!({
            "proposal_id": "route_policy:fix_request:wizard_sonnet",
            "kind": "route_policy",
            "status": "applied",
            "review": {"status": "approved"},
            "policy": "cost_sensitive",
            "task_type": "fix_request",
            "proposed_profile": "wizard_sonnet",
            "score_delta": 12.5,
            "policy_rule": {
                "when_task_type": "fix_request",
                "prefer_profile": "wizard_sonnet",
                "policy": "cost_sensitive",
                "fallback_to_current_profile": "claude_plan",
            },
            "evidence": {"source": "test", "proposed": {"samples": tachi_dispatch::MIN_ROUTE_POLICY_RULE_SAMPLES}},
        });
        server
            .with_global_store(|store| {
                store
                    .set_state(
                        ROUTE_POLICY_RULE_NS,
                        "route_policy:fix_request:wizard_sonnet",
                        &rule.to_string(),
                    )
                    .map_err(|err| err.to_string())
            })
            .expect("seed route policy rule");

        let raw = handle_dispatch_recommendation(&server, "fix a bug", None, 50, &[])
            .expect("recommendation succeeds");
        let payload: Value = serde_json::from_str(&raw).unwrap();
        let recommendation_id = payload["recommendation_id"].as_str().unwrap().to_string();
        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_recommendation(store.connection(), &recommendation_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .expect("row present");

        // Independently read the (unchanged, single-threaded-test) state and
        // hash it the SAME way the production code does — this must equal
        // what got recorded, proving the recorded hash really is a function
        // of `route_policy_rows`, not something else.
        let expected_rows = server
            .with_global_store_read(|store| {
                store
                    .list_state(ROUTE_POLICY_RULE_NS)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        let expected_hash =
            crate::tune_ops::route_policy::route_policy_source_revision(&expected_rows);
        assert_eq!(
            row.policy_source_revision.as_deref(),
            Some(expected_hash.as_str())
        );
    }
}
