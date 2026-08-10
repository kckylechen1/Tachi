//! Pure decision rules for the ledger-backed route projection (tachi#1675
//! PR2, design D6). No IO, no store handle, no clock: every input —
//! including `now` — is passed in, so a projection run is a deterministic
//! function of (rows, eligible set, window, now).
//!
//! Three rules live here, in the order the design states them:
//!
//! 1. **Hard gates first.** The eligible candidate set is computed by the
//!    EXISTING admission/required/blocked logic *before* any scoring
//!    ([`super::eligible_candidate_set`]), and a row for a candidate that is
//!    not in it is excluded outright. Historical score can never resurrect a
//!    removed candidate (discrimination 5).
//! 2. **Per-row usability.** Terminal state exists and agrees with
//!    `status.json` ∧ a rubric row is present ∧ `independence_basis` is
//!    eligible ∧ `assignment_mode ∉ {user_forced, experiment}` ∧ the
//!    candidate was in the decision-time eligible set ∧ the row is in window.
//! 3. **Lexicographic tiers, never weights.** Safety compares first,
//!    correctness second, process third, and cost/latency only ever
//!    tie-break *within* an already-equal tier. A weighted sum can always be
//!    bought down by cheap-enough cost, which is precisely why discrimination
//!    4 ("a safety-failing candidate is unbuyable") is only provable
//!    lexicographically.
//!
//! And one refusal: with fewer than [`N_MIN_USABLE_ROWS`] usable rows, a
//! single sample, a top-2 difference inside the uncertainty band, or an
//! overturn newer than the settling window, the projection ABSTAINS. The
//! no-evidence branch is abstain too — never `baseline_mbit_fit` (design D7).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::{json, Value};

use memcore::{EvalObservation, EvalSpine};

/// Bumped explicitly whenever these rules change meaning. PR4's evidence
/// flip is a policy_version bump, never a silent change (design D6 phase 2).
pub(crate) const POLICY_VERSION: &str = "route_projection/v1";

/// What every response on this path declares its evidence came from, so a
/// consumer can tell the ledger path from the `/eval`-memory path while both
/// run in parallel (design D6 phase 1).
pub(crate) const EVIDENCE_SOURCE: &str = "decision_fact_ledger";

/// Minimum usable rows in window before a candidate may be recommended at
/// all. A code constant in this phase on purpose: promoting it to
/// `tachi_tune` would widen that tool's closed 8-action set, which the design
/// defers.
pub(crate) const N_MIN_USABLE_ROWS: usize = 3;

pub(crate) const DEFAULT_WINDOW_DAYS: u32 = 30;
pub(crate) const MAX_WINDOW_DAYS: u32 = 365;

/// How long an overturn (a correction appended after an earlier judgment)
/// keeps the evidence base "unsettled". Inside this window the projection
/// abstains rather than acting on a verdict that just moved.
pub(crate) const OVERTURN_SETTLING_SECS: i64 = 72 * 3600;

/// Half-width coefficient of the uncertainty band: `band(n) = K / sqrt(n)`.
/// With ordinal scores in `[0, 1]`, `0.5 / sqrt(n)` is the normal-approx
/// standard error of a maximally-uncertain mean; K is deliberately set to
/// HALF of that, so a difference must exceed roughly one standard error per
/// side before it counts as a claim. This is a conservative code constant,
/// not a statistical guarantee — it exists to stop a one-row difference at
/// n=3 from being reported as a winner.
pub(crate) const UNCERTAINTY_BAND_K: f64 = 0.25;

/// The ONLY `independence_basis` eligible to feed positive routing evidence
/// in this phase (design D5).
pub(crate) const ELIGIBLE_INDEPENDENCE_BASIS: &str = "structural_cross_vendor";

/// Retained, counted, queryable — and never trained on (design D6).
pub(crate) const RETAINED_NOT_TRAINED_ASSIGNMENT_MODES: &[&str] = &["user_forced", "experiment"];

/// Cap on the per-response raw-row explain list. The rows themselves stay
/// independently inspectable in the ledger; this only bounds the response.
pub(crate) const MAX_EXPLAINED_EXCLUSIONS: usize = 50;

/// Closed vocabulary of per-row exclusion reasons. Every excluded row lands
/// in exactly one of these, and the counts are reported — an excluded row is
/// never silently dropped.
pub(crate) mod reason {
    pub(crate) const OUTSIDE_WINDOW: &str = "outside_window";
    pub(crate) const UNATTRIBUTED_PROFILE: &str = "unattributed_profile";
    pub(crate) const NOT_IN_ELIGIBLE_CANDIDATE_SET: &str = "not_in_eligible_candidate_set";
    /// The row is evidence about a DIFFERENT task type than the one being
    /// projected. Every existing scorer in `tachi_dispatch::routing` matches
    /// on task type; carrying `explain_request` evidence into a
    /// `migration_request` decision would be the projection making a claim
    /// its rows do not support. Only applied when BOTH sides record a task
    /// type — a row with none is never assumed to mismatch.
    pub(crate) const TASK_TYPE_MISMATCH: &str = "task_type_mismatch";
    pub(crate) const NO_TERMINAL_STATE: &str = "no_terminal_state";
    pub(crate) const TERMINAL_STATE_MISMATCH: &str = "terminal_state_mismatch";
    pub(crate) const TERMINAL_STATE_UNVERIFIABLE: &str = "terminal_state_unverifiable";
    pub(crate) const NOT_ADJUDICATED: &str = "not_adjudicated";
    /// A judgment exists but carries no structured rubric row — the legacy
    /// free-text verdict (design D3). Ages out of policy learning naturally.
    pub(crate) const UNSTRUCTURED_VERDICT: &str = "unstructured_verdict";
    pub(crate) const INDEPENDENCE_DECLARED_ONLY: &str = "independence_declared_only";
    pub(crate) const INDEPENDENCE_SELF: &str = "independence_self";
    pub(crate) const INDEPENDENCE_INELIGIBLE: &str = "independence_ineligible";
    pub(crate) const ASSIGNMENT_MODE_USER_FORCED: &str = "assignment_mode_user_forced";
    pub(crate) const ASSIGNMENT_MODE_EXPERIMENT: &str = "assignment_mode_experiment";
    /// Retained as QUALITY evidence, never as candidate-set-level routing
    /// evidence: the mirror spine structurally has no candidate set.
    pub(crate) const MIRROR_SPINE_NO_CANDIDATE_SET: &str = "mirror_spine_no_candidate_set";
    /// Dispatch row with no recommendation-linked decision (`unadvised`), or
    /// a decision whose recorded candidate set did not contain this
    /// candidate: off-policy, so quality evidence only.
    pub(crate) const NO_DECISION_CANDIDATE_SET: &str = "no_decision_candidate_set";
}

/// Abstain reasons (closed set).
pub(crate) mod abstain {
    pub(crate) const INSUFFICIENT_EVIDENCE: &str = "insufficient_usable_evidence";
    pub(crate) const RECENT_OVERTURN_UNSETTLED: &str = "recent_overturn_unsettled";
    pub(crate) const TOP_TWO_UNCERTAINTY_OVERLAP: &str = "top_two_uncertainty_overlap";
    pub(crate) const NO_DISCRIMINATING_EVIDENCE: &str = "no_discriminating_evidence";
}

/// Result of cross-checking a durable terminal row against the live receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalCheck {
    /// Durable `execution_outcome` agrees with the run's `status.json`.
    Consistent,
    /// They disagree — the row is evidence of a bookkeeping fault, not of
    /// candidate quality.
    Mismatch,
    /// No readable `status.json` (run dir pruned, archived, or unreadable).
    /// The design's rule is "terminal state exists AND matches status.json";
    /// an unverifiable claim is not a match, so it is excluded rather than
    /// optimistically accepted.
    Unverifiable,
    /// The mirror spine has no `status.json` by construction — Tachi never
    /// dispatched that work. Its observation row IS the terminal receipt.
    NotApplicable,
}

/// One row plus its terminal cross-check, as handed to the pure rules.
#[derive(Debug, Clone)]
pub(crate) struct ProjectionRow {
    pub(crate) observation: EvalObservation,
    pub(crate) terminal: TerminalCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowDisposition {
    /// On-policy: counts toward N_min and toward the routing metric vector.
    Usable,
    /// Retained and reported, contributes QUALITY evidence only, never
    /// candidate-set-level routing evidence and never N_min.
    QualityOnly(&'static str),
    Excluded(&'static str),
}

/// Normalize a timestamp for lexicographic comparison. The #1432 contract is
/// RFC3339-ms-Z with lexicographic ordering, but legacy `created_at` values
/// were not necessarily written through `now_utc_iso` (a bare `...:00Z` sorts
/// AFTER `...:00.000Z` byte-wise), so both sides are normalized before any
/// window comparison.
fn normalized(ts: &str) -> String {
    memcore::normalize_utc_iso(ts).unwrap_or_else(|_| ts.to_string())
}

/// Shift an RFC3339 timestamp by `delta_secs`, parsed as a real instant —
/// never by string arithmetic.
pub(crate) fn shift_iso(ts: &str, delta_secs: i64) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(ts.trim()).ok()?;
    let shifted = parsed.with_timezone(&chrono::Utc) + chrono::Duration::seconds(delta_secs);
    Some(shifted.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Window start for `now - days`.
pub(crate) fn window_since(now: &str, days: u32) -> Option<String> {
    shift_iso(now, -(days as i64) * 86_400)
}

fn in_window(ts: &str, since: &str, until: Option<&str>) -> bool {
    let ts = normalized(ts);
    let since = normalized(since);
    if ts < since {
        return false;
    }
    match until {
        Some(until) => ts < normalized(until),
        None => true,
    }
}

/// The per-row usability rule (design D6), evaluated in the design's own
/// order so the reported reason is the FIRST thing that disqualified the row.
pub(crate) fn classify_row(
    row: &ProjectionRow,
    eligible_profiles: &[String],
    since: &str,
    until: Option<&str>,
    task_type: Option<&str>,
) -> RowDisposition {
    let observation = &row.observation;

    if !in_window(&observation.occurred_at, since, until) {
        return RowDisposition::Excluded(reason::OUTSIDE_WINDOW);
    }

    let Some(profile) = observation.profile.as_deref() else {
        return RowDisposition::Excluded(reason::UNATTRIBUTED_PROFILE);
    };

    // Hard gate BEFORE any scoring: a candidate the current admission logic
    // removed contributes nothing, however good its history is.
    if !eligible_profiles.iter().any(|p| p == profile) {
        return RowDisposition::Excluded(reason::NOT_IN_ELIGIBLE_CANDIDATE_SET);
    }

    // Task-type scoping, applied only when both sides state one.
    if let (Some(query), Some(row_task_type)) = (
        task_type.map(str::trim).filter(|t| !t.is_empty()),
        observation
            .task_type
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty()),
    ) {
        if query != row_task_type {
            return RowDisposition::Excluded(reason::TASK_TYPE_MISMATCH);
        }
    }

    if observation.terminal_outcome.is_none() {
        return RowDisposition::Excluded(reason::NO_TERMINAL_STATE);
    }
    match row.terminal {
        TerminalCheck::Mismatch => {
            return RowDisposition::Excluded(reason::TERMINAL_STATE_MISMATCH)
        }
        TerminalCheck::Unverifiable => {
            return RowDisposition::Excluded(reason::TERMINAL_STATE_UNVERIFIABLE)
        }
        TerminalCheck::Consistent | TerminalCheck::NotApplicable => {}
    }

    if observation.adjudication.is_none() {
        return RowDisposition::Excluded(reason::NOT_ADJUDICATED);
    }
    let Some(rubric) = observation.rubric.as_ref() else {
        return RowDisposition::Excluded(reason::UNSTRUCTURED_VERDICT);
    };

    if rubric.independence_basis != ELIGIBLE_INDEPENDENCE_BASIS {
        return RowDisposition::Excluded(match rubric.independence_basis.as_str() {
            "declared_only" => reason::INDEPENDENCE_DECLARED_ONLY,
            "self" => reason::INDEPENDENCE_SELF,
            _ => reason::INDEPENDENCE_INELIGIBLE,
        });
    }

    if let Some(route) = observation.route.as_ref() {
        let mode = route.assignment_mode.as_str();
        if RETAINED_NOT_TRAINED_ASSIGNMENT_MODES.contains(&mode) {
            return RowDisposition::Excluded(if mode == "user_forced" {
                reason::ASSIGNMENT_MODE_USER_FORCED
            } else {
                reason::ASSIGNMENT_MODE_EXPERIMENT
            });
        }
    }

    if observation.binds_decision_candidate_set() {
        RowDisposition::Usable
    } else if observation.spine == EvalSpine::Mirror {
        RowDisposition::QualityOnly(reason::MIRROR_SPINE_NO_CANDIDATE_SET)
    } else {
        RowDisposition::QualityOnly(reason::NO_DECISION_CANDIDATE_SET)
    }
}

// ─── metric vector ─────────────────────────────────────────────────────────

/// Ordinal tallies for one judged rubric dimension. Deliberately NOT a float
/// field: floats invite averaging into a single reputation number, which the
/// design forbids. The derived [`DimensionSummary::score`] exists only to
/// order candidates WITHIN one lexicographic tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DimensionSummary {
    pub(crate) pass: usize,
    pub(crate) concern: usize,
    pub(crate) fail: usize,
    pub(crate) not_assessed: usize,
}

impl DimensionSummary {
    fn observe(&mut self, value: &str) {
        match value {
            "pass" => self.pass += 1,
            "concern" => self.concern += 1,
            "fail" => self.fail += 1,
            _ => self.not_assessed += 1,
        }
    }

    pub(crate) fn assessed(&self) -> usize {
        self.pass + self.concern + self.fail
    }

    pub(crate) fn score(&self) -> Option<f64> {
        let assessed = self.assessed();
        (assessed > 0).then(|| (self.pass as f64 + 0.5 * self.concern as f64) / assessed as f64)
    }

    pub(crate) fn to_json(self) -> Value {
        json!({
            "pass": self.pass,
            "concern": self.concern,
            "fail": self.fail,
            "not_assessed": self.not_assessed,
            "score": self.score(),
        })
    }
}

/// Latency is a machine fact, not a judged dimension — and it does not exist
/// on the dispatch spine at all (design D3 / codex finding 3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LatencyMetric {
    /// No row in this set carried a duration. On the dispatch spine this is
    /// STRUCTURAL: `dispatch_outcomes` has no duration column.
    NotAvailable,
    AvgMs(f64),
}

impl LatencyMetric {
    fn value(self) -> Option<f64> {
        match self {
            LatencyMetric::NotAvailable => None,
            LatencyMetric::AvgMs(ms) => Some(ms),
        }
    }

    pub(crate) fn to_json(self) -> Value {
        match self {
            LatencyMetric::NotAvailable => json!("not_available"),
            LatencyMetric::AvgMs(ms) => json!(ms),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MetricVector {
    pub(crate) safety: DimensionSummary,
    pub(crate) contract_correctness: DimensionSummary,
    pub(crate) completion_integrity: DimensionSummary,
    pub(crate) evidence_quality: DimensionSummary,
    pub(crate) scope_discipline: DimensionSummary,
    pub(crate) intervention_burden: DimensionSummary,
    pub(crate) avg_cost_tokens: Option<f64>,
    pub(crate) avg_cost_usd: Option<f64>,
    pub(crate) latency: LatencyMetric,
    pub(crate) samples: usize,
}

impl Default for MetricVector {
    fn default() -> Self {
        Self {
            safety: DimensionSummary::default(),
            contract_correctness: DimensionSummary::default(),
            completion_integrity: DimensionSummary::default(),
            evidence_quality: DimensionSummary::default(),
            scope_discipline: DimensionSummary::default(),
            intervention_burden: DimensionSummary::default(),
            avg_cost_tokens: None,
            avg_cost_usd: None,
            latency: LatencyMetric::NotAvailable,
            samples: 0,
        }
    }
}

impl MetricVector {
    pub(crate) fn to_json(self) -> Value {
        json!({
            "samples": self.samples,
            "safety": self.safety.to_json(),
            "contract_correctness": self.contract_correctness.to_json(),
            "completion_integrity": self.completion_integrity.to_json(),
            "evidence_quality": self.evidence_quality.to_json(),
            "scope_discipline": self.scope_discipline.to_json(),
            "intervention_burden": self.intervention_burden.to_json(),
            "avg_cost_tokens": self.avg_cost_tokens,
            "avg_cost_usd": self.avg_cost_usd,
            "latency_ms": self.latency.to_json(),
        })
    }

    /// Tier 1 — safety. Compared first and alone: nothing below can trade
    /// against it.
    fn tier_safety(&self) -> TierScore {
        TierScore::of(&[&self.safety])
    }

    /// Tier 2 — correctness (did the contract get met, and was the finish
    /// real).
    fn tier_correctness(&self) -> TierScore {
        TierScore::of(&[&self.contract_correctness, &self.completion_integrity])
    }

    /// Tier 3 — process discipline.
    fn tier_process(&self) -> TierScore {
        TierScore::of(&[
            &self.evidence_quality,
            &self.scope_discipline,
            &self.intervention_burden,
        ])
    }

    fn tiers(&self) -> [(&'static str, TierScore); 3] {
        [
            ("safety", self.tier_safety()),
            ("correctness", self.tier_correctness()),
            ("process", self.tier_process()),
        ]
    }
}

fn build_metric_vector<'a, I>(rows: I) -> MetricVector
where
    I: IntoIterator<Item = &'a EvalObservation>,
{
    let mut vector = MetricVector::default();
    let mut cost_tokens_sum = 0f64;
    let mut cost_tokens_n = 0usize;
    let mut cost_usd_sum = 0f64;
    let mut cost_usd_n = 0usize;
    let mut latency_sum = 0f64;
    let mut latency_n = 0usize;

    for row in rows {
        vector.samples += 1;
        if let Some(rubric) = row.rubric.as_ref() {
            vector.safety.observe(&rubric.safety);
            vector
                .contract_correctness
                .observe(&rubric.contract_correctness);
            vector
                .completion_integrity
                .observe(&rubric.completion_integrity);
            vector.evidence_quality.observe(&rubric.evidence_quality);
            vector.scope_discipline.observe(&rubric.scope_discipline);
            vector
                .intervention_burden
                .observe(&rubric.intervention_burden);
        }
        if let Some(tokens) = row.cost_tokens {
            cost_tokens_sum += tokens as f64;
            cost_tokens_n += 1;
        }
        if let Some(usd) = row.cost_usd {
            cost_usd_sum += usd;
            cost_usd_n += 1;
        }
        // Only the mirror spine can ever contribute here (design D3).
        if let Some(duration) = row.duration_ms {
            latency_sum += duration as f64;
            latency_n += 1;
        }
    }

    vector.avg_cost_tokens = (cost_tokens_n > 0).then(|| cost_tokens_sum / cost_tokens_n as f64);
    vector.avg_cost_usd = (cost_usd_n > 0).then(|| cost_usd_sum / cost_usd_n as f64);
    vector.latency = if latency_n > 0 {
        LatencyMetric::AvgMs(latency_sum / latency_n as f64)
    } else {
        LatencyMetric::NotAvailable
    };
    vector
}

/// One lexicographic tier's comparable summary.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TierScore {
    /// Any `fail` anywhere in the tier. Categorical: a tier with a fail is
    /// worse than one without, at ANY score, and no lower-tier cost or
    /// latency is consulted afterwards.
    has_fail: bool,
    score: Option<f64>,
    assessed: usize,
}

impl TierScore {
    fn of(dimensions: &[&DimensionSummary]) -> Self {
        let has_fail = dimensions.iter().any(|d| d.fail > 0);
        let assessed = dimensions.iter().map(|d| d.assessed()).sum::<usize>();
        let weighted = dimensions
            .iter()
            .map(|d| d.pass as f64 + 0.5 * d.concern as f64)
            .sum::<f64>();
        Self {
            has_fail,
            score: (assessed > 0).then(|| weighted / assessed as f64),
            assessed,
        }
    }

    /// Half-width of this tier's uncertainty band.
    fn band(&self) -> f64 {
        if self.assessed == 0 {
            f64::INFINITY
        } else {
            UNCERTAINTY_BAND_K / (self.assessed as f64).sqrt()
        }
    }
}

/// `Ordering::Less` means "ranks BEFORE" (better).
fn compare_tier(a: &TierScore, b: &TierScore) -> Ordering {
    a.has_fail
        .cmp(&b.has_fail)
        .then_with(|| match (a.score, b.score) {
            (Some(left), Some(right)) => right.partial_cmp(&left).unwrap_or(Ordering::Equal),
            // An assessed tier beats an unassessed one: absence of judgment
            // is not evidence of quality.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
}

/// Lower is better; a MISSING value is no signal at all (never a penalty and
/// never a bonus).
fn compare_lower_is_better(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    }
}

// ─── candidates ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct CandidateSummary {
    pub(crate) profile: String,
    /// Computed over USABLE (on-policy) rows only.
    pub(crate) metrics: MetricVector,
    /// Computed over quality-only rows (mirror rows, and dispatch rows with
    /// no decision-time candidate set). Reported, never routed on.
    pub(crate) quality_metrics: MetricVector,
    pub(crate) usable_rows: usize,
    pub(crate) quality_only_rows: usize,
    pub(crate) excluded_counts: BTreeMap<String, usize>,
    pub(crate) first_occurred_at: Option<String>,
    pub(crate) last_occurred_at: Option<String>,
    /// Event time of the most recent overturn among this candidate's usable
    /// rows, if any.
    pub(crate) latest_overturn_at: Option<String>,
}

impl CandidateSummary {
    fn new(profile: &str) -> Self {
        Self {
            profile: profile.to_string(),
            metrics: MetricVector::default(),
            quality_metrics: MetricVector::default(),
            usable_rows: 0,
            quality_only_rows: 0,
            excluded_counts: BTreeMap::new(),
            first_occurred_at: None,
            last_occurred_at: None,
            latest_overturn_at: None,
        }
    }

    pub(crate) fn qualified(&self) -> bool {
        self.usable_rows >= N_MIN_USABLE_ROWS
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "profile": self.profile,
            "qualified": self.qualified(),
            "evidence_window": {
                "usable_rows": self.usable_rows,
                "quality_only_rows": self.quality_only_rows,
                "first_occurred_at": self.first_occurred_at,
                "last_occurred_at": self.last_occurred_at,
                "latest_overturn_at": self.latest_overturn_at,
                "n_min": N_MIN_USABLE_ROWS,
            },
            "metric_vector": self.metrics.to_json(),
            "quality_only_metric_vector": self.quality_metrics.to_json(),
            "uncertainty": {
                "safety_band": finite_or_null(self.metrics.tier_safety().band()),
                "correctness_band": finite_or_null(self.metrics.tier_correctness().band()),
                "process_band": finite_or_null(self.metrics.tier_process().band()),
            },
            "excluded_counts": self.excluded_counts,
        })
    }
}

fn finite_or_null(value: f64) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}

/// The lexicographic comparator. `Ordering::Less` = ranks first.
pub(crate) fn compare_candidates(a: &CandidateSummary, b: &CandidateSummary) -> Ordering {
    // Unqualified candidates always rank after qualified ones — an empty
    // record must never outrank a judged one just because it has no `fail`.
    b.qualified()
        .cmp(&a.qualified())
        .then_with(|| {
            let left = a.metrics.tiers();
            let right = b.metrics.tiers();
            for (index, (_, left_tier)) in left.iter().enumerate() {
                let ordering = compare_tier(left_tier, &right[index].1);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            Ordering::Equal
        })
        // Reached ONLY when every judged tier is exactly equal: cost and
        // latency are tie-breakers within a tier, never cross-tier currency.
        .then_with(|| compare_lower_is_better(a.metrics.avg_cost_usd, b.metrics.avg_cost_usd))
        .then_with(|| compare_lower_is_better(a.metrics.avg_cost_tokens, b.metrics.avg_cost_tokens))
        .then_with(|| compare_lower_is_better(a.metrics.latency.value(), b.metrics.latency.value()))
        // Final total order so the same input always yields the same output.
        .then_with(|| a.profile.cmp(&b.profile))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProjectionDecision {
    Recommend {
        profile: String,
        reasons: Vec<String>,
    },
    Abstain {
        reason: &'static str,
        detail: String,
    },
}

impl ProjectionDecision {
    pub(crate) fn to_json(&self) -> Value {
        match self {
            ProjectionDecision::Recommend { profile, reasons } => json!({
                "kind": "recommended",
                "profile": profile,
                "reasons": reasons,
            }),
            ProjectionDecision::Abstain { reason, detail } => json!({
                "kind": "abstain",
                "profile": Value::Null,
                "reason": reason,
                "detail": detail,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExcludedRow {
    pub(crate) spine: &'static str,
    pub(crate) subject_id: String,
    pub(crate) profile: Option<String>,
    pub(crate) reason: &'static str,
    /// Whether this row still feeds the candidate's QUALITY vector. Never a
    /// statement about retention in the ledger: EVERY row here — including
    /// `user_forced`/`experiment` — stays in its source table, queryable and
    /// counted. This flag says only whether the projection reads it as
    /// quality evidence.
    pub(crate) contributes_quality_evidence: bool,
}

impl ExcludedRow {
    fn to_json(&self) -> Value {
        json!({
            "spine": self.spine,
            "subject_id": self.subject_id,
            "profile": self.profile,
            "reason": self.reason,
            "contributes_quality_evidence": self.contributes_quality_evidence,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionOutcome {
    pub(crate) candidates: Vec<CandidateSummary>,
    pub(crate) excluded_counts: BTreeMap<String, usize>,
    pub(crate) explained_exclusions: Vec<ExcludedRow>,
    pub(crate) decision: ProjectionDecision,
    pub(crate) rows_considered: usize,
    pub(crate) usable_rows: usize,
    pub(crate) quality_only_rows: usize,
}

impl ProjectionOutcome {
    pub(crate) fn explained_exclusions_json(&self) -> Value {
        Value::Array(
            self.explained_exclusions
                .iter()
                .map(ExcludedRow::to_json)
                .collect(),
        )
    }
}

fn bump(counts: &mut BTreeMap<String, usize>, reason: &str) {
    *counts.entry(reason.to_string()).or_insert(0) += 1;
}

/// The whole projection, as a pure function.
pub(crate) fn project(
    rows: &[ProjectionRow],
    eligible_profiles: &[String],
    since: &str,
    until: Option<&str>,
    now: &str,
    task_type: Option<&str>,
) -> ProjectionOutcome {
    // Every eligible candidate appears in the output, including the ones with
    // zero evidence — "no evidence" is an answer, not an omission.
    let mut summaries: BTreeMap<String, CandidateSummary> = eligible_profiles
        .iter()
        .map(|profile| (profile.clone(), CandidateSummary::new(profile)))
        .collect();
    let mut usable_by_profile: BTreeMap<String, Vec<&EvalObservation>> = BTreeMap::new();
    let mut quality_by_profile: BTreeMap<String, Vec<&EvalObservation>> = BTreeMap::new();
    let mut excluded_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut explained_exclusions = Vec::new();
    let mut usable_rows = 0usize;
    let mut quality_only_rows = 0usize;

    for row in rows {
        let disposition = classify_row(row, eligible_profiles, since, until, task_type);
        let observation = &row.observation;
        let profile = observation.profile.clone();
        match disposition {
            // `classify_row` only returns Usable/QualityOnly for a row that
            // carries a profile, so the `else` arms below are unreachable —
            // written as a fallthrough rather than an `expect` because a
            // projection must never panic a live tool call.
            RowDisposition::Usable => {
                let Some(profile) = profile else {
                    bump(&mut excluded_counts, reason::UNATTRIBUTED_PROFILE);
                    continue;
                };
                usable_rows += 1;
                usable_by_profile
                    .entry(profile)
                    .or_default()
                    .push(observation);
            }
            RowDisposition::QualityOnly(reason) => {
                let Some(profile) = profile else {
                    bump(&mut excluded_counts, self::reason::UNATTRIBUTED_PROFILE);
                    continue;
                };
                quality_only_rows += 1;
                bump(&mut excluded_counts, reason);
                if let Some(summary) = summaries.get_mut(&profile) {
                    bump(&mut summary.excluded_counts, reason);
                }
                if explained_exclusions.len() < MAX_EXPLAINED_EXCLUSIONS {
                    explained_exclusions.push(ExcludedRow {
                        spine: observation.spine.as_str(),
                        subject_id: observation.subject_id.clone(),
                        profile: Some(profile.clone()),
                        reason,
                        contributes_quality_evidence: true,
                    });
                }
                quality_by_profile
                    .entry(profile)
                    .or_default()
                    .push(observation);
            }
            RowDisposition::Excluded(reason) => {
                bump(&mut excluded_counts, reason);
                if let Some(summary) = profile
                    .as_ref()
                    .and_then(|profile| summaries.get_mut(profile))
                {
                    bump(&mut summary.excluded_counts, reason);
                }
                if explained_exclusions.len() < MAX_EXPLAINED_EXCLUSIONS {
                    explained_exclusions.push(ExcludedRow {
                        spine: observation.spine.as_str(),
                        subject_id: observation.subject_id.clone(),
                        profile,
                        reason,
                        contributes_quality_evidence: false,
                    });
                }
            }
        }
    }

    for (profile, observations) in &usable_by_profile {
        let Some(summary) = summaries.get_mut(profile) else {
            continue;
        };
        summary.usable_rows = observations.len();
        summary.metrics = build_metric_vector(observations.iter().copied());
        for observation in observations {
            let occurred = normalized(&observation.occurred_at);
            summary.first_occurred_at = Some(match summary.first_occurred_at.take() {
                Some(existing) if existing <= occurred => existing,
                _ => occurred.clone(),
            });
            summary.last_occurred_at = Some(match summary.last_occurred_at.take() {
                Some(existing) if existing >= occurred => existing,
                _ => occurred.clone(),
            });
            if let Some(adjudication) = observation
                .adjudication
                .as_ref()
                .filter(|adjudication| adjudication.is_overturn())
            {
                let at = normalized(&adjudication.created_at);
                summary.latest_overturn_at = Some(match summary.latest_overturn_at.take() {
                    Some(existing) if existing >= at => existing,
                    _ => at,
                });
            }
        }
    }
    for (profile, observations) in &quality_by_profile {
        let Some(summary) = summaries.get_mut(profile) else {
            continue;
        };
        summary.quality_only_rows = observations.len();
        summary.quality_metrics = build_metric_vector(observations.iter().copied());
    }

    let mut candidates = summaries.into_values().collect::<Vec<_>>();
    candidates.sort_by(compare_candidates);
    let decision = decide(&candidates, now);

    ProjectionOutcome {
        candidates,
        excluded_counts,
        explained_exclusions,
        decision,
        rows_considered: rows.len(),
        usable_rows,
        quality_only_rows,
    }
}

/// Recommend-or-abstain over already-sorted candidates.
pub(crate) fn decide(candidates: &[CandidateSummary], now: &str) -> ProjectionDecision {
    let qualified = candidates
        .iter()
        .filter(|candidate| candidate.qualified())
        .collect::<Vec<_>>();

    if qualified.is_empty() {
        return ProjectionDecision::Abstain {
            reason: abstain::INSUFFICIENT_EVIDENCE,
            detail: format!(
                "no eligible candidate has {N_MIN_USABLE_ROWS} usable rows in window \
                 (best: {})",
                candidates
                    .iter()
                    .map(|candidate| format!("{}={}", candidate.profile, candidate.usable_rows))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }

    // An overturn that has not settled means the evidence base itself is
    // still moving; ranking on it would publish a verdict that just changed.
    let settling_floor = shift_iso(now, -OVERTURN_SETTLING_SECS);
    if let Some(floor) = settling_floor.as_deref() {
        if let Some(unsettled) = qualified.iter().find(|candidate| {
            candidate
                .latest_overturn_at
                .as_deref()
                .map(|at| at > floor)
                .unwrap_or(false)
        }) {
            return ProjectionDecision::Abstain {
                reason: abstain::RECENT_OVERTURN_UNSETTLED,
                detail: format!(
                    "{} carries an overturn at {}, inside the {OVERTURN_SETTLING_SECS}s settling window",
                    unsettled.profile,
                    unsettled.latest_overturn_at.as_deref().unwrap_or("")
                ),
            };
        }
    }

    let leader = qualified[0];
    let Some(runner_up) = qualified.get(1) else {
        return ProjectionDecision::Recommend {
            profile: leader.profile.clone(),
            reasons: vec![format!(
                "sole qualified candidate with {} usable rows in window",
                leader.usable_rows
            )],
        };
    };

    let leader_tiers = leader.metrics.tiers();
    let runner_tiers = runner_up.metrics.tiers();
    for (index, (tier_name, leader_tier)) in leader_tiers.iter().enumerate() {
        let runner_tier = &runner_tiers[index].1;
        if compare_tier(leader_tier, runner_tier) == Ordering::Equal {
            continue;
        }
        // A fail-vs-no-fail difference is categorical, not a noisy estimate:
        // no band applies and no cost can buy it back.
        if leader_tier.has_fail != runner_tier.has_fail {
            return ProjectionDecision::Recommend {
                profile: leader.profile.clone(),
                reasons: vec![format!(
                    "{tier_name} tier is decisive: {} has no failing judgment, {} does",
                    leader.profile, runner_up.profile
                )],
            };
        }
        let delta = match (leader_tier.score, runner_tier.score) {
            (Some(left), Some(right)) => (left - right).abs(),
            // One side has no assessed judgment in this tier at all — an
            // unassessed tier is not a measured difference.
            _ => {
                return ProjectionDecision::Recommend {
                    profile: leader.profile.clone(),
                    reasons: vec![format!(
                        "{tier_name} tier is decisive: {} is judged there, {} is not",
                        leader.profile, runner_up.profile
                    )],
                }
            }
        };
        let band = leader_tier.band() + runner_tier.band();
        if delta <= band {
            return ProjectionDecision::Abstain {
                reason: abstain::TOP_TWO_UNCERTAINTY_OVERLAP,
                detail: format!(
                    "{tier_name} tier separates {} and {} by {delta:.3}, inside the \
                     combined uncertainty band {band:.3}",
                    leader.profile, runner_up.profile
                ),
            };
        }
        return ProjectionDecision::Recommend {
            profile: leader.profile.clone(),
            reasons: vec![format!(
                "{tier_name} tier separates {} from {} by {delta:.3} (> band {band:.3})",
                leader.profile, runner_up.profile
            )],
        };
    }

    // Every judged tier is exactly equal. Cost/latency are exact machine
    // facts, so they may break the tie — but only here, after the judged
    // tiers have already tied.
    let cost = compare_lower_is_better(leader.metrics.avg_cost_usd, runner_up.metrics.avg_cost_usd)
        .then_with(|| {
            compare_lower_is_better(
                leader.metrics.avg_cost_tokens,
                runner_up.metrics.avg_cost_tokens,
            )
        })
        .then_with(|| {
            compare_lower_is_better(
                leader.metrics.latency.value(),
                runner_up.metrics.latency.value(),
            )
        });
    if cost == Ordering::Less {
        return ProjectionDecision::Recommend {
            profile: leader.profile.clone(),
            reasons: vec![format!(
                "judged tiers tie with {}; broken by lower cost/latency",
                runner_up.profile
            )],
        };
    }

    ProjectionDecision::Abstain {
        reason: abstain::NO_DISCRIMINATING_EVIDENCE,
        detail: format!(
            "{} and {} tie on every judged tier and on cost/latency",
            leader.profile, runner_up.profile
        ),
    }
}

#[cfg(test)]
mod tests;
