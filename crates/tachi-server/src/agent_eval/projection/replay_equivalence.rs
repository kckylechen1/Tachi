//! Replay-vs-incremental equivalence at the PROJECTION layer (tachi#1675 PR3,
//! design D6).
//!
//! `memcore::db::eval_replay`'s own tests prove the two READ paths reconstruct
//! the same rows. These prove the thing a consumer actually cares about: the
//! same recommendation comes out the other end — same candidate ranking, same
//! excluded counts and reasons, same recommend-or-abstain decision — whether
//! the evidence was read as current state or replayed from the append-only
//! log. And that a route-policy rule written TODAY cannot change what
//! yesterday's rows replay to.
//!
//! Why the terminal cross-check is injected here rather than taken from
//! `super::terminal_check_for`: that function reads `status.json` off the
//! filesystem, which no unit test owns. It is a pure function of the row's
//! `dispatch_id` and the filesystem, so it is identical across the two paths
//! exactly when the two paths produce the same `(spine, subject_id,
//! dispatch_id)` sequence — which
//! [`the_two_paths_carry_identical_row_identity`] asserts directly, including
//! against the REAL `terminal_check_for`. With that pinned, the projection
//! tests below inject a deterministic check so there is a non-trivial
//! recommendation to compare instead of a uniform "unverifiable" abstain.

use serde_json::{json, Value};

use memcore::{EvalObservation, EvalSpine};

use crate::server_state::MemoryServer;

use super::rules::{self, ProjectionOutcome, ProjectionRow, TerminalCheck};
use super::{eligible_candidate_set, terminal_check_for};

const WINDOW_START: &str = "2026-07-01T00:00:00.000Z";
/// Far enough past every seeded row that the overturn settling window is not
/// what any assertion below is measuring.
const NOW: &str = "2026-09-01T00:00:00.000Z";
const LIMIT: usize = 200;

fn test_server() -> MemoryServer {
    let db_path = crate::utils::test_fixture_path(format!(
        "route-projection-replay-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("test memory server")
}

// ─── seeding ────────────────────────────────────────────────────────────────

fn seed_outcome(server: &MemoryServer, outcome_id: &str, dispatch_id: &str, created_at: &str) {
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO dispatch_outcomes \
                     (outcome_id, dispatch_id, model, vendor, task_type, execution_outcome, \
                      identity_attribution_basis, cost_tokens, cost_usd, idempotency_key, \
                      created_at, updated_at) \
                     VALUES (?1, ?2, 'claude-sonnet', 'claude', 'fix_request', 'completed', \
                             'observed', 1000, 0.5, ?1, ?3, ?3)",
                    rusqlite::params![outcome_id, dispatch_id, created_at],
                )
                .map_err(|err| err.to_string())
        })
        .expect("seed dispatch outcome");
}

fn seed_adjudication(
    server: &MemoryServer,
    adjudication_id: &str,
    outcome_id: &str,
    verdict: &str,
    created_at: &str,
    insertion_seq: i64,
) {
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO dispatch_adjudications \
                     (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, \
                      created_at, insertion_seq) \
                     VALUES (?1, ?2, ?1, ?3, 'leader', 'evidence://x', ?4, ?5)",
                    rusqlite::params![
                        adjudication_id,
                        outcome_id,
                        verdict,
                        created_at,
                        insertion_seq
                    ],
                )
                .map_err(|err| err.to_string())
        })
        .expect("seed adjudication");
}

fn seed_mirror_run(server: &MemoryServer, eval_run_id: &str, profile: &str, created_at: &str) {
    server
        .with_global_store(|store| {
            let conn = store.connection();
            conn.execute(
                "INSERT INTO mirror_eval_runs \
                 (eval_run_id, register_key, frozen_contract_ref, execution_origin, \
                  lifecycle_owner, harness, native_child_id, requested_profile, requested_model, \
                  requested_agent, created_at) \
                 VALUES (?1, ?1, 'kckylechen1/tachi#1675', 'host_native_subagent', 'host', \
                         'claude_code_task_tool', ?1, ?2, 'anthropic/claude-sonnet', 'claude', ?3)",
                rusqlite::params![eval_run_id, profile, created_at],
            )
            .map_err(|err| err.to_string())?;
            conn.execute(
                "INSERT INTO mirror_eval_observations \
                 (observation_id, eval_run_id, terminal_outcome, duration_ms, cost_tokens, \
                  cost_usd, effective_model, created_at) \
                 VALUES (?1, ?1, 'completed', 4242, 900, 0.2, 'anthropic/claude-sonnet', ?2)",
                rusqlite::params![eval_run_id, created_at],
            )
            .map_err(|err| err.to_string())?;
            conn.execute(
                "INSERT INTO mirror_eval_adjudications \
                 (adjudication_id, eval_run_id, event_key, actor, usefulness, evidence_usable, \
                  evidence_ref, created_at, insertion_seq) \
                 VALUES (?1, ?1, ?1, 'leader', 'useful', 1, 'evidence://mirror', ?2, 1)",
                rusqlite::params![eval_run_id, created_at],
            )
            .map_err(|err| err.to_string())
        })
        .expect("seed mirror run");
}

fn rubric(adjudication_id: &str, subject_kind: &str, safety: &str) -> memcore::NewEvalRubricScore {
    memcore::NewEvalRubricScore {
        rubric_score_id: format!("rs-{adjudication_id}"),
        adjudication_id: adjudication_id.to_string(),
        subject_kind: subject_kind.to_string(),
        rubric_hash: "rubric-v1".to_string(),
        contract_correctness: "pass".to_string(),
        evidence_quality: "pass".to_string(),
        safety: safety.to_string(),
        scope_discipline: "pass".to_string(),
        intervention_burden: "not_assessed".to_string(),
        completion_integrity: "pass".to_string(),
        adjudication_confidence: "high".to_string(),
        adjudicator_actor: "leader".to_string(),
        adjudicator_vendor: "codex".to_string(),
        independence_basis: "structural_cross_vendor".to_string(),
        occurred_at: "2026-08-01T00:00:00.000Z".to_string(),
    }
}

/// One fully-judged, on-policy dispatch row: outcome + recommendation (with a
/// recorded candidate set and policy revision) + acceptance decision +
/// adjudication + rubric.
#[allow(clippy::too_many_arguments)]
fn seed_judged_row(
    server: &MemoryServer,
    key: &str,
    profile: &str,
    candidate_set: &[String],
    safety: &str,
    created_at: &str,
    assignment_mode: &str,
    policy_source_revision: &str,
) {
    let outcome_id = format!("out-{key}");
    let dispatch_id = format!("disp-{key}");
    let recommendation_id = format!("rec-{key}");
    let adjudication_id = format!("adj-{key}");

    seed_outcome(server, &outcome_id, &dispatch_id, created_at);
    server
        .with_global_store(|store| {
            let conn = store.connection();
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
                    policy_source_revision: Some(policy_source_revision.to_string()),
                    rows_considered: candidate_set.len() as u64,
                    occurred_at: created_at.to_string(),
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
                    assignment_mode: assignment_mode.to_string(),
                    override_flag: false,
                    contract_hash: None,
                    env_id: Some("env-1".to_string()),
                    host_profile: Some("dev".to_string()),
                    work_claim_id: None,
                    occurred_at: created_at.to_string(),
                },
            )
            .map_err(|err| err.to_string())?;
            Ok(())
        })
        .expect("seed route facts");
    seed_adjudication(
        server,
        &adjudication_id,
        &outcome_id,
        "APPROVED",
        created_at,
        1,
    );
    server
        .with_global_store(|store| {
            memcore::insert_eval_rubric_score(
                store.connection(),
                &rubric(&adjudication_id, "dispatch", safety),
            )
            .map(|_| ())
            .map_err(|err| err.to_string())
        })
        .expect("seed rubric");
}

// ─── reads ──────────────────────────────────────────────────────────────────

fn incremental_rows(server: &MemoryServer) -> Vec<EvalObservation> {
    server
        .with_global_store_read(|store| {
            memcore::list_eval_observations(store.connection(), WINDOW_START, None, LIMIT)
                .map_err(|err| err.to_string())
        })
        .expect("incremental read")
}

fn replayed(server: &MemoryServer) -> memcore::EvalReplay {
    server
        .with_global_store_read(|store| {
            memcore::replay_eval_observations(store.connection(), WINDOW_START, None, LIMIT)
                .map_err(|err| err.to_string())
        })
        .expect("replay read")
}

/// A deterministic stand-in for the `status.json` cross-check: every dispatch
/// row reconciles, every mirror row has nothing to reconcile against. See the
/// module doc for why this is injected rather than read off the filesystem.
fn projection_rows(observations: Vec<EvalObservation>) -> Vec<ProjectionRow> {
    observations
        .into_iter()
        .map(|observation| {
            let terminal = match observation.spine {
                EvalSpine::Dispatch => TerminalCheck::Consistent,
                EvalSpine::Mirror => TerminalCheck::NotApplicable,
            };
            ProjectionRow {
                observation,
                terminal,
            }
        })
        .collect()
}

fn project(observations: Vec<EvalObservation>, eligible: &[String]) -> ProjectionOutcome {
    let rows = projection_rows(observations);
    rules::project(
        &rows,
        eligible,
        WINDOW_START,
        None,
        NOW,
        Some("fix_request"),
    )
}

/// The whole projection response, as the one comparable artifact: candidate
/// ranking (in order), every excluded row and count, and the decision.
fn canonical_outcome(outcome: &ProjectionOutcome) -> String {
    json!({
        "candidates": outcome
            .candidates
            .iter()
            .map(|candidate| candidate.to_json())
            .collect::<Vec<_>>(),
        "excluded_counts": outcome.excluded_counts,
        "excluded_rows": outcome.explained_exclusions_json(),
        "decision": outcome.decision.to_json(),
        "rows_considered": outcome.rows_considered,
        "usable_rows": outcome.usable_rows,
        "quality_only_rows": outcome.quality_only_rows,
    })
    .to_string()
}

fn eligible_profiles() -> Vec<String> {
    let risk =
        tachi_dispatch::classify_dispatch_risk("fix a bug in the parser", "fix_request", None, &[]);
    let gates = eligible_candidate_set(&risk);
    assert!(
        gates.eligible.len() >= 2,
        "this fixture needs at least two admissible candidates, got {:?}",
        gates.eligible
    );
    gates.eligible
}

/// A ledger with two well-evidenced candidates (one carrying a safety fail),
/// an off-policy acceptance, an unjudged row, and a mirror row.
fn seed_full_fixture(server: &MemoryServer, eligible: &[String], policy_revision: &str) {
    let leader = eligible[0].clone();
    let runner_up = eligible[1].clone();
    let candidate_set = vec![leader.clone(), runner_up.clone()];

    for index in 0..4 {
        seed_judged_row(
            server,
            &format!("leader-{index}"),
            &leader,
            &candidate_set,
            "pass",
            &format!("2026-08-0{}T00:00:00.000Z", index + 1),
            "advised",
            policy_revision,
        );
        seed_judged_row(
            server,
            &format!("runner-{index}"),
            &runner_up,
            &candidate_set,
            // One failing safety judgment is categorical, and no amount of
            // cheap cost may buy it back — the lexicographic tier.
            if index == 0 { "fail" } else { "pass" },
            &format!("2026-08-0{}T01:00:00.000Z", index + 1),
            "advised",
            policy_revision,
        );
    }

    // Off-policy: retained, counted, never trained on.
    seed_judged_row(
        server,
        "forced",
        &leader,
        &candidate_set,
        "pass",
        "2026-08-06T00:00:00.000Z",
        "user_forced",
        policy_revision,
    );
    // Un-judged: a terminal row with no adjudication at all.
    seed_outcome(server, "out-bare", "disp-bare", "2026-08-07T00:00:00.000Z");
    // Mirror spine: quality evidence only, permanently off-policy for routing.
    seed_mirror_run(server, "run-1", &leader, "2026-08-08T00:00:00.000Z");
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// disc-8 at the projection layer: the ledger replayed and the ledger read as
/// current state produce the SAME recommendation, ranking, exclusions and
/// counts.
#[test]
fn replay_and_incremental_projections_are_identical() {
    let server = test_server();
    let eligible = eligible_profiles();
    seed_full_fixture(&server, &eligible, "policyrev-frozen");

    let incremental = incremental_rows(&server);
    let replay = replayed(&server);
    assert_eq!(
        memcore::eval_observations_digest(&replay.observations),
        memcore::eval_observations_digest(&incremental),
        "the two read paths disagree before the projection even runs"
    );

    let from_incremental = project(incremental, &eligible);
    let from_replay = project(replay.observations, &eligible);
    assert_eq!(
        canonical_outcome(&from_replay),
        canonical_outcome(&from_incremental),
        "replayed and incremental projections diverged"
    );

    // The fixture has to actually decide something, or "identical" would be
    // two identical abstains and would prove nothing.
    assert!(
        from_replay.usable_rows >= rules::N_MIN_USABLE_ROWS,
        "fixture produced too little usable evidence to discriminate: {} rows",
        from_replay.usable_rows
    );
    match &from_replay.decision {
        rules::ProjectionDecision::Recommend { profile, .. } => {
            assert_eq!(
                profile, &eligible[0],
                "the safety-failing candidate must not win"
            );
        }
        rules::ProjectionDecision::Abstain { reason, detail } => {
            panic!("expected a recommendation from this fixture, abstained: {reason} — {detail}")
        }
    }
}

/// The premise the injected terminal check rests on: both paths hand the
/// cross-check the SAME rows in the SAME order, so the REAL
/// `terminal_check_for` — filesystem and all — necessarily answers the same
/// for both.
#[test]
fn the_two_paths_carry_identical_row_identity() {
    let server = test_server();
    let eligible = eligible_profiles();
    seed_full_fixture(&server, &eligible, "policyrev-frozen");

    let incremental = incremental_rows(&server);
    let replay = replayed(&server);

    let identity = |rows: &[EvalObservation]| {
        rows.iter()
            .map(|row| {
                (
                    row.spine.as_str(),
                    row.subject_id.clone(),
                    row.dispatch_id.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(identity(&replay.observations), identity(&incremental));

    let checks = |rows: &[EvalObservation]| rows.iter().map(terminal_check_for).collect::<Vec<_>>();
    assert_eq!(
        checks(&replay.observations),
        checks(&incremental),
        "the real status.json cross-check answers differently across the two paths"
    );
}

/// Spec correction 5, closed: route-policy rules live in a MUTABLE key-value
/// table, so "the ledger is replayable" is only true if a replay interprets
/// each row under the policy revision recorded WITH it. Writing a new live
/// rule must therefore move the live revision and leave every replayed row —
/// and the projection over it — byte-identical.
#[test]
fn changing_live_route_policy_does_not_change_the_replay_of_old_rows() {
    let server = test_server();
    let eligible = eligible_profiles();
    seed_full_fixture(&server, &eligible, "policyrev-frozen");

    let live_revision = |server: &MemoryServer| {
        let rows = server
            .with_global_store_read(|store| {
                store
                    .list_state(crate::dispatch_profile::ROUTE_POLICY_RULE_NS)
                    .map_err(|err| err.to_string())
            })
            .expect("read live route policy rules");
        crate::tune_ops::route_policy::route_policy_source_revision(&rows)
    };

    let live_before = live_revision(&server);
    let replay_before = replayed(&server);
    let projection_before =
        canonical_outcome(&project(replay_before.observations.clone(), &eligible));

    // Change the live route policy underneath the recorded rows.
    server
        .with_global_store(|store| {
            store
                .set_state(
                    crate::dispatch_profile::ROUTE_POLICY_RULE_NS,
                    "route_policy:fix_request:written_after_the_fact",
                    &json!({
                        "proposal_id": "route_policy:fix_request:written_after_the_fact",
                        "kind": "route_policy",
                        "status": "applied",
                        "policy_rule": {
                            "when_task_type": "fix_request",
                            "prefer_profile": eligible[1],
                        },
                    })
                    .to_string(),
                )
                .map_err(|err| err.to_string())
        })
        .expect("write a new live route policy rule");

    let live_after = live_revision(&server);
    assert_ne!(
        live_before, live_after,
        "the live route-policy revision must actually have moved, or this test proves nothing"
    );

    let replay_after = replayed(&server);
    assert_eq!(
        replay_after.digest(),
        replay_before.digest(),
        "a rule written after these rows changed what they replay to"
    );
    assert_eq!(
        canonical_outcome(&project(replay_after.observations.clone(), &eligible)),
        projection_before,
        "a rule written after these rows changed the projection over them"
    );

    // And the recorded revision census still reports the FROZEN revision —
    // the live one never enters the replay's interpretation of old rows.
    assert_eq!(
        replay_after.policy_revisions.get("policyrev-frozen"),
        replay_before.policy_revisions.get("policyrev-frozen")
    );
    assert!(!replay_after.policy_revisions.contains_key(&live_after));
}

/// End-to-end through the real `tachi_agent_eval(route_projection)` handler:
/// the response states which policy revisions its EVIDENCE was recorded
/// under, states the LIVE revision separately, and changing the live rules
/// moves only the latter.
///
/// Seeded relative to `now` rather than at fixed dates on purpose — the
/// handler's window is `now - window_days`, and a fixture pinned to absolute
/// timestamps would quietly stop covering its own rows as the calendar moves.
#[tokio::test]
async fn the_live_surface_reports_recorded_revisions_without_applying_the_live_one() {
    let server = test_server();
    let eligible = eligible_profiles();
    let candidate_set = vec![eligible[0].clone(), eligible[1].clone()];
    for days_ago in 1..=3 {
        seed_judged_row(
            &server,
            &format!("recent-{days_ago}"),
            &eligible[0],
            &candidate_set,
            "pass",
            &days_ago_iso(days_ago),
            "advised",
            "policyrev-frozen",
        );
    }

    let before = route_projection_payload(&server).await;
    let provenance = &before["policy_provenance"];
    let live_before = provenance["live_policy_source_revision"]
        .as_str()
        .expect("the live revision is reported")
        .to_string();
    assert!(!live_before.is_empty());
    assert_eq!(
        provenance["evidence_policy_revisions"]["policyrev-frozen"],
        json!(3),
        "the response must state which recorded revision its evidence came from: {provenance}"
    );
    assert_eq!(provenance["rows_without_policy_revision"], json!(0));
    assert_eq!(
        provenance["evidence_spans_other_revisions"],
        json!(true),
        "evidence recorded under a different revision than the live one must say so"
    );

    server
        .with_global_store(|store| {
            store
                .set_state(
                    crate::dispatch_profile::ROUTE_POLICY_RULE_NS,
                    "route_policy:fix_request:written_after_the_fact",
                    &json!({
                        "proposal_id": "route_policy:fix_request:written_after_the_fact",
                        "kind": "route_policy",
                        "status": "applied",
                        "policy_rule": {
                            "when_task_type": "fix_request",
                            "prefer_profile": eligible[1],
                        },
                    })
                    .to_string(),
                )
                .map_err(|err| err.to_string())
        })
        .expect("write a new live route policy rule");

    let after = route_projection_payload(&server).await;
    let provenance = &after["policy_provenance"];
    assert_ne!(
        provenance["live_policy_source_revision"],
        json!(live_before),
        "the live revision must track the live rules"
    );
    assert_eq!(
        provenance["evidence_policy_revisions"],
        before["policy_provenance"]["evidence_policy_revisions"],
        "the recorded revisions of existing evidence must not move when live rules change"
    );
}

fn days_ago_iso(days: i64) -> String {
    rules::shift_iso(&memcore::now_utc_iso(), -days * 86_400).expect("shift a timestamp")
}

async fn route_projection_payload(server: &MemoryServer) -> Value {
    let raw = crate::agent_eval::handle_agent_eval(
        server,
        crate::tool_params::TachiAgentEvalParams {
            action: "route_projection".to_string(),
            fixture_path: None,
            limit: None,
            register: None,
            observe: None,
            adjudicate: None,
            get: None,
            projection: Some(crate::tool_params::RouteProjectionParams {
                task: Some("fix a bug in the parser".to_string()),
                task_type: Some("fix_request".to_string()),
                ..Default::default()
            }),
        },
    )
    .await
    .expect("route_projection succeeds");
    serde_json::from_str(&raw).expect("valid JSON")
}

/// disc-2: a `user_forced` acceptance stays fully queryable in the ledger and
/// trains nothing — identically on both read paths.
#[test]
fn user_forced_rows_are_queryable_but_train_nothing_on_both_paths() {
    let server = test_server();
    let eligible = eligible_profiles();
    seed_full_fixture(&server, &eligible, "policyrev-frozen");

    for observations in [incremental_rows(&server), replayed(&server).observations] {
        // Queryable: the row is right there in the read, with its recorded
        // mode intact — retention is not the question, training is.
        let forced = observations
            .iter()
            .find(|row| row.subject_id == "out-forced")
            .expect("the user_forced row is still returned by the read path");
        assert_eq!(
            forced.route.as_ref().expect("route facts").assignment_mode,
            "user_forced"
        );
        assert!(
            forced.rubric.is_some(),
            "the row is fully judged — it is excluded for its assignment mode, nothing else"
        );

        let outcome = project(observations, &eligible);
        assert_eq!(
            outcome
                .excluded_counts
                .get(rules::reason::ASSIGNMENT_MODE_USER_FORCED),
            Some(&1),
            "the user_forced row must be counted as excluded, never silently dropped"
        );
        assert!(
            outcome.explained_exclusions.iter().any(|row| {
                row.subject_id == "out-forced"
                    && row.reason == rules::reason::ASSIGNMENT_MODE_USER_FORCED
                    && !row.contributes_quality_evidence
            }),
            "the excluded row must be explained by subject id and reason"
        );
    }
}
