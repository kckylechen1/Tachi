use super::risk::classify_dispatch_risk;
use super::*;

use crate::agent_eval::projection::{
    resolve_route_evidence, rules as projection_rules, LedgerRouteEvidence,
};

/// The recommend surface's own rules version, bumped by tachi#1675 PR4 when its
/// evidence input flipped from `/eval` memory entries to the decision-fact
/// ledger (design D6 phase 2: the flip happens "with an explicit policy_version
/// bump — never silently").
///
/// `v1` is the pre-cutover `/eval`-memory scorer, which emitted no version at
/// all — which is exactly why the bump has to be legible in the payload rather
/// than inferred from behaviour.
pub(crate) const RECOMMEND_RULES_VERSION: &str = "dispatch_recommend/v2";

/// The content-bearing form of [`RECOMMEND_RULES_VERSION`]: the rules version
/// plus a prefix of the route-policy content digest this answer was produced
/// under.
///
/// Content-bearing, not a counter (spec correction 5 / PR3's reuse of codex
/// finding 7): route-policy rules live in a MUTABLE key-value table, so a
/// version that only counted rule revisions could not tell two different rule
/// states apart. The digest is `route_policy_source_revision`'s — the SAME hash
/// PR1 stamps onto `route_recommendations` and PR3 replays under, never a
/// second hash scheme.
pub(crate) fn recommend_policy_version(policy_source_revision: &str) -> String {
    let digest = policy_source_revision.trim();
    let short = digest.get(..12).unwrap_or(digest);
    format!("{RECOMMEND_RULES_VERSION}+{short}")
}

pub(crate) fn handle_dispatch_recommendation(
    server: &MemoryServer,
    task: &str,
    risk_override: Option<&str>,
    limit: usize,
    file_paths: &[String],
) -> Result<String, String> {
    let risk = classify_dispatch_risk(task, risk_override, file_paths);

    // tachi#1675 PR4 (design D6 phase 2): the evidence input is the
    // decision-fact ledger, NOT `/eval/YYYY-MM-DD` memory entries. Resolved
    // through the SAME pipeline `tachi_agent_eval(action='route_projection')`
    // runs — window, hard gates, per-row usability, abstain rules, policy
    // provenance — so the advisory surface and the independently inspectable
    // projection cannot answer differently about the same ledger.
    //
    // It also subsumes the tachi#1675 BUG-8 (TOCTOU) discipline: the resolver
    // reads the route-policy state in the SAME store checkout as the rows and
    // hands that snapshot back, so the scoring loadout AND the
    // `policy_source_revision` recorded on this call's `route_recommendations`
    // row come from one read. Nothing below re-reads `ROUTE_POLICY_RULE_NS`.
    let evidence = resolve_route_evidence(
        server,
        &risk,
        // The same bound every `tachi_agent_eval` read uses, applied to the
        // caller's limit — this path now reads the ledger (and, per usable
        // row, a `status.json`), so it takes the facade's cap rather than
        // trusting an arbitrary caller-supplied one.
        crate::agent_eval::capped_eval_limit(Some(limit)),
        projection_rules::DEFAULT_WINDOW_DAYS,
    )?;
    let route_policy_records = evidence
        .route_policy_rows
        .iter()
        .map(|row| RoutePolicyRuleRecord {
            proposal_id: row.key.clone(),
            value_json: row.value_json.clone(),
        })
        .collect::<Vec<_>>();
    let route_policy_rules =
        tachi_dispatch::build_route_policy_rule_loadout(&route_policy_records, &risk);

    let ledger_rows = evidence.usable_route_eval_rows();
    let mut candidates = tachi_dispatch::recommend_dispatch_profile_candidates(
        &risk,
        &ledger_rows,
        // The ledger carries no subagent-role rollup and no performance-matrix
        // aggregate — those were `/eval`-memory derivations. Empty slices
        // report that absence; synthesizing them from ledger rows would invent
        // human-override/retry/latency statistics the ledger never recorded.
        &[],
        &[],
        &route_policy_rules,
        tachi_dispatch::RouteEvidenceSource::DecisionFactLedger,
        |profile| profile_weak_against_for_server(server, profile),
    )?;
    // The ledger's own recommendation takes the top slot when it made one. It
    // can only ever name a candidate the hard gates already admitted
    // (`rules::project` ranks the gated set), so this reorders survivors and
    // never resurrects a profile the admission rules removed.
    let ledger_decision = promote_ledger_recommendation(&mut candidates, &evidence);

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
        evidence.outcome.rows_considered,
        tachi_dispatch::RouteEvidenceSource::DecisionFactLedger,
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
    // The revision the evidence resolver hashed from the SAME store checkout
    // the rows and the scoring loadout came from — no second read (BUG-8 fix).
    let policy_source_revision = evidence.live_policy_source_revision.clone();
    let new_recommendation = memcore::NewRouteRecommendation {
        recommendation_id: recommendation_id.clone(),
        task_type: Some(risk.task_type.clone()),
        risk: risk.risk.clone(),
        candidates: serde_json::to_value(&candidates).unwrap_or_else(|_| json!([])),
        recommended_profile: Some(best.profile.clone()),
        policy_source_revision: Some(policy_source_revision.clone()),
        rows_considered: evidence.outcome.rows_considered as u64,
        occurred_at,
    };
    server.with_global_store(|store| {
        memcore::insert_route_recommendation(store.connection(), &new_recommendation)
            .map_err(|e| e.to_string())
    })?;
    payload["recommendation_id"] = json!(recommendation_id);

    // tachi#1675 PR4: the flip's declaration block. Every field here is NEW —
    // no pre-cutover key changes name or meaning, so an existing consumer
    // (today: the feature briefing's `route_recommendation` block) keeps
    // reading exactly what it read before.
    payload["policy_version"] = json!(recommend_policy_version(&policy_source_revision));
    payload["policy_rules_version"] = json!(RECOMMEND_RULES_VERSION);
    payload["policy_source_revision"] = json!(policy_source_revision);
    payload["policy_provenance"] = evidence.policy_provenance_json();
    // The ledger's own recommend-or-abstain, verbatim from the projection.
    // `recommended_profile` above stays populated either way: on abstain it is
    // deterministic admission/role fit, and `evidence_backed` says so rather
    // than letting a caller mistake a fit for a judged result.
    payload["decision"] = evidence.outcome.decision.to_json();
    payload["evidence_backed"] = json!(ledger_decision.is_some());
    payload["ledger_evidence"] = json!({
        "window": evidence.window_json(),
        "rows_considered": evidence.outcome.rows_considered,
        "rows_limit": evidence.row_limit,
        "rows_truncated": evidence.rows_truncated,
        "usable_rows": evidence.outcome.usable_rows,
        "quality_only_rows": evidence.outcome.quality_only_rows,
        "n_min_usable_rows": projection_rules::N_MIN_USABLE_ROWS,
        "excluded_counts": evidence.outcome.excluded_counts,
        "hard_gates": {
            "eligible_profiles": evidence.gates.eligible,
            "excluded_profiles": evidence
                .gates
                .excluded
                .iter()
                .map(|(profile, reason)| json!({"profile": profile, "reason": reason}))
                .collect::<Vec<_>>(),
            "notes": evidence.gates.notes,
        },
        "eligible_candidates": evidence
            .outcome
            .candidates
            .iter()
            .map(|candidate| candidate.to_json())
            .collect::<Vec<_>>(),
        "note": "evidence source flipped from /eval memory entries to the decision-fact ledger \
                 (kckylechen1/tachi#1675 PR4); inspect the same rows via \
                 tachi_agent_eval(action='route_projection')",
    });

    serde_json::to_string(&payload).map_err(|e| format!("serialize recommendation: {e}"))
}

/// Move the ledger projection's recommended profile to the front of the
/// deterministically-scored candidate list, and return the reason it gave.
///
/// Returns `None` when the projection ABSTAINED (no usable evidence, thin
/// evidence, an unsettled overturn, or an uncertainty overlap) — in which case
/// the deterministic admission/role order stands untouched and the response
/// reports `evidence_backed: false`. An abstain must never be silently
/// upgraded into a ranking claim.
///
/// A recommended profile that is somehow absent from the scored list (it
/// cannot be today — both sides enumerate `DISPATCH_PROFILES`) is treated as an
/// abstain rather than inserted: the response must not name a profile the
/// scorer never produced a candidate row for.
fn promote_ledger_recommendation(
    candidates: &mut [tachi_dispatch::ProfileCandidate],
    evidence: &LedgerRouteEvidence,
) -> Option<String> {
    let projection_rules::ProjectionDecision::Recommend { profile, reasons } =
        &evidence.outcome.decision
    else {
        return None;
    };
    let position = candidates
        .iter()
        .position(|candidate| &candidate.profile == profile)?;
    let reason = reasons.first().cloned().unwrap_or_else(|| {
        format!("ledger projection recommends {profile} on usable in-window evidence")
    });
    candidates[..=position].rotate_right(1);
    if let Some(promoted) = candidates.first_mut() {
        promoted
            .reasons
            .push(format!("ledger_evidence_recommended:{reason}"));
    }
    Some(reason)
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

/// tachi#1675 PR4 (design D6 phase 2 / D7): the evidence-source flip itself.
///
/// These run against a temp `TACHI_HOME` because a dispatch-spine row is only
/// usable when its durable terminal outcome reconciles against the run's real
/// `status.json` — that cross-check is a filesystem read, and a fixture that
/// skipped it would be exercising a projection nobody ships.
#[cfg(test)]
mod evidence_flip_tests {
    use super::*;
    use crate::agent_eval::projection::rules as projection_rules;

    /// Every key a pre-cutover consumer could already read off this response.
    /// The flip may ADD fields; removing or renaming one of these would break
    /// the feature briefing (`copilot_ops::feature_briefing::dispatch`), the
    /// surviving production consumer.
    const PRE_CUTOVER_KEYS: &[&str] = &[
        "task",
        "task_type",
        "risk",
        "risk_reasons",
        "required_profiles",
        "blocked_profiles",
        "recommended_profile",
        "recommended_agent",
        "recommended_model",
        "identity_receipt",
        "recommended_transport",
        "transport_readiness",
        "role",
        "tool_profile",
        "evidence_required",
        "evidence_contract",
        "resolved_skills",
        "resolved_skill_loadout",
        "fallback_chain",
        "reason",
        "route_explanation",
        "evidence_note",
        "live_eval",
        "route_policy_rules",
        "mbit_card",
        "candidates",
        // tachi#1675 PR1 Seam A.
        "recommendation_id",
    ];

    /// Every per-candidate key that existed before the flip.
    const PRE_CUTOVER_CANDIDATE_KEYS: &[&str] = &[
        "profile",
        "agent",
        "role",
        "model",
        "score",
        "reasons",
        "live_samples",
        "useful_rate",
        "failure_count",
        "performance_samples",
        "human_override_rate",
        "avg_retry_count",
        "avg_latency_ms",
        "avg_cost_usd",
    ];

    const TASK: &str = "fix a bug in the parser";

    fn recommend(server: &MemoryServer) -> Value {
        let raw = handle_dispatch_recommendation(server, TASK, None, 200, &[])
            .expect("recommendation succeeds");
        serde_json::from_str(&raw).expect("recommendation payload is JSON")
    }

    /// A `status.json` receipt that agrees with the seeded durable outcome, at
    /// the real path `terminal_check_for` reads.
    fn write_completed_receipt(dispatch_id: &str) {
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::to_string(&json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_COMPLETED",
            }))
            .expect("serialize receipt"),
        )
        .expect("write receipt");
    }

    /// One row that satisfies EVERY usability clause of design D6: a terminal
    /// state that reconciles with its receipt, a structured rubric, a
    /// `structural_cross_vendor` independence basis, an `advised` acceptance,
    /// and a recorded decision-time candidate set containing this profile.
    fn seed_usable_row(
        server: &MemoryServer,
        key: &str,
        profile: &str,
        candidate_set: &[String],
        occurred_at: &str,
    ) {
        let outcome_id = format!("out-{key}");
        let dispatch_id = format!("disp-{key}");
        let recommendation_id = format!("rec-{key}");
        let adjudication_id = format!("adj-{key}");

        server
            .with_global_store(|store| {
                let conn = store.connection();
                conn.execute(
                    "INSERT INTO dispatch_outcomes \
                     (outcome_id, dispatch_id, model, vendor, task_type, execution_outcome, \
                      identity_attribution_basis, cost_tokens, cost_usd, idempotency_key, \
                      created_at, updated_at) \
                     VALUES (?1, ?2, 'anthropic/claude-sonnet', 'claude', 'fix_request', \
                             'completed', 'observed', 1000, 0.5, ?1, ?3, ?3)",
                    rusqlite::params![outcome_id, dispatch_id, occurred_at],
                )
                .map_err(|err| err.to_string())?;
                conn.execute(
                    "INSERT INTO dispatch_adjudications \
                     (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, \
                      created_at, insertion_seq) \
                     VALUES (?1, ?2, ?1, 'APPROVED', 'reviewer', 'evidence://pr4', ?3, 1)",
                    rusqlite::params![adjudication_id, outcome_id, occurred_at],
                )
                .map_err(|err| err.to_string())?;
                memcore::insert_route_recommendation(
                    conn,
                    &memcore::NewRouteRecommendation {
                        recommendation_id: recommendation_id.clone(),
                        task_type: Some("fix_request".to_string()),
                        risk: "medium".to_string(),
                        candidates: Value::Array(
                            candidate_set
                                .iter()
                                .map(|profile| json!({"profile": profile, "score": 1.0}))
                                .collect(),
                        ),
                        recommended_profile: candidate_set.first().cloned(),
                        policy_source_revision: Some("policyrev-fixture".to_string()),
                        rows_considered: candidate_set.len() as u64,
                        occurred_at: occurred_at.to_string(),
                    },
                )
                .map_err(|err| err.to_string())?;
                memcore::insert_route_decision_idempotent(
                    conn,
                    &memcore::NewRouteDecision {
                        route_decision_id: format!("rd-{key}"),
                        dispatch_id: dispatch_id.clone(),
                        recommendation_id: Some(recommendation_id.clone()),
                        selected_profile: Some(profile.to_string()),
                        selected_model: Some("anthropic/claude-sonnet".to_string()),
                        assignment_mode: "advised".to_string(),
                        override_flag: false,
                        contract_hash: None,
                        env_id: Some("env-pr4".to_string()),
                        host_profile: Some("dev".to_string()),
                        work_claim_id: None,
                        occurred_at: occurred_at.to_string(),
                    },
                )
                .map_err(|err| err.to_string())?;
                memcore::insert_eval_rubric_score(
                    conn,
                    &memcore::NewEvalRubricScore {
                        rubric_score_id: format!("rs-{key}"),
                        adjudication_id: adjudication_id.clone(),
                        subject_kind: "dispatch".to_string(),
                        rubric_hash: "rubric-v1".to_string(),
                        contract_correctness: "pass".to_string(),
                        evidence_quality: "pass".to_string(),
                        safety: "pass".to_string(),
                        scope_discipline: "pass".to_string(),
                        intervention_burden: "not_assessed".to_string(),
                        completion_integrity: "pass".to_string(),
                        adjudication_confidence: "high".to_string(),
                        adjudicator_actor: "reviewer".to_string(),
                        adjudicator_vendor: "codex".to_string(),
                        independence_basis: "structural_cross_vendor".to_string(),
                        occurred_at: occurred_at.to_string(),
                    },
                )
                .map(|_| ())
                .map_err(|err| err.to_string())
            })
            .expect("seed usable ledger row");

        write_completed_receipt(&dispatch_id);
    }

    fn string_array(value: &Value) -> Vec<String> {
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|item| item.as_str().expect("string").to_string())
            .collect()
    }

    /// Seed `count` usable rows for a candidate that is NOT the deterministic
    /// winner, and return it.
    fn seed_usable_rows_for_a_challenger(
        server: &MemoryServer,
        baseline_payload: &Value,
        key_prefix: &str,
        count: i64,
    ) -> String {
        let baseline = baseline_payload["recommended_profile"]
            .as_str()
            .expect("a deterministic admission fit is still named")
            .to_string();
        let eligible =
            string_array(&baseline_payload["ledger_evidence"]["hard_gates"]["eligible_profiles"]);
        let target = eligible
            .iter()
            .find(|profile| **profile != baseline)
            .expect("a second admissible candidate to move the answer to")
            .clone();
        let candidate_set = vec![target.clone(), baseline];

        let now = memcore::now_utc_iso();
        for index in 0..count {
            let occurred_at = projection_rules::shift_iso(&now, -3600 * (index + 1))
                .expect("shift the fixture timestamp into the window");
            seed_usable_row(
                server,
                &format!("{key_prefix}-{index}"),
                &target,
                &candidate_set,
                &occurred_at,
            );
        }
        target
    }

    /// THE discriminating test for the cutover: the same request, answered
    /// twice, differs ONLY because rows were written to the decision-fact
    /// ledger in between. If `recommend` were still scoring `/eval` memory
    /// entries, seeding the ledger could not move the answer at all.
    #[test]
    fn ledger_rows_decide_the_recommendation_after_the_flip() {
        let (server, _home) = crate::tests::make_server_with_temp_home();

        let before = recommend(&server);
        assert_eq!(
            before["decision"]["kind"],
            json!("abstain"),
            "an empty ledger has nothing to recommend from: {before:#}"
        );
        let target = seed_usable_rows_for_a_challenger(&server, &before, "pr4-flip", 3);

        let after = recommend(&server);
        assert_eq!(
            after["decision"]["kind"],
            json!("recommended"),
            "three usable rows clear N_min: {after:#}"
        );
        assert_eq!(after["decision"]["profile"], json!(target));
        assert_eq!(
            after["recommended_profile"],
            json!(target),
            "the ledger's answer IS the recommendation, not a footnote: {after:#}"
        );
        assert_ne!(
            after["recommended_profile"], before["recommended_profile"],
            "the ledger rows must have CHANGED the answer, or this proves nothing"
        );
        assert_eq!(after["evidence_backed"], json!(true));
        assert_eq!(after["evidence_source"], json!("decision_fact_ledger"));
        assert!(
            after["ledger_evidence"]["usable_rows"]
                .as_u64()
                .is_some_and(|rows| rows >= 3),
            "the usable rows are counted and reported: {after:#}"
        );
        assert_eq!(after["candidates"][0]["profile"], json!(target));
        assert!(
            after["candidates"][0]["reasons"]
                .as_array()
                .expect("reasons array")
                .iter()
                .any(|reason| reason
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("ledger_evidence_recommended:"))),
            "the promoted candidate states WHY the ledger promoted it: {after:#}"
        );
        // The note stops claiming an `/eval` provenance this answer no longer
        // has.
        let note = after["evidence_note"].as_str().unwrap_or_default();
        assert!(
            !note.contains("/eval"),
            "the note must not attribute this answer to /eval evidence: {note}"
        );
    }

    /// Design D7: with no usable evidence the answer is ABSTAIN, and
    /// `baseline_mbit_fit` appears nowhere in the response — while the field a
    /// consumer reads (`recommended_profile`) still carries the deterministic
    /// admission fit, explicitly marked as not evidence-backed.
    #[test]
    fn no_ledger_evidence_abstains_and_never_reports_a_baseline_mbit_fit() {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        let raw = handle_dispatch_recommendation(&server, TASK, None, 200, &[])
            .expect("recommendation succeeds on an empty ledger");
        let payload: Value = serde_json::from_str(&raw).expect("payload is JSON");

        assert_eq!(payload["decision"]["kind"], json!("abstain"));
        assert_eq!(payload["decision"]["profile"], Value::Null);
        assert_eq!(payload["evidence_backed"], json!(false));
        assert_eq!(payload["ledger_evidence"]["usable_rows"], json!(0));
        assert!(
            !raw.contains(tachi_dispatch::BASELINE_MBIT_FIT_REASON),
            "the ledger path must never report a baseline MBIT fit anywhere in \
             the response (#1202 / design D7): {raw}"
        );
        assert!(
            payload["recommended_profile"]
                .as_str()
                .is_some_and(|profile| !profile.is_empty()),
            "an abstain reports the deterministic admission fit rather than \
             blanking the field its consumers read: {payload:#}"
        );
    }

    /// Consumer compatibility across the flip: every pre-cutover key is still
    /// present, with the same name and shape, on BOTH the abstain and the
    /// recommended path. New keys are allowed; disappearing ones are not.
    #[test]
    fn the_response_schema_stays_compatible_across_the_flip() {
        let (server, _home) = crate::tests::make_server_with_temp_home();

        let abstained = recommend(&server);
        seed_usable_rows_for_a_challenger(&server, &abstained, "pr4-schema", 3);
        let recommended = recommend(&server);

        for payload in [&abstained, &recommended] {
            let object = payload.as_object().expect("payload is an object");
            for key in PRE_CUTOVER_KEYS {
                assert!(
                    object.contains_key(*key),
                    "pre-cutover key {key} disappeared: {payload:#}"
                );
            }
            let candidates = payload["candidates"].as_array().expect("candidates");
            assert!(!candidates.is_empty());
            for candidate in candidates {
                let candidate = candidate.as_object().expect("candidate is an object");
                for key in PRE_CUTOVER_CANDIDATE_KEYS {
                    assert!(
                        candidate.contains_key(*key),
                        "pre-cutover candidate key {key} disappeared: {candidate:#?}"
                    );
                }
            }
            // Shapes the briefing plumbing depends on, not just presence.
            assert!(payload["fallback_chain"].as_array().is_some());
            assert!(payload["live_eval"]["row_count"].as_u64().is_some());
            assert!(payload["evidence_note"].as_str().is_some());
            // ...and the flip's own declaration is present on both paths.
            assert_eq!(payload["evidence_source"], json!("decision_fact_ledger"));
            assert!(payload["policy_version"].as_str().is_some());
            assert!(payload["decision"]["kind"].as_str().is_some());
        }
        assert_eq!(abstained["decision"]["kind"], json!("abstain"));
        assert_eq!(recommended["decision"]["kind"], json!("recommended"));
    }

    /// The bump is observable AND content-bearing: the rules version is
    /// explicit (`v2`, never the silent pre-cutover shape), and the version
    /// string moves when the route-policy CONTENT moves — reusing
    /// `route_policy_source_revision`, the same digest PR1 stamps onto the
    /// `route_recommendations` row this very call writes.
    #[test]
    fn the_policy_version_bump_is_observable_and_content_bearing() {
        let (server, _home) = crate::tests::make_server_with_temp_home();

        let first = recommend(&server);
        assert_eq!(
            first["policy_rules_version"],
            json!(RECOMMEND_RULES_VERSION),
            "the flip declares its rules version: {first:#}"
        );
        let first_version = first["policy_version"]
            .as_str()
            .expect("policy_version")
            .to_string();
        assert!(
            first_version.starts_with(&format!("{RECOMMEND_RULES_VERSION}+")),
            "the version names the rules AND the policy content: {first_version}"
        );
        assert_ne!(
            first_version, "dispatch_recommend/v1",
            "the pre-cutover version must not be reported after the cutover"
        );
        let first_revision = first["policy_source_revision"]
            .as_str()
            .expect("policy_source_revision")
            .to_string();
        assert_eq!(
            first_version,
            recommend_policy_version(&first_revision),
            "the version is derived from the recorded revision, not from a \
             separate counter"
        );

        // Change the route-policy CONTENT. A counter-shaped version would not
        // have to move; a content-bearing one must.
        let rule = json!({
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

        let second = recommend(&server);
        let second_revision = second["policy_source_revision"]
            .as_str()
            .expect("policy_source_revision")
            .to_string();
        assert_ne!(
            second_revision, first_revision,
            "the route-policy digest must follow its content"
        );
        assert_ne!(
            second["policy_version"].as_str().unwrap_or_default(),
            first_version,
            "a content-bearing version moves with the content: {second:#}"
        );
        assert_eq!(
            second["policy_rules_version"],
            json!(RECOMMEND_RULES_VERSION),
            "only the content half moved; the rules version is stable"
        );

        // The digest inside the version is the SAME one stamped on this call's
        // ledger row — one hash scheme, not two.
        let recommendation_id = second["recommendation_id"]
            .as_str()
            .expect("recommendation_id")
            .to_string();
        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_recommendation(store.connection(), &recommendation_id)
                    .map_err(|err| err.to_string())
            })
            .expect("read the recommendation row")
            .expect("row present");
        assert_eq!(
            row.policy_source_revision.as_deref(),
            Some(second_revision.as_str())
        );
    }
}
