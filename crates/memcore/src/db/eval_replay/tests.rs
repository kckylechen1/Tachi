//! Replay-vs-incremental discriminating tests (tachi#1675 PR3).
//!
//! Every seed here goes in through raw SQL rather than the append helpers, on
//! purpose: `insertion_seq` and `created_at` are exactly what these tests need
//! to pull APART (a correction that is older by the clock and newer by the
//! ledger), and the helpers deliberately stamp both for you.

use super::*;

use rusqlite::params;

use crate::db::eval_projection::{list_eval_observations, OCCURRED_AT_BASIS_LEGACY_CREATED_AT};
use crate::db::route_eval::{
    insert_eval_rubric_score, insert_route_decision_idempotent, insert_route_recommendation,
    NewEvalRubricScore, NewRouteDecision, NewRouteRecommendation,
};

const WINDOW_START: &str = "2026-07-01T00:00:00.000Z";
const LIMIT: usize = 200;

fn open_conn() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn
}

// ─── seeds ──────────────────────────────────────────────────────────────────

fn seed_outcome(conn: &Connection, outcome_id: &str, dispatch_id: &str, created_at: &str) {
    conn.execute(
        "INSERT INTO dispatch_outcomes \
         (outcome_id, dispatch_id, model, vendor, task_type, execution_outcome, \
          identity_attribution_basis, cost_tokens, cost_usd, idempotency_key, created_at, updated_at) \
         VALUES (?1, ?2, 'claude-sonnet', 'claude', 'fix_request', 'completed', \
                 'observed', 1000, 0.5, ?1, ?3, ?3)",
        params![outcome_id, dispatch_id, created_at],
    )
    .unwrap();
}

/// One appended judgment event with EXPLICIT `insertion_seq` and `created_at`
/// — the two things a replay must not confuse.
fn seed_adjudication(
    conn: &Connection,
    adjudication_id: &str,
    outcome_id: &str,
    verdict: &str,
    created_at: &str,
    insertion_seq: i64,
) {
    conn.execute(
        "INSERT INTO dispatch_adjudications \
         (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, created_at, insertion_seq) \
         VALUES (?1, ?2, ?1, ?3, 'leader', 'evidence://x', ?4, ?5)",
        params![adjudication_id, outcome_id, verdict, created_at, insertion_seq],
    )
    .unwrap();
}

fn seed_mirror_run(conn: &Connection, eval_run_id: &str, profile: &str, created_at: &str) {
    conn.execute(
        "INSERT INTO mirror_eval_runs \
         (eval_run_id, register_key, frozen_contract_ref, execution_origin, lifecycle_owner, \
          harness, native_child_id, requested_profile, requested_model, requested_agent, created_at) \
         VALUES (?1, ?1, 'kckylechen1/tachi#1675', 'host_native_subagent', 'host', \
                 'claude_code_task_tool', ?1, ?2, 'anthropic/claude-sonnet', 'claude', ?3)",
        params![eval_run_id, profile, created_at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mirror_eval_observations \
         (observation_id, eval_run_id, terminal_outcome, duration_ms, cost_tokens, cost_usd, \
          effective_model, created_at) \
         VALUES (?1, ?1, 'completed', 4242, 900, 0.2, 'anthropic/claude-sonnet', ?2)",
        params![eval_run_id, created_at],
    )
    .unwrap();
}

fn seed_mirror_adjudication(
    conn: &Connection,
    adjudication_id: &str,
    eval_run_id: &str,
    usefulness: &str,
    created_at: &str,
    insertion_seq: i64,
) {
    conn.execute(
        "INSERT INTO mirror_eval_adjudications \
         (adjudication_id, eval_run_id, event_key, actor, usefulness, evidence_usable, \
          evidence_ref, created_at, insertion_seq) \
         VALUES (?1, ?2, ?1, 'leader', ?3, 1, 'evidence://mirror', ?4, ?5)",
        params![
            adjudication_id,
            eval_run_id,
            usefulness,
            created_at,
            insertion_seq
        ],
    )
    .unwrap();
}

fn rubric(adjudication_id: &str, subject_kind: &str, safety: &str) -> NewEvalRubricScore {
    NewEvalRubricScore {
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

fn recommendation(id: &str, policy_source_revision: &str) -> NewRouteRecommendation {
    NewRouteRecommendation {
        recommendation_id: id.to_string(),
        task_type: Some("fix_request".to_string()),
        risk: "medium".to_string(),
        candidates: serde_json::json!([
            {"profile": "wizard_sonnet", "score": 9.0},
            {"profile": "codex_55_review", "score": 4.0},
        ]),
        recommended_profile: Some("wizard_sonnet".to_string()),
        policy_source_revision: Some(policy_source_revision.to_string()),
        rows_considered: 3,
        occurred_at: "2026-08-01T00:00:00.000Z".to_string(),
    }
}

fn decision(dispatch_id: &str, recommendation_id: Option<&str>, mode: &str) -> NewRouteDecision {
    NewRouteDecision {
        route_decision_id: format!("rd-{dispatch_id}"),
        dispatch_id: dispatch_id.to_string(),
        recommendation_id: recommendation_id.map(str::to_string),
        selected_profile: Some("wizard_sonnet".to_string()),
        selected_model: Some("anthropic/claude-sonnet".to_string()),
        assignment_mode: mode.to_string(),
        override_flag: false,
        contract_hash: None,
        env_id: Some("env-1".to_string()),
        host_profile: Some("dev".to_string()),
        work_claim_id: None,
        occurred_at: "2026-08-01T00:00:00.000Z".to_string(),
    }
}

// ─── helpers ────────────────────────────────────────────────────────────────

/// Every column of every row of `table`, verbatim, as the storage layer holds
/// them — the byte-level view an append-only claim has to survive.
fn table_rows_verbatim(conn: &Connection, table: &str) -> Vec<String> {
    let sql = format!("SELECT * FROM {table}");
    let mut statement = conn.prepare(&sql).unwrap();
    let column_count = statement.column_count();
    let mut rows = statement.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut cells = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value: rusqlite::types::Value = row.get(index).unwrap();
            cells.push(format!("{value:?}"));
        }
        out.push(cells.join("\u{1f}"));
    }
    out.sort();
    out
}

/// The equivalence assertion itself: a full replay and an incremental read
/// over the same window must be canonically identical.
fn assert_replay_matches_incremental(conn: &Connection, label: &str) -> EvalReplay {
    let incremental = list_eval_observations(conn, WINDOW_START, None, LIMIT).unwrap();
    let replay = replay_eval_observations(conn, WINDOW_START, None, LIMIT).unwrap();

    assert_eq!(
        replay.observations.len(),
        incremental.len(),
        "{label}: replay returned a different row count"
    );
    // Compare the canonical serializations, not just `PartialEq`: a mismatch
    // has to print WHAT diverged, and the serialization is the artifact the
    // server-side projection equivalence is graded on too.
    assert_eq!(
        canonical_eval_observations(&replay.observations).to_string(),
        canonical_eval_observations(&incremental).to_string(),
        "{label}: replay and incremental projections diverged"
    );
    assert_eq!(
        eval_observations_digest(&replay.observations),
        eval_observations_digest(&incremental),
        "{label}: canonical digests diverged"
    );
    assert_eq!(replay.ordering_basis, REPLAY_ORDERING_BASIS);
    replay
}

// ─── disc-8: replay ≡ incremental ───────────────────────────────────────────

/// The empty ledger is a real case, not a degenerate one: an evidence base
/// that has never been written must replay to exactly the same nothing the
/// incremental read returns.
#[test]
fn replay_equals_incremental_on_an_empty_ledger() {
    let conn = open_conn();
    let replay = assert_replay_matches_incremental(&conn, "empty ledger");
    assert!(replay.observations.is_empty());
    assert_eq!(replay.judgment_events_applied, 0);
    assert_eq!(replay.corrected_subjects, 0);
    assert!(replay.policy_revisions.is_empty());
    assert_eq!(replay.rows_without_policy_revision, 0);
}

/// A single un-judged row: no adjudication, no rubric, no route decision.
#[test]
fn replay_equals_incremental_on_a_single_bare_row() {
    let conn = open_conn();
    seed_outcome(&conn, "out-solo", "disp-solo", "2026-08-01T00:00:00.000Z");
    let replay = assert_replay_matches_incremental(&conn, "single bare row");
    assert_eq!(replay.observations.len(), 1);
    assert!(replay.observations[0].adjudication.is_none());
    assert!(replay.observations[0].route.is_none());
    assert_eq!(replay.rows_without_policy_revision, 1);
}

/// The whole shape at once: both spines, an advised decision with a recorded
/// candidate set, an `unadvised` one, a decision citing a recommendation row
/// that does not exist, a three-event overturn chain, and a mirror correction.
#[test]
fn replay_equals_incremental_across_mixed_spines_and_an_overturn_chain() {
    let conn = open_conn();

    // Advised dispatch row, fully judged.
    seed_outcome(
        &conn,
        "out-advised",
        "disp-advised",
        "2026-08-05T00:00:00.000Z",
    );
    insert_route_recommendation(&conn, &recommendation("rec-1", "policyrev-alpha")).unwrap();
    insert_route_decision_idempotent(&conn, &decision("disp-advised", Some("rec-1"), "advised"))
        .unwrap();
    seed_adjudication(
        &conn,
        "adj-advised",
        "out-advised",
        "APPROVED",
        "2026-08-05T01:00:00.000Z",
        1,
    );
    insert_eval_rubric_score(&conn, &rubric("adj-advised", "dispatch", "pass")).unwrap();

    // Unadvised acceptance: a decision row with no recommendation at all.
    seed_outcome(
        &conn,
        "out-unadvised",
        "disp-unadvised",
        "2026-08-04T00:00:00.000Z",
    );
    insert_route_decision_idempotent(&conn, &decision("disp-unadvised", None, "unadvised"))
        .unwrap();

    // Dangling reference: the decision cites a recommendation row that is not
    // there. Both paths must resolve it to "no candidate set", never a guess.
    seed_outcome(
        &conn,
        "out-dangling",
        "disp-dangling",
        "2026-08-03T00:00:00.000Z",
    );
    insert_route_decision_idempotent(
        &conn,
        &decision("disp-dangling", Some("rec-vanished"), "advised"),
    )
    .unwrap();

    // Three-event overturn chain on one subject; only the middle event
    // carries a rubric row, so the authoritative (third) judgment has none.
    seed_outcome(
        &conn,
        "out-corrected",
        "disp-corrected",
        "2026-08-02T00:00:00.000Z",
    );
    seed_adjudication(
        &conn,
        "adj-c1",
        "out-corrected",
        "APPROVED",
        "2026-08-02T01:00:00.000Z",
        1,
    );
    seed_adjudication(
        &conn,
        "adj-c2",
        "out-corrected",
        "CHANGES_REQUESTED",
        "2026-08-02T02:00:00.000Z",
        2,
    );
    insert_eval_rubric_score(&conn, &rubric("adj-c2", "dispatch", "concern")).unwrap();
    seed_adjudication(
        &conn,
        "adj-c3",
        "out-corrected",
        "REJECTED",
        "2026-08-02T03:00:00.000Z",
        3,
    );

    // Mirror spine: a run with a correction and a rubric on the correction.
    seed_mirror_run(&conn, "run-1", "wizard_sonnet", "2026-08-06T00:00:00.000Z");
    seed_mirror_adjudication(
        &conn,
        "madj-1",
        "run-1",
        "useful",
        "2026-08-06T01:00:00.000Z",
        1,
    );
    seed_mirror_adjudication(
        &conn,
        "madj-2",
        "run-1",
        "not_useful",
        "2026-08-06T02:00:00.000Z",
        2,
    );
    insert_eval_rubric_score(&conn, &rubric("madj-2", "mirror", "fail")).unwrap();

    let replay = assert_replay_matches_incremental(&conn, "mixed spines + overturn chain");

    assert_eq!(replay.observations.len(), 5);
    // Two subjects were corrected (one per spine); six judgment events across
    // the in-scope subjects.
    assert_eq!(replay.corrected_subjects, 2);
    assert_eq!(replay.judgment_events_applied, 6);

    let corrected = replay
        .observations
        .iter()
        .find(|row| row.subject_id == "out-corrected")
        .expect("corrected row present");
    let judgment = corrected.adjudication.as_ref().expect("judgment");
    assert_eq!(judgment.adjudication_id, "adj-c3");
    assert_eq!(judgment.event_count, 3);
    assert!(judgment.is_overturn());
    assert!(
        corrected.rubric.is_none(),
        "the superseded event's rubric row must never stand in for a correction that carries none"
    );

    let dangling = replay
        .observations
        .iter()
        .find(|row| row.subject_id == "out-dangling")
        .expect("dangling row present");
    let route = dangling.route.as_ref().expect("route facts");
    assert_eq!(route.recommendation_id.as_deref(), Some("rec-vanished"));
    assert!(
        route.candidate_profiles.is_empty(),
        "a missing recommendation row is an empty candidate set, never a reconstructed one"
    );
    assert!(route.policy_source_revision.is_none());
    assert!(!dangling.binds_decision_candidate_set());

    let mirror = replay
        .observations
        .iter()
        .find(|row| row.spine == EvalSpine::Mirror)
        .expect("mirror row present");
    assert!(
        mirror.route.is_none(),
        "the mirror spine has no route decision to replay"
    );
    assert_eq!(
        mirror.adjudication.as_ref().unwrap().adjudication_id,
        "madj-2"
    );
}

/// The window and the limit are part of the equivalence claim: a replay that
/// selected a different slice of the ledger would be "equivalent" only by
/// accident.
#[test]
fn replay_honors_the_same_window_and_limit_as_the_incremental_read() {
    let conn = open_conn();
    seed_outcome(&conn, "out-old", "disp-old", "2026-01-01T00:00:00.000Z");
    seed_outcome(&conn, "out-mid", "disp-mid", "2026-08-02T00:00:00.000Z");
    seed_outcome(&conn, "out-new", "disp-new", "2026-08-09T00:00:00.000Z");

    let incremental = list_eval_observations(&conn, WINDOW_START, None, LIMIT).unwrap();
    let replay = replay_eval_observations(&conn, WINDOW_START, None, LIMIT).unwrap();
    assert_eq!(incremental.len(), 2, "the pre-window row is excluded");
    assert_eq!(
        eval_observations_digest(&replay.observations),
        eval_observations_digest(&incremental)
    );

    // Capped read: same cap, same rows, same order.
    let incremental = list_eval_observations(&conn, WINDOW_START, None, 1).unwrap();
    let replay = replay_eval_observations(&conn, WINDOW_START, None, 1).unwrap();
    assert_eq!(incremental.len(), 1);
    assert_eq!(incremental[0].subject_id, "out-new");
    assert_eq!(
        eval_observations_digest(&replay.observations),
        eval_observations_digest(&incremental)
    );

    // Bounded upper end, on both paths.
    let incremental =
        list_eval_observations(&conn, WINDOW_START, Some("2026-08-05T00:00:00.000Z"), LIMIT)
            .unwrap();
    let replay =
        replay_eval_observations(&conn, WINDOW_START, Some("2026-08-05T00:00:00.000Z"), LIMIT)
            .unwrap();
    assert_eq!(incremental.len(), 1);
    assert_eq!(incremental[0].subject_id, "out-mid");
    assert_eq!(
        eval_observations_digest(&replay.observations),
        eval_observations_digest(&incremental)
    );
}

// ─── replay ordering is insertion_seq, never a timestamp ────────────────────

/// A correction that is NEWER by `insertion_seq` and OLDER by `created_at`.
/// The ledger order wins on both paths. A replay that sorted the judgment
/// stream by timestamp would resurrect the superseded verdict here — which is
/// the entire reason `insertion_seq` exists (#1035 FIX-3) and why design D4
/// says replay ordering is never a timestamp.
#[test]
fn replay_authority_follows_insertion_seq_not_created_at() {
    let conn = open_conn();
    seed_outcome(&conn, "out-skew", "disp-skew", "2026-08-01T00:00:00.000Z");
    seed_adjudication(
        &conn,
        "adj-superseded",
        "out-skew",
        "APPROVED",
        // LATER by the clock ...
        "2026-08-01T23:00:00.000Z",
        1,
    );
    seed_adjudication(
        &conn,
        "adj-correction",
        "out-skew",
        "REJECTED",
        // ... EARLIER by the clock, but appended after.
        "2026-08-01T02:00:00.000Z",
        2,
    );

    // The premise the test rests on: timestamp order and ledger order really
    // do disagree here.
    let (superseded_at, correction_at): (String, String) = conn
        .query_row(
            "SELECT (SELECT created_at FROM dispatch_adjudications WHERE adjudication_id = 'adj-superseded'), \
                    (SELECT created_at FROM dispatch_adjudications WHERE adjudication_id = 'adj-correction')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        correction_at < superseded_at,
        "the correction must be the older row by the clock for this test to discriminate"
    );

    let replay = assert_replay_matches_incremental(&conn, "clock-skewed correction");
    let judgment = replay.observations[0]
        .adjudication
        .as_ref()
        .expect("judgment");
    assert_eq!(
        judgment.adjudication_id, "adj-correction",
        "the highest insertion_seq is authoritative, whatever the timestamps say"
    );
    assert_eq!(judgment.verdict.as_deref(), Some("REJECTED"));
    assert_eq!(judgment.insertion_seq, 2);
    assert!(judgment.is_overturn());
}

/// The fold is a function of the event SET, not of read order: shuffling the
/// stream cannot change which event is authoritative or how many there were.
#[test]
fn the_judgment_fold_is_independent_of_stream_order() {
    let ordered = vec![
        JudgmentEvent {
            subject_id: "s".to_string(),
            adjudication_id: "a1".to_string(),
            actor: "leader".to_string(),
            verdict: Some("APPROVED".to_string()),
            created_at: "2026-08-01T00:00:00.000Z".to_string(),
            insertion_seq: 1,
        },
        JudgmentEvent {
            subject_id: "s".to_string(),
            adjudication_id: "a2".to_string(),
            actor: "leader".to_string(),
            verdict: Some("REJECTED".to_string()),
            created_at: "2026-07-01T00:00:00.000Z".to_string(),
            insertion_seq: 2,
        },
    ];
    let reversed = vec![
        JudgmentEvent {
            subject_id: "s".to_string(),
            adjudication_id: "a2".to_string(),
            actor: "leader".to_string(),
            verdict: Some("REJECTED".to_string()),
            created_at: "2026-07-01T00:00:00.000Z".to_string(),
            insertion_seq: 2,
        },
        JudgmentEvent {
            subject_id: "s".to_string(),
            adjudication_id: "a1".to_string(),
            actor: "leader".to_string(),
            verdict: Some("APPROVED".to_string()),
            created_at: "2026-08-01T00:00:00.000Z".to_string(),
            insertion_seq: 1,
        },
    ];

    let forward = fold_judgment_stream(ordered);
    let backward = fold_judgment_stream(reversed);
    assert_eq!(forward, backward);
    let facts = forward.get("s").expect("folded subject");
    assert_eq!(facts.adjudication_id, "a2");
    assert_eq!(facts.event_count, 2);
}

// ─── disc-7: corrections append, they never rewrite ─────────────────────────

/// Appending a correction changes what the replay RESOLVES TO while leaving
/// every pre-existing ledger row byte-identical — asserted over every column
/// of every row of both append-only tables, not over a summary of them.
#[test]
fn an_overturn_appends_and_rewrites_no_earlier_ledger_row() {
    let conn = open_conn();
    seed_outcome(
        &conn,
        "out-overturn",
        "disp-overturn",
        "2026-08-01T00:00:00.000Z",
    );
    seed_adjudication(
        &conn,
        "adj-first",
        "out-overturn",
        "APPROVED",
        "2026-08-01T01:00:00.000Z",
        1,
    );
    insert_eval_rubric_score(&conn, &rubric("adj-first", "dispatch", "pass")).unwrap();

    let before_adjudications = table_rows_verbatim(&conn, "dispatch_adjudications");
    let before_rubrics = table_rows_verbatim(&conn, "eval_rubric_scores");
    let before = replay_eval_observations(&conn, WINDOW_START, None, LIMIT).unwrap();
    let before_digest = before.digest();
    assert_eq!(
        before.observations[0]
            .rubric
            .as_ref()
            .expect("rubric")
            .safety,
        "pass"
    );

    // The correction: a NEW adjudication event carrying its OWN rubric row.
    seed_adjudication(
        &conn,
        "adj-overturn",
        "out-overturn",
        "REJECTED",
        "2026-08-01T02:00:00.000Z",
        2,
    );
    insert_eval_rubric_score(&conn, &rubric("adj-overturn", "dispatch", "fail")).unwrap();

    let after_adjudications = table_rows_verbatim(&conn, "dispatch_adjudications");
    let after_rubrics = table_rows_verbatim(&conn, "eval_rubric_scores");
    let after = assert_replay_matches_incremental(&conn, "after the overturn");

    // The replay result MOVED ...
    assert_ne!(
        before_digest,
        after.digest(),
        "appending a correction must change what the ledger replays to"
    );
    assert_eq!(
        after.observations[0]
            .adjudication
            .as_ref()
            .expect("judgment")
            .adjudication_id,
        "adj-overturn"
    );
    assert_eq!(
        after.observations[0]
            .rubric
            .as_ref()
            .expect("rubric")
            .safety,
        "fail"
    );

    // ... and every earlier row is still there, byte for byte.
    assert_eq!(after_adjudications.len(), before_adjudications.len() + 1);
    for row in &before_adjudications {
        assert!(
            after_adjudications.contains(row),
            "a pre-existing dispatch_adjudications row was rewritten: {row}"
        );
    }
    assert_eq!(after_rubrics.len(), before_rubrics.len() + 1);
    for row in &before_rubrics {
        assert!(
            after_rubrics.contains(row),
            "a pre-existing eval_rubric_scores row was rewritten: {row}"
        );
    }
}

// ─── disc-2: off-policy rows are queryable, on both paths ───────────────────

/// `user_forced` and `experiment` acceptances replay exactly as they were
/// recorded and stay fully queryable from the ledger. Whether they TRAIN
/// anything is the projection layer's rule, tested there; the ledger's job is
/// to keep them, and to keep them identically on both read paths.
#[test]
fn off_policy_rows_replay_identically_and_stay_queryable() {
    let conn = open_conn();
    insert_route_recommendation(&conn, &recommendation("rec-off", "policyrev-alpha")).unwrap();

    seed_outcome(
        &conn,
        "out-forced",
        "disp-forced",
        "2026-08-05T00:00:00.000Z",
    );
    insert_route_decision_idempotent(
        &conn,
        &decision("disp-forced", Some("rec-off"), "user_forced"),
    )
    .unwrap();
    seed_adjudication(
        &conn,
        "adj-forced",
        "out-forced",
        "APPROVED",
        "2026-08-05T01:00:00.000Z",
        1,
    );
    insert_eval_rubric_score(&conn, &rubric("adj-forced", "dispatch", "pass")).unwrap();

    seed_outcome(&conn, "out-exp", "disp-exp", "2026-08-04T00:00:00.000Z");
    insert_route_decision_idempotent(&conn, &decision("disp-exp", Some("rec-off"), "experiment"))
        .unwrap();

    let replay = assert_replay_matches_incremental(&conn, "off-policy acceptances");
    assert_eq!(replay.observations.len(), 2);

    let modes = replay
        .observations
        .iter()
        .filter_map(|row| {
            row.route
                .as_ref()
                .map(|route| route.assignment_mode.clone())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(modes.contains("user_forced"));
    assert!(modes.contains("experiment"));

    // Retained means retained: the rows are still readable straight out of
    // the source table, with their recorded mode intact.
    let stored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM route_decisions WHERE assignment_mode IN ('user_forced', 'experiment')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, 2);
}

// ─── design D4: the occurred_at basis is carried, not invented ──────────────

/// Legacy execution rows have no `occurred_at` column, so both paths map
/// `occurred_at := created_at` and SAY so. Neither path may quietly promote
/// the companion tables' real `occurred_at` (an acceptance time or a judgment
/// time) into the execution row's event-time slot.
#[test]
fn legacy_rows_carry_the_created_at_basis_on_both_paths() {
    let conn = open_conn();
    seed_outcome(
        &conn,
        "out-legacy",
        "disp-legacy",
        "2026-08-01T00:00:00.000Z",
    );
    insert_route_recommendation(&conn, &recommendation("rec-legacy", "policyrev-alpha")).unwrap();
    // The companion rows DO carry a real occurred_at, and it differs from the
    // execution row's created_at — the exact confusion this asserts against.
    insert_route_decision_idempotent(
        &conn,
        &decision("disp-legacy", Some("rec-legacy"), "advised"),
    )
    .unwrap();
    seed_mirror_run(
        &conn,
        "run-legacy",
        "wizard_sonnet",
        "2026-08-03T00:00:00.000Z",
    );

    let incremental = list_eval_observations(&conn, WINDOW_START, None, LIMIT).unwrap();
    let replay = assert_replay_matches_incremental(&conn, "legacy occurred_at basis");

    for rows in [&incremental, &replay.observations] {
        assert_eq!(rows.len(), 2);
        for row in rows.iter() {
            assert_eq!(row.occurred_at_basis, OCCURRED_AT_BASIS_LEGACY_CREATED_AT);
        }
        let dispatch_row = rows
            .iter()
            .find(|row| row.spine == EvalSpine::Dispatch)
            .unwrap();
        assert_eq!(dispatch_row.occurred_at, "2026-08-01T00:00:00.000Z");
        let mirror_row = rows
            .iter()
            .find(|row| row.spine == EvalSpine::Mirror)
            .unwrap();
        assert_eq!(mirror_row.occurred_at, "2026-08-03T00:00:00.000Z");
    }
}

// ─── recorded policy revisions ──────────────────────────────────────────────

/// The replay censuses the policy revision each row was RECORDED under, and
/// rows with none are counted rather than folded into some default. Two
/// recommendation rows written under different route-policy states stay
/// distinguishable forever, which is the whole point of stamping a
/// content-bearing hash instead of a counter (spec correction 5).
#[test]
fn replay_censuses_the_policy_revision_each_row_was_recorded_under() {
    let conn = open_conn();
    insert_route_recommendation(&conn, &recommendation("rec-alpha", "policyrev-alpha")).unwrap();
    insert_route_recommendation(&conn, &recommendation("rec-beta", "policyrev-beta")).unwrap();

    seed_outcome(&conn, "out-a1", "disp-a1", "2026-08-05T00:00:00.000Z");
    insert_route_decision_idempotent(&conn, &decision("disp-a1", Some("rec-alpha"), "advised"))
        .unwrap();
    seed_outcome(&conn, "out-a2", "disp-a2", "2026-08-04T00:00:00.000Z");
    insert_route_decision_idempotent(&conn, &decision("disp-a2", Some("rec-alpha"), "advised"))
        .unwrap();
    seed_outcome(&conn, "out-b1", "disp-b1", "2026-08-03T00:00:00.000Z");
    insert_route_decision_idempotent(&conn, &decision("disp-b1", Some("rec-beta"), "advised"))
        .unwrap();
    // No decision row at all: no recorded revision to census.
    seed_outcome(&conn, "out-none", "disp-none", "2026-08-02T00:00:00.000Z");

    let replay = assert_replay_matches_incremental(&conn, "policy revision census");
    assert_eq!(replay.policy_revisions.get("policyrev-alpha"), Some(&2));
    assert_eq!(replay.policy_revisions.get("policyrev-beta"), Some(&1));
    assert_eq!(replay.rows_without_policy_revision, 1);
}

/// Two replays of an unchanged ledger are byte-identical. Determinism is a
/// precondition of the equivalence claim, not a bonus.
#[test]
fn replaying_an_unchanged_ledger_twice_is_byte_identical() {
    let conn = open_conn();
    seed_outcome(&conn, "out-det", "disp-det", "2026-08-01T00:00:00.000Z");
    insert_route_recommendation(&conn, &recommendation("rec-det", "policyrev-alpha")).unwrap();
    insert_route_decision_idempotent(&conn, &decision("disp-det", Some("rec-det"), "advised"))
        .unwrap();
    seed_adjudication(
        &conn,
        "adj-det",
        "out-det",
        "APPROVED",
        "2026-08-01T01:00:00.000Z",
        1,
    );
    insert_eval_rubric_score(&conn, &rubric("adj-det", "dispatch", "pass")).unwrap();

    let first = replay_eval_observations(&conn, WINDOW_START, None, LIMIT).unwrap();
    let second = replay_eval_observations(&conn, WINDOW_START, None, LIMIT).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.digest(), second.digest());
    assert_eq!(
        first.canonical_json().to_string(),
        second.canonical_json().to_string()
    );
}
