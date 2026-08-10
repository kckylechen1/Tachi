//! Discriminating tests for the projection rules (tachi#1675 PR2 covers
//! discriminations 1, 4, 5, 6 of the frozen design).
//!
//! Every test here is a pure function call: rows in, decision out, no store,
//! no clock, no filesystem. `NOW` is a fixed instant, so "deterministic" is
//! an assertion this file can actually make.

use super::*;

use memcore::{
    EvalAdjudicationFacts, EvalObservation, EvalRouteFacts, EvalRubricScoreRow, EvalSpine,
    ProfileAttributionBasis, OCCURRED_AT_BASIS_LEGACY_CREATED_AT,
};

const NOW: &str = "2026-08-10T00:00:00.000Z";
/// 30 days before `NOW`.
const SINCE: &str = "2026-07-11T00:00:00.000Z";
const IN_WINDOW: &str = "2026-08-01T00:00:00.000Z";
const BEFORE_WINDOW: &str = "2026-05-01T00:00:00.000Z";
const TASK_TYPE: &str = "fix_request";

/// Declarative fixture for one ledger row. Defaults describe the "clean,
/// usable, on-policy" row; each test perturbs exactly the field under test.
#[derive(Debug, Clone)]
struct Row {
    profile: &'static str,
    occurred_at: String,
    spine: EvalSpine,
    safety: &'static str,
    contract_correctness: &'static str,
    completion_integrity: &'static str,
    evidence_quality: &'static str,
    scope_discipline: &'static str,
    intervention_burden: &'static str,
    independence_basis: &'static str,
    assignment_mode: &'static str,
    candidate_profiles: Vec<&'static str>,
    with_adjudication: bool,
    with_rubric: bool,
    adjudication_events: usize,
    adjudicated_at: String,
    terminal_outcome: Option<&'static str>,
    terminal: TerminalCheck,
    task_type: Option<&'static str>,
    cost_usd: Option<f64>,
    cost_tokens: Option<u64>,
    duration_ms: Option<u64>,
    subject_id: String,
}

impl Default for Row {
    fn default() -> Self {
        Self {
            profile: "profile_a",
            occurred_at: IN_WINDOW.to_string(),
            spine: EvalSpine::Dispatch,
            safety: "pass",
            contract_correctness: "pass",
            completion_integrity: "pass",
            evidence_quality: "pass",
            scope_discipline: "pass",
            intervention_burden: "pass",
            independence_basis: "structural_cross_vendor",
            assignment_mode: "advised",
            candidate_profiles: vec!["profile_a", "profile_b"],
            with_adjudication: true,
            with_rubric: true,
            adjudication_events: 1,
            adjudicated_at: IN_WINDOW.to_string(),
            terminal_outcome: Some("completed"),
            terminal: TerminalCheck::Consistent,
            task_type: Some(TASK_TYPE),
            cost_usd: Some(1.0),
            cost_tokens: Some(1_000),
            duration_ms: None,
            subject_id: String::new(),
        }
    }
}

impl Row {
    fn build(self, index: usize) -> ProjectionRow {
        let subject_id = if self.subject_id.is_empty() {
            format!("{}-{index}", self.profile)
        } else {
            self.subject_id.clone()
        };
        let adjudication_id = format!("adj-{subject_id}");
        let rubric = self.with_rubric.then(|| EvalRubricScoreRow {
            rubric_score_id: format!("rs-{subject_id}"),
            adjudication_id: adjudication_id.clone(),
            subject_kind: self.spine.subject_kind().to_string(),
            rubric_hash: "rubric-v1".to_string(),
            contract_correctness: self.contract_correctness.to_string(),
            evidence_quality: self.evidence_quality.to_string(),
            safety: self.safety.to_string(),
            scope_discipline: self.scope_discipline.to_string(),
            intervention_burden: self.intervention_burden.to_string(),
            completion_integrity: self.completion_integrity.to_string(),
            adjudication_confidence: "high".to_string(),
            adjudicator_actor: "leader".to_string(),
            adjudicator_vendor: "codex".to_string(),
            independence_basis: self.independence_basis.to_string(),
            occurred_at: self.adjudicated_at.clone(),
            recorded_at: self.adjudicated_at.clone(),
        });
        let adjudication = self.with_adjudication.then(|| EvalAdjudicationFacts {
            adjudication_id,
            actor: "leader".to_string(),
            verdict: Some("APPROVED".to_string()),
            created_at: self.adjudicated_at.clone(),
            insertion_seq: self.adjudication_events as i64,
            event_count: self.adjudication_events,
        });
        // The mirror spine structurally has no route decision — the fixture
        // cannot build one even if a test asked for it.
        let route = (self.spine == EvalSpine::Dispatch).then(|| EvalRouteFacts {
            route_decision_id: format!("rd-{subject_id}"),
            assignment_mode: self.assignment_mode.to_string(),
            override_flag: false,
            recommendation_id: Some("rec-1".to_string()),
            recommended_profile: self.candidate_profiles.first().map(|p| p.to_string()),
            candidate_profiles: self
                .candidate_profiles
                .iter()
                .map(|p| p.to_string())
                .collect(),
            policy_source_revision: Some("rev-1".to_string()),
        });
        ProjectionRow {
            observation: EvalObservation {
                spine: self.spine,
                subject_id,
                dispatch_id: (self.spine == EvalSpine::Dispatch).then(|| "disp-1".to_string()),
                profile: Some(self.profile.to_string()),
                profile_attribution_basis: match self.spine {
                    EvalSpine::Dispatch => ProfileAttributionBasis::RouteDecision,
                    EvalSpine::Mirror => ProfileAttributionBasis::MirrorRequested,
                },
                model: Some("anthropic/claude-sonnet".to_string()),
                vendor: Some("claude".to_string()),
                task_type: self.task_type.map(str::to_string),
                terminal_outcome: self.terminal_outcome.map(str::to_string),
                identity_attribution_basis: Some("observed".to_string()),
                cost_tokens: self.cost_tokens,
                cost_usd: self.cost_usd,
                duration_ms: self.duration_ms,
                occurred_at: self.occurred_at.clone(),
                occurred_at_basis: OCCURRED_AT_BASIS_LEGACY_CREATED_AT,
                adjudication,
                rubric,
                route,
            },
            terminal: self.terminal,
        }
    }
}

fn rows(specs: Vec<Row>) -> Vec<ProjectionRow> {
    specs
        .into_iter()
        .enumerate()
        .map(|(index, row)| row.build(index))
        .collect()
}

fn repeat(row: Row, count: usize) -> Vec<Row> {
    (0..count).map(|_| row.clone()).collect()
}

fn eligible(profiles: &[&str]) -> Vec<String> {
    profiles.iter().map(|p| p.to_string()).collect()
}

fn run(rows: &[ProjectionRow], eligible_profiles: &[String]) -> ProjectionOutcome {
    project(rows, eligible_profiles, SINCE, None, NOW, Some(TASK_TYPE))
}

fn candidate<'a>(outcome: &'a ProjectionOutcome, profile: &str) -> &'a CandidateSummary {
    outcome
        .candidates
        .iter()
        .find(|candidate| candidate.profile == profile)
        .unwrap_or_else(|| panic!("{profile} missing from the projection output"))
}

fn position(outcome: &ProjectionOutcome, profile: &str) -> usize {
    outcome
        .candidates
        .iter()
        .position(|candidate| candidate.profile == profile)
        .unwrap_or_else(|| panic!("{profile} missing from the projection output"))
}

// ─── disc 1: deterministic recommendation ──────────────────────────────────

/// Discrimination 1: two candidates, each with adjudicated results in window,
/// produce a recommendation — and the SAME input produces byte-identical
/// output every run (fixed input, fixed output).
#[test]
fn disc1_two_reviewed_candidates_yield_a_deterministic_recommendation() {
    let mut specs = repeat(Row::default(), 3);
    specs.extend(repeat(
        Row {
            profile: "profile_b",
            safety: "concern",
            contract_correctness: "concern",
            completion_integrity: "concern",
            ..Row::default()
        },
        3,
    ));
    let rows = rows(specs);
    let eligible = eligible(&["profile_a", "profile_b"]);

    let outcome = run(&rows, &eligible);
    assert_eq!(outcome.usable_rows, 6);
    assert_eq!(
        outcome.decision,
        ProjectionDecision::Recommend {
            profile: "profile_a".to_string(),
            reasons: vec![
                "safety tier separates profile_a from profile_b by 0.500 (> band 0.289)"
                    .to_string()
            ],
        }
    );
    assert!(candidate(&outcome, "profile_a").qualified());

    let again = run(&rows, &eligible);
    assert_eq!(
        outcome.decision, again.decision,
        "same rows, same decision — the projection is a pure function"
    );
    assert_eq!(
        outcome
            .candidates
            .iter()
            .map(|candidate| candidate.profile.clone())
            .collect::<Vec<_>>(),
        again
            .candidates
            .iter()
            .map(|candidate| candidate.profile.clone())
            .collect::<Vec<_>>(),
        "candidate order is a total order, not hash-map luck"
    );
}

/// Latency on the dispatch spine is reported as `not_available`, never as a
/// fabricated number (design D3 / codex finding 3).
#[test]
fn dispatch_spine_latency_is_structurally_not_available() {
    let rows = rows(repeat(Row::default(), 3));
    let outcome = run(&rows, &eligible(&["profile_a"]));
    let candidate = candidate(&outcome, "profile_a");
    assert_eq!(candidate.metrics.latency, LatencyMetric::NotAvailable);
    assert_eq!(candidate.metrics.latency.to_json(), json!("not_available"));
}

// ─── disc 4: safety is unbuyable ───────────────────────────────────────────

/// Discrimination 4: a candidate whose safety dimension FAILS cannot be
/// bought back by being cheaper and faster. This is only provable
/// lexicographically — under any weighted sum, a large enough cost advantage
/// eventually wins.
#[test]
fn disc4_safety_fail_is_not_buyable_with_cheaper_cost_or_latency() {
    let mut specs = repeat(
        Row {
            profile: "cheap_unsafe",
            safety: "fail",
            // Everything else about it is excellent, and it is free.
            cost_usd: Some(0.0),
            cost_tokens: Some(1),
            ..Row::default()
        },
        3,
    );
    specs.extend(repeat(
        Row {
            profile: "safe_expensive",
            safety: "pass",
            // Two orders of magnitude more expensive.
            cost_usd: Some(100.0),
            cost_tokens: Some(1_000_000),
            ..Row::default()
        },
        3,
    ));
    let eligible = eligible(&["cheap_unsafe", "safe_expensive"]);
    let outcome = run(&rows(specs), &eligible);

    assert_eq!(
        outcome.decision,
        ProjectionDecision::Recommend {
            profile: "safe_expensive".to_string(),
            reasons: vec![
                "safety tier is decisive: safe_expensive has no failing judgment, \
                 cheap_unsafe does"
                    .to_string()
            ],
        }
    );
    assert!(
        position(&outcome, "safe_expensive") < position(&outcome, "cheap_unsafe"),
        "a safety failure ranks last regardless of price"
    );
    assert_eq!(candidate(&outcome, "cheap_unsafe").metrics.safety.fail, 3);
}

/// The same claim stated at the comparator level, so a future refactor that
/// reintroduces a weighted sum fails here first: cost is consulted ONLY after
/// every judged tier has already tied.
#[test]
fn cost_only_breaks_ties_after_every_judged_tier_is_equal() {
    let mut specs = repeat(
        Row {
            profile: "cheap",
            cost_usd: Some(0.1),
            ..Row::default()
        },
        3,
    );
    specs.extend(repeat(
        Row {
            profile: "pricey",
            cost_usd: Some(9.0),
            ..Row::default()
        },
        3,
    ));
    let outcome = run(&rows(specs), &eligible(&["cheap", "pricey"]));
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Recommend { profile, reasons }
            if profile == "cheap" && reasons[0].contains("broken by lower cost/latency")
    ));

    // Now give the pricey one a better judged record: cost must NOT rescue
    // the cheap one.
    let mut specs = repeat(
        Row {
            profile: "cheap",
            evidence_quality: "concern",
            scope_discipline: "concern",
            intervention_burden: "concern",
            cost_usd: Some(0.1),
            ..Row::default()
        },
        4,
    );
    specs.extend(repeat(
        Row {
            profile: "pricey",
            cost_usd: Some(9.0),
            ..Row::default()
        },
        4,
    ));
    let outcome = run(&rows(specs), &eligible(&["cheap", "pricey"]));
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Recommend { profile, .. } if profile == "pricey"
    ));
}

// ─── disc 5: hard gates first ──────────────────────────────────────────────

/// Discrimination 5: a candidate the CURRENT admission rules removed never
/// reappears, however good its history is — the excluded rows are counted
/// and reported, not silently dropped.
#[test]
fn disc5_removed_candidate_never_resurrects_on_history() {
    let mut specs = repeat(
        Row {
            profile: "blocked_star",
            candidate_profiles: vec!["blocked_star", "plain_survivor"],
            ..Row::default()
        },
        10,
    );
    specs.extend(repeat(
        Row {
            profile: "plain_survivor",
            // Deliberately WORSE than the blocked candidate.
            evidence_quality: "concern",
            scope_discipline: "concern",
            candidate_profiles: vec!["blocked_star", "plain_survivor"],
            ..Row::default()
        },
        3,
    ));
    // The hard gate has already removed `blocked_star` from the eligible set.
    let outcome = run(&rows(specs), &eligible(&["plain_survivor"]));

    assert!(
        !outcome
            .candidates
            .iter()
            .any(|candidate| candidate.profile == "blocked_star"),
        "a removed candidate must not appear in the projection at all"
    );
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::NOT_IN_ELIGIBLE_CANDIDATE_SET),
        Some(&10),
        "its rows are counted under an explicit reason, not dropped"
    );
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Recommend { profile, .. } if profile == "plain_survivor"
    ));
}

// ─── disc 6: abstain ───────────────────────────────────────────────────────

/// Discrimination 6a: a single sample never produces a confident winner.
#[test]
fn disc6_single_sample_abstains() {
    let outcome = run(&rows(repeat(Row::default(), 1)), &eligible(&["profile_a"]));
    assert_eq!(candidate(&outcome, "profile_a").usable_rows, 1);
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Abstain { reason, .. } if *reason == abstain::INSUFFICIENT_EVIDENCE
    ));
}

/// Discrimination 6b: evidence that has aged out of the window is excluded
/// as `outside_window`, so a stale record abstains instead of coasting on
/// old wins.
#[test]
fn disc6_stale_evidence_falls_out_of_window_and_abstains() {
    let specs = repeat(
        Row {
            occurred_at: BEFORE_WINDOW.to_string(),
            ..Row::default()
        },
        5,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(outcome.usable_rows, 0);
    assert_eq!(
        outcome.excluded_counts.get(reason::OUTSIDE_WINDOW),
        Some(&5)
    );
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Abstain { reason, .. } if *reason == abstain::INSUFFICIENT_EVIDENCE
    ));
}

/// Discrimination 6c: a top-2 difference smaller than the combined
/// uncertainty band is noise, not a winner.
#[test]
fn disc6_top_two_inside_the_uncertainty_band_abstains() {
    let mut specs = repeat(Row::default(), 3);
    specs.extend(repeat(
        Row {
            profile: "profile_b",
            ..Row::default()
        },
        2,
    ));
    // One single differing judgment out of three.
    specs.push(Row {
        profile: "profile_b",
        safety: "concern",
        ..Row::default()
    });
    let outcome = run(&rows(specs), &eligible(&["profile_a", "profile_b"]));
    assert!(candidate(&outcome, "profile_a").qualified());
    assert!(candidate(&outcome, "profile_b").qualified());
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Abstain { reason, .. }
            if *reason == abstain::TOP_TWO_UNCERTAINTY_OVERLAP
    ));
}

/// Discrimination 6d: an overturn newer than the settling window keeps the
/// projection silent — the evidence base is still moving.
#[test]
fn disc6_recent_overturn_inside_settling_window_abstains() {
    let mut specs = repeat(Row::default(), 3);
    specs.push(Row {
        adjudication_events: 2,
        // 1 hour before NOW, far inside the 72h settling window.
        adjudicated_at: "2026-08-09T23:00:00.000Z".to_string(),
        ..Row::default()
    });
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Abstain { reason, .. }
            if *reason == abstain::RECENT_OVERTURN_UNSETTLED
    ));

    // The same overturn, long settled, no longer silences the projection.
    let mut specs = repeat(Row::default(), 3);
    specs.push(Row {
        adjudication_events: 2,
        adjudicated_at: "2026-07-20T00:00:00.000Z".to_string(),
        ..Row::default()
    });
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Recommend { .. }
    ));
}

/// The no-evidence branch is abstain — never a baseline MBIT fit (design D7).
#[test]
fn no_evidence_at_all_abstains() {
    let outcome = run(&[], &eligible(&["profile_a", "profile_b"]));
    assert_eq!(outcome.rows_considered, 0);
    assert_eq!(
        outcome.candidates.len(),
        2,
        "eligible set is still reported"
    );
    match &outcome.decision {
        ProjectionDecision::Abstain { reason, detail } => {
            assert_eq!(*reason, abstain::INSUFFICIENT_EVIDENCE);
            assert!(detail.contains("profile_a=0"));
        }
        other => panic!("expected abstain, got {other:?}"),
    }
}

// ─── retained-not-trained / unusable rows ──────────────────────────────────

/// `user_forced` and `experiment` rows stay queryable and counted — and
/// train nothing.
#[test]
fn user_forced_and_experiment_rows_are_counted_but_never_trained() {
    let mut specs = repeat(
        Row {
            assignment_mode: "user_forced",
            ..Row::default()
        },
        3,
    );
    specs.extend(repeat(
        Row {
            assignment_mode: "experiment",
            ..Row::default()
        },
        2,
    ));
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));

    assert_eq!(outcome.usable_rows, 0, "neither mode trains anything");
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::ASSIGNMENT_MODE_USER_FORCED),
        Some(&3)
    );
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::ASSIGNMENT_MODE_EXPERIMENT),
        Some(&2)
    );
    let candidate = candidate(&outcome, "profile_a");
    assert_eq!(candidate.metrics.samples, 0);
    assert_eq!(
        candidate
            .excluded_counts
            .get(reason::ASSIGNMENT_MODE_USER_FORCED),
        Some(&3),
        "the counts are visible per candidate, so the rows stay queryable"
    );
    // ...and they are individually explainable, not just tallied.
    assert!(outcome
        .explained_exclusions
        .iter()
        .any(|row| row.reason == reason::ASSIGNMENT_MODE_USER_FORCED));
}

/// A legacy free-text adjudication carrying no rubric row is excluded as
/// `unstructured_verdict` — legacy rows age out of policy learning with zero
/// migration (design D3).
#[test]
fn legacy_verdict_without_a_rubric_row_is_unstructured_verdict() {
    let specs = repeat(
        Row {
            with_rubric: false,
            ..Row::default()
        },
        4,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(
        outcome.excluded_counts.get(reason::UNSTRUCTURED_VERDICT),
        Some(&4)
    );
    assert_eq!(outcome.usable_rows, 0);

    // A row never adjudicated at all is a DIFFERENT state and says so.
    let specs = repeat(
        Row {
            with_rubric: false,
            with_adjudication: false,
            ..Row::default()
        },
        2,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(
        outcome.excluded_counts.get(reason::NOT_ADJUDICATED),
        Some(&2)
    );
}

/// Only `structural_cross_vendor` feeds positive routing evidence (design
/// D5); `declared_only` and `self` are kept as evidence and excluded from
/// labels.
#[test]
fn ineligible_independence_bases_are_excluded_with_distinct_reasons() {
    let mut specs = repeat(
        Row {
            independence_basis: "declared_only",
            ..Row::default()
        },
        3,
    );
    specs.extend(repeat(
        Row {
            independence_basis: "self",
            ..Row::default()
        },
        2,
    ));
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::INDEPENDENCE_DECLARED_ONLY),
        Some(&3)
    );
    assert_eq!(
        outcome.excluded_counts.get(reason::INDEPENDENCE_SELF),
        Some(&2)
    );
    assert_eq!(outcome.usable_rows, 0);
}

/// Terminal state must exist AND agree with `status.json`; a mismatch and an
/// unverifiable receipt are distinct, and neither is usable.
#[test]
fn terminal_state_problems_are_excluded_with_distinct_reasons() {
    let specs = vec![
        Row {
            terminal_outcome: None,
            ..Row::default()
        },
        Row {
            terminal: TerminalCheck::Mismatch,
            ..Row::default()
        },
        Row {
            terminal: TerminalCheck::Unverifiable,
            ..Row::default()
        },
    ];
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(outcome.usable_rows, 0);
    assert_eq!(
        outcome.excluded_counts.get(reason::NO_TERMINAL_STATE),
        Some(&1)
    );
    assert_eq!(
        outcome.excluded_counts.get(reason::TERMINAL_STATE_MISMATCH),
        Some(&1)
    );
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::TERMINAL_STATE_UNVERIFIABLE),
        Some(&1)
    );
}

// ─── mirror spine: quality evidence only, forever ──────────────────────────

/// NEGATIVE TEST: mirror rows never contribute candidate-set-level routing
/// evidence. Ten flawless mirror rows still cannot make a recommendation —
/// they are retained as quality evidence and counted, and the projection
/// abstains for lack of on-policy rows.
#[test]
fn mirror_rows_never_contribute_candidate_set_routing_evidence() {
    let specs = repeat(
        Row {
            spine: EvalSpine::Mirror,
            duration_ms: Some(1_500),
            // A mirror run has no status.json to reconcile against — Tachi
            // never dispatched it.
            terminal: TerminalCheck::NotApplicable,
            // A mirror run carries no task type of its own.
            task_type: None,
            ..Row::default()
        },
        10,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));

    assert_eq!(
        outcome.usable_rows, 0,
        "no number of mirror rows becomes routing evidence"
    );
    assert_eq!(outcome.quality_only_rows, 10);
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::MIRROR_SPINE_NO_CANDIDATE_SET),
        Some(&10)
    );
    let candidate = candidate(&outcome, "profile_a");
    assert!(!candidate.qualified());
    assert_eq!(candidate.metrics.samples, 0, "routing vector stays empty");
    assert_eq!(
        candidate.quality_metrics.samples, 10,
        "the quality vector keeps them"
    );
    // The mirror spine is the only place a real latency exists today.
    assert_eq!(
        candidate.quality_metrics.latency,
        LatencyMetric::AvgMs(1_500.0)
    );
    assert!(matches!(
        &outcome.decision,
        ProjectionDecision::Abstain { reason, .. } if *reason == abstain::INSUFFICIENT_EVIDENCE
    ));
}

/// A dispatch row whose decision recorded no candidate set (`unadvised`, the
/// day-one shape) is off-policy too: retained as quality evidence, never
/// counted toward N_min.
#[test]
fn unadvised_dispatch_rows_are_quality_evidence_only() {
    let specs = repeat(
        Row {
            assignment_mode: "unadvised",
            candidate_profiles: Vec::new(),
            ..Row::default()
        },
        5,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(outcome.usable_rows, 0);
    assert_eq!(outcome.quality_only_rows, 5);
    assert_eq!(
        outcome
            .excluded_counts
            .get(reason::NO_DECISION_CANDIDATE_SET),
        Some(&5)
    );
}

/// Evidence about a different task type is not evidence about this decision.
#[test]
fn rows_from_another_task_type_are_excluded() {
    let specs = repeat(
        Row {
            task_type: Some("explain_request"),
            ..Row::default()
        },
        3,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(
        outcome.excluded_counts.get(reason::TASK_TYPE_MISMATCH),
        Some(&3)
    );
    assert_eq!(outcome.usable_rows, 0);
}

// ─── helpers under test ────────────────────────────────────────────────────

/// Window arithmetic parses real instants; it never compares or subtracts
/// timestamp STRINGS.
#[test]
fn window_since_is_instant_arithmetic() {
    assert_eq!(window_since(NOW, 30).as_deref(), Some(SINCE));
    assert_eq!(
        shift_iso("2026-08-10T00:00:00.000Z", -3_600).as_deref(),
        Some("2026-08-09T23:00:00.000Z")
    );
    assert_eq!(shift_iso("not-a-timestamp", -1), None);
}

/// A legacy `created_at` written without milliseconds still compares
/// correctly against a normalized window bound (byte-wise, `...00Z` sorts
/// AFTER `...00.000Z`, which would silently mis-window every legacy row).
#[test]
fn window_comparison_normalizes_legacy_timestamps() {
    let specs = repeat(
        Row {
            occurred_at: "2026-08-01T00:00:00Z".to_string(),
            ..Row::default()
        },
        3,
    );
    let outcome = run(&rows(specs), &eligible(&["profile_a"]));
    assert_eq!(
        outcome.usable_rows, 3,
        "a second-precision legacy timestamp is still inside the window"
    );
}
