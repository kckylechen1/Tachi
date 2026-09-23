//! `tachi_agent_eval(action='candidate_projection')` — the read-only,
//! advisory-only per-candidate projection of VERIFIED mirror-eval
//! experience (the native dispatch-experience surface).
//!
//! The host harness owns the model roster and the spawn decision. This
//! projection answers ONE question for a bounded, caller-supplied candidate
//! set: what terminal, adjudicated, evidence-usable, non-self-eval mirror
//! runs exist for this exact observed identity — including the stored
//! `next_prompt_delta` advisories those verdicts carried — and nothing else.
//!
//! Frozen invariants (see the dispatch packet for this lane):
//! - Identity is matched ONLY against carrier-OBSERVED facts
//!   (`effective_model`, `effective_harness`, `effective_role` since v34).
//!   Register-time requested identity is displayed, never matched — a
//!   missing effective identity is never inferred from the requested one.
//! - Model identity is the release part of the observed effective model
//!   string (the existing `provider_model_parts` convention). Provider,
//!   family, and profile are different concepts and are never matched
//!   fuzzily, aliased, or hardcoded.
//! - A model revision is confirmed by the explicit observed
//!   `effective_model_revision` column (v34), or — where that column is
//!   NULL — by the legacy `@version` suffix on the observed effective
//!   model. A run observed with neither stays `unresolved` (historical),
//!   never version-confirmed because an API-visible name matches. NEW
//!   writes refuse conflicting revision representations; a candidate's
//!   model-string `@version` suffix IS its declared revision when the
//!   separate field is omitted, and disagreeing representations are a
//!   validation error.
//! - Role confirmation requires the carrier-observed `effective_role`
//!   (v34); there is no requested_role/requested_profile fallback. Task
//!   scoping keys on the register-time `requested_task_type` under an
//!   explicit `register_requested` basis — never presented as observed.
//!   Unknown legacy role/task/revision stays visible historical evidence
//!   and never inflates confirmed-compatible samples.
//! - Eligibility reuses the #1066/#1035 semantics already enforced
//!   elsewhere: terminal observation present, current (last-appended)
//!   adjudication `evidence_usable`, and not a same-lineage self-eval.
//!   Rejected/failed outcomes stay rejected/failed; terminal alone is not
//!   success. At most one sample per run exists (the mirror spine's single
//!   observation + current adjudication).
//! - Read-only: this handler writes no route recommendations/decisions, no
//!   policy, no cards, no mirror rows; it promotes nothing. It is advisory
//!   evidence for the host's own choice, never launch authority.
//! - Statuses are split: `adjudication_status` reports
//!   whether eligible terminal/adjudicated/usable/non-self-eval rows exist;
//!   `compatibility_status` reports how far those rows CONFIRM the
//!   candidate identity. A revision-unresolved (historical) row never
//!   yields an unqualified "verified" — it is counted unresolved, and a
//!   sample is confirmed-compatible only when model, harness, role, task,
//!   and revision are ALL explicitly confirmed. Query dimensions left
//!   unscoped are labeled `unscoped`, never confirmed.
//! - The time contract is exact and two-layered: the LOWER bound (`since`)
//!   applies only to the run cohort (a run's `created_at` must be an
//!   actual instant in `[since, generated_at)`, RFC3339-authoritative —
//!   the store's julianday preselect is instant-aware but not
//!   RFC3339-strict, so this module's validation decides admission); the
//!   UPPER bound (`generated_at`) applies to every associated fact — the
//!   observation, the current adjudication, superseded advisories, and
//!   rubric provenance are strictly before it and MAY legitimately predate
//!   `since`. A future current adjudication excludes the whole sample (no
//!   fallback to an older usable event); an unusable (blank or malformed)
//!   timestamp fails its bound rather than passing it.
//! - Advisory texts are kept WHOLE or dropped WHOLE with an omission flag;
//!   nothing is sliced to fit a budget. No raw transcripts, credentials, or
//!   task corpus are returned — stored fields were scrubbed at write time.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::server_state::MemoryServer;
use crate::tool_params::{CandidateProjectionCandidate, CandidateProjectionParams};

use super::mirror::{is_self_eval, lineage_of, producer_lineage_for_gating};
use super::projection::rules;

/// Bounded candidate set: one host shortlist, inspectable in one response.
pub(crate) const MAX_CANDIDATES: usize = 6;
/// Newest eligible samples returned per candidate. Older eligible rows stay
/// counted (`samples_total`) and flagged (`samples_truncated`), never lost
/// silently.
const MAX_SAMPLES_PER_CANDIDATE: usize = 5;
/// Per-advisory size bound: a `next_prompt_delta` is kept whole or dropped
/// whole, never sliced.
const MAX_ADVISORY_CHARS: usize = 4_000;
/// Whole-response advisory budget; once exhausted, further deltas drop whole
/// with an omission flag so total output stays bounded.
const MAX_TOTAL_ADVISORY_CHARS: usize = 24_000;
/// Artifact refs per sample (refs only — never transcript content).
const MAX_ARTIFACT_REFS: usize = 10;
/// Superseded (historical) advisories surfaced per sample.
const MAX_HISTORICAL_ADVISORIES: usize = 3;

/// The fixed exclusion vocabulary. Every response reports a count for each
/// reason (zero included) so an absence is visible, not silent.
const EXCLUSION_REASONS: &[&str] = &[
    "run_timestamp_malformed",
    "run_out_of_window",
    "not_observed",
    "observation_future",
    "unadjudicated",
    "current_adjudication_future",
    "evidence_not_usable",
    "self_eval",
    "identity_unobserved",
    "model_mismatch",
    "harness_mismatch",
    "harness_effective_unobserved",
    "revision_mismatch",
    "role_unobserved",
    "role_mismatch",
    "task_type_unrecorded",
    "task_type_mismatch",
];

/// Per-sample model-revision resolution states.
const REVISION_STATES: &[&str] = &["confirmed", "unresolved", "observed", "unrecorded"];

fn scrub(text: String) -> String {
    crate::memory_search_ops::scrub_secrets(&text).0
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// The projection instant. Production reads the real clock; tests can pin a
/// fixed cutoff so register→adjudicate→project flows and future-fact
/// fixtures are deterministic without sleeps (see `test_hooks`).
fn projection_now() -> String {
    #[cfg(test)]
    if let Some(fixed) = test_hooks::overridden_cutoff() {
        return fixed;
    }
    memcore::now_utc_iso()
}

/// Whether a stored timestamp is provably BEFORE the cutoff. Blank or
/// malformed values are NOT in-window: a fact whose time cannot be
/// established is never eligible evidence (it fails the bound rather than
/// silently passing it — a blank string would otherwise compare as older
/// than any real timestamp).
fn strictly_before_cutoff(timestamp: &str, cutoff: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(timestamp.trim()),
        chrono::DateTime::parse_from_rfc3339(cutoff.trim()),
    ) {
        (Ok(timestamp), Ok(cutoff)) => timestamp < cutoff,
        _ => false,
    }
}

/// The run-cohort check: the projection's authoritative admission of a
/// run's `created_at` against BOTH window ends as actual instants. The
/// store's windowed query preselects with SQLite `julianday` (instant-
/// aware, but it ACCEPTS strings RFC3339 rejects, such as date-only
/// forms), so this re-validation is what actually decides inclusion —
/// offset-bearing timestamps land on their real instant, a string that
/// cannot be established as RFC3339 is malformed (fail-closed, including
/// an unparseable bound), and a parseable instant outside
/// `[since, cutoff)` is out of window (lower-inclusive, upper-exclusive).
enum RunCohortDisposition {
    InCohort,
    Malformed,
    OutOfWindow,
}

fn run_cohort_check(created_at: &str, since: &str, cutoff: &str) -> RunCohortDisposition {
    match (
        chrono::DateTime::parse_from_rfc3339(created_at.trim()),
        chrono::DateTime::parse_from_rfc3339(since.trim()),
        chrono::DateTime::parse_from_rfc3339(cutoff.trim()),
    ) {
        (Ok(run), Ok(since), Ok(cutoff)) if run >= since && run < cutoff => {
            RunCohortDisposition::InCohort
        }
        (Ok(_), Ok(_), Ok(_)) => RunCohortDisposition::OutOfWindow,
        _ => RunCohortDisposition::Malformed,
    }
}

#[cfg(test)]
mod timestamp_tests {
    use super::strictly_before_cutoff;

    #[test]
    fn cutoff_compares_instants_and_rejects_invalid_timestamps() {
        let cutoff = "2027-01-01T00:00:00.000Z";
        for invalid in ["", "!", "2026-not-a-date", "2026-02-30T00:00:00Z"] {
            assert!(!strictly_before_cutoff(invalid, cutoff));
            assert!(!strictly_before_cutoff(cutoff, invalid));
        }
        assert!(!strictly_before_cutoff(cutoff, cutoff));
        assert!(!strictly_before_cutoff("2026-12-31T23:30:00-01:00", cutoff));
        assert!(!strictly_before_cutoff("2027-01-01T01:00:00+01:00", cutoff));
        assert!(strictly_before_cutoff("2027-01-01T00:30:00+01:00", cutoff));
        assert!(strictly_before_cutoff("2026-12-31T23:59:59.999Z", cutoff));
    }

    /// The run-cohort admission decides on parsed RFC3339 instants against
    /// BOTH window ends: lower-inclusive, upper-exclusive, offset-aware,
    /// and fail-closed for anything chrono cannot establish (including the
    /// date-only forms SQLite's julianday preselect accepts) and for an
    /// unparseable bound.
    #[test]
    fn run_cohort_check_admits_on_parsed_instants_both_ends() {
        use super::run_cohort_check;
        use super::RunCohortDisposition::{InCohort, Malformed, OutOfWindow};
        let since = "2026-01-01T00:00:00.000Z";
        let cutoff = "2027-01-01T00:00:00.000Z";
        let admits = |created_at: &str| match run_cohort_check(created_at, since, cutoff) {
            InCohort => "in_cohort",
            Malformed => "malformed",
            OutOfWindow => "out_of_window",
        };
        // Lower-inclusive at the exact instant, including an offset form a
        // lexical filter would omit ("2025-12-31..." sorts before `since`).
        assert_eq!(admits("2026-01-01T00:00:00.000Z"), "in_cohort");
        assert_eq!(admits("2025-12-31T19:00:00-05:00"), "in_cohort");
        // Upper-exclusive at the exact instant, in both notations — a
        // lexically-past string whose actual instant is future lands out of
        // window.
        assert_eq!(admits("2027-01-01T00:00:00.000Z"), "out_of_window");
        assert_eq!(admits("2026-12-31T23:30:00-01:00"), "out_of_window");
        assert_eq!(admits("2026-12-31T19:00:01-05:00"), "out_of_window");
        assert_eq!(admits("2026-12-31T18:59:59-05:00"), "in_cohort");
        // Below the lower bound.
        assert_eq!(admits("2025-12-31T18:59:59-05:00"), "out_of_window");
        // Malformed / non-RFC3339 (date-only is exactly the form the store's
        // julianday preselect accepts), and an unusable bound fails closed.
        for malformed in ["", "not-a-timestamp", "2026-06-01", "2026-02-30T00:00:00Z"] {
            assert_eq!(admits(malformed), "malformed", "{malformed:?}");
        }
        assert_eq!(
            match run_cohort_check("2026-06-01T00:00:00Z", "garbage-since", cutoff) {
                InCohort => "in_cohort",
                Malformed => "malformed",
                OutOfWindow => "out_of_window",
            },
            "malformed"
        );
    }
}

/// Test-only fixed-cutoff fixture (the memcore `test_hooks` precedent): pin
/// the projection instant so same-millisecond write→read flows and
/// future-fact regressions are deterministic. Restores the real clock on
/// drop; parallel tests without the guard are unaffected.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;

    thread_local! {
        static CUTOFF_OVERRIDE: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(crate) struct CutoffGuard;

    impl CutoffGuard {
        pub(crate) fn set(cutoff: &str) -> Self {
            CUTOFF_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(cutoff.to_string()));
            CutoffGuard
        }
    }

    impl Drop for CutoffGuard {
        fn drop(&mut self) {
            CUTOFF_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        }
    }

    pub(crate) fn overridden_cutoff() -> Option<String> {
        CUTOFF_OVERRIDE.with(|cell| cell.borrow().clone())
    }
}

/// A validated, scrubbed, pre-parsed candidate.
struct PreparedCandidate {
    candidate_id: String,
    /// Full caller-supplied model string (echoed verbatim).
    model: String,
    /// Release part the observed effective model must equal exactly.
    model_release: String,
    role: Option<String>,
    harness: String,
    /// The DECLARED revision this candidate's evidence must respect: the
    /// explicit `model_revision` field, or — when that field is omitted —
    /// the `@version` suffix on the model string itself (a caller who pins
    /// the revision in the model name has declared it; ignoring the suffix
    /// here is exactly the silent-discard bug the parent review flagged).
    model_revision: Option<String>,
}

fn prepare_candidates(
    raw: &[CandidateProjectionCandidate],
) -> Result<Vec<PreparedCandidate>, String> {
    if raw.is_empty() {
        return Err("candidate_projection requires 1..=6 candidates; none supplied".to_string());
    }
    if raw.len() > MAX_CANDIDATES {
        return Err(format!(
            "candidate_projection accepts at most {MAX_CANDIDATES} candidates; {} supplied",
            raw.len()
        ));
    }
    let mut seen_ids: Vec<String> = Vec::with_capacity(raw.len());
    let mut prepared = Vec::with_capacity(raw.len());
    for (index, candidate) in raw.iter().enumerate() {
        let candidate_id = scrub(candidate.candidate_id.trim().to_string());
        if candidate_id.is_empty() {
            return Err(format!(
                "candidates[{index}]: candidate_id is required and must be non-empty"
            ));
        }
        if seen_ids.contains(&candidate_id) {
            return Err(format!(
                "candidates[{index}]: duplicate candidate_id '{candidate_id}'"
            ));
        }
        seen_ids.push(candidate_id.clone());

        let model = scrub(candidate.model.trim().to_string());
        if model.is_empty() {
            return Err(format!(
                "candidates[{index}] ('{candidate_id}'): model identity is required — a model, \
                 not a profile and not a provider/family label"
            ));
        }
        let harness = scrub(candidate.harness.trim().to_string());
        if harness.is_empty() {
            return Err(format!(
                "candidates[{index}] ('{candidate_id}'): harness is required"
            ));
        }
        let role = non_empty(candidate.role.as_deref()).map(|role| scrub(role.to_string()));
        let (model_release, _, suffix_revision) =
            tachi_dispatch::provider_model_parts(Some(model.as_str()));
        let explicit_revision = non_empty(candidate.model_revision.as_deref())
            .map(|revision| scrub(revision.to_string()));
        // The declared revision is the explicit field, else the model
        // string's own `@version` suffix. Supplying BOTH with disagreeing
        // values is a validation error — silently preferring one would
        // discard the caller's other declaration.
        let model_revision = match (explicit_revision, suffix_revision.as_str()) {
            (Some(explicit), suffix)
                if suffix != tachi_dispatch::UNKNOWN_IDENTITY && suffix != explicit =>
            {
                return Err(format!(
                    "candidates[{index}] ('{candidate_id}'): model_revision '{explicit}' \
                     conflicts with the '@{suffix}' suffix on model '{model}'; supply one \
                     revision, or make them agree"
                ))
            }
            (Some(explicit), _) => Some(explicit),
            (None, suffix) if suffix != tachi_dispatch::UNKNOWN_IDENTITY => {
                Some(suffix.to_string())
            }
            (None, _) => None,
        };
        prepared.push(PreparedCandidate {
            candidate_id,
            model,
            model_release,
            role,
            harness,
            model_revision,
        });
    }
    Ok(prepared)
}

enum Disposition {
    /// Eligible evidence, with the resolved revision state and the
    /// per-sample compatibility verdict.
    Eligible {
        revision_status: RevisionStatus,
        compatibility: SampleCompatibility,
    },
    /// Excluded under exactly one reason from [`EXCLUSION_REASONS`].
    Excluded(&'static str),
}

/// Per-sample model-revision resolution state, with the representation the
/// resolution came from when one exists.
enum RevisionStatus {
    Confirmed { basis: &'static str },
    Unresolved { basis: Option<&'static str> },
    Observed { basis: &'static str },
    Unrecorded,
}

impl RevisionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            RevisionStatus::Confirmed { .. } => "confirmed",
            RevisionStatus::Unresolved { .. } => "unresolved",
            RevisionStatus::Observed { .. } => "observed",
            RevisionStatus::Unrecorded => "unrecorded",
        }
    }

    fn basis(&self) -> Option<&'static str> {
        match self {
            RevisionStatus::Confirmed { basis }
            | RevisionStatus::Unresolved { basis: Some(basis) }
            | RevisionStatus::Observed { basis } => Some(basis),
            RevisionStatus::Unresolved { basis: None } | RevisionStatus::Unrecorded => None,
        }
    }
}

/// Resolved observed-revision facts for one run: the revision value and
/// where it came from. The explicit v34 column is authoritative; a legacy
/// `@version` suffix on the observed model string is a fallback ONLY when
/// the column is absent (new writes refuse conflicting representations, so
/// the read side can never see both disagree). Neither present ⇒
/// `(None, None)` — unresolved, never guessed.
fn observed_revision_of(
    observation: &memcore::MirrorEvalObservation,
    effective_model: &str,
) -> (Option<String>, Option<&'static str>) {
    if let Some(explicit) = non_empty(observation.effective_model_revision.as_deref()) {
        return (Some(explicit.to_string()), Some("observed_column"));
    }
    let (_, _, suffix) = tachi_dispatch::provider_model_parts(Some(effective_model));
    if suffix != tachi_dispatch::UNKNOWN_IDENTITY {
        return (Some(suffix), Some("legacy_model_suffix"));
    }
    (None, None)
}

/// Classify one run against one candidate. Evaluated in order; the FIRST
/// failing reason is the one counted, so each run is counted at most once
/// per candidate. `query_task_type` is the caller's query-level task scope,
/// matched against the run's register-time `requested_task_type`.
/// `since`/`cutoff` bracket the run cohort as actual instants (RFC3339
/// authoritative); `cutoff` additionally bounds every associated fact —
/// the observation, the current adjudication, the superseded advisories,
/// and rubric provenance — strictly before the projection instant (they
/// may legitimately predate `since`; only the RUN must fall in the cohort
/// window). The returned compatibility carries which scoping dimensions
/// were actually CONFIRMED versus merely unscoped by the query.
fn classify_for_candidate(
    view: &memcore::MirrorEvalRunView,
    candidate: &PreparedCandidate,
    query_task_type: Option<&str>,
    since: &str,
    cutoff: &str,
) -> Disposition {
    // The run's cohort admission is authoritative here, on the parsed
    // instant: the store's julianday preselect is instant-aware but not
    // RFC3339-strict, so a date-only form (or any string chrono rejects)
    // fails closed, and an offset-bearing timestamp that merely LOOKS
    // lexically in-window is judged by its actual instant on both ends.
    match run_cohort_check(&view.run.created_at, since, cutoff) {
        RunCohortDisposition::InCohort => {}
        RunCohortDisposition::Malformed => {
            return Disposition::Excluded("run_timestamp_malformed");
        }
        RunCohortDisposition::OutOfWindow => {
            return Disposition::Excluded("run_out_of_window");
        }
    }
    let Some(observation) = view.observation.as_ref() else {
        return Disposition::Excluded("not_observed");
    };
    // terminal_outcome is required non-empty at write time; a blank one is
    // defended against anyway — it is "no terminal fact", not a success.
    if observation.terminal_outcome.trim().is_empty() {
        return Disposition::Excluded("not_observed");
    }
    // An observation stamped at/after the projection instant (or
    // carrying an unusable timestamp) is a future fact and never evidence.
    if !strictly_before_cutoff(&observation.created_at, cutoff) {
        return Disposition::Excluded("observation_future");
    }
    let Some(adjudication) = view.current_adjudication() else {
        return Disposition::Excluded("unadjudicated");
    };
    // The CURRENT (last-appended) judgment must itself be before the
    // cutoff. There is deliberately NO fallback to an older usable event:
    // silently promoting a superseded judgment when the latest is future
    // would fabricate the current state of an unfinished adjudication.
    if !strictly_before_cutoff(&adjudication.created_at, cutoff) {
        return Disposition::Excluded("current_adjudication_future");
    }
    if !adjudication.evidence_usable {
        return Disposition::Excluded("evidence_not_usable");
    }
    let producer_lineage = producer_lineage_for_gating(view.observation.as_ref());
    let verifier_lineage = lineage_of(adjudication.verifier_model.as_deref());
    if is_self_eval(&producer_lineage, &verifier_lineage) {
        return Disposition::Excluded("self_eval");
    }
    // Identity anchoring: OBSERVED effective model only. A run with no
    // observed model (register-only, or an observe that omitted it) has an
    // unattributable identity — never inferred from requested_model.
    let Some(effective_model) = non_empty(observation.effective_model.as_deref()) else {
        return Disposition::Excluded("identity_unobserved");
    };
    let (observed_release, _, _) = tachi_dispatch::provider_model_parts(Some(effective_model));
    if observed_release != candidate.model_release {
        return Disposition::Excluded("model_mismatch");
    }
    match non_empty(observation.effective_harness.as_deref()) {
        Some(harness) if harness == candidate.harness => {}
        Some(_) => return Disposition::Excluded("harness_mismatch"),
        // Register-time harness is PLANNED, not observed: it can never
        // confirm a candidate-harness match.
        None => return Disposition::Excluded("harness_effective_unobserved"),
    }
    // Role confirmation requires the carrier-OBSERVED role. No
    // requested_role/requested_profile fallback exists: an unobserved role
    // excludes the row (visible, historical) rather than counting it
    // compatible, and an explicitly different observed role is a mismatch.
    let role_compatibility = match (
        candidate.role.as_deref(),
        non_empty(observation.effective_role.as_deref()),
    ) {
        (Some(wanted), Some(observed)) if observed != wanted => {
            return Disposition::Excluded("role_mismatch");
        }
        (Some(_), Some(_)) => DimensionCompatibility::Confirmed,
        (Some(_), None) => return Disposition::Excluded("role_unobserved"),
        (None, _) => DimensionCompatibility::Unscoped,
    };
    // Task scoping: the run's task anchor is the register-time
    // `requested_task_type` (frozen contract metadata, register_requested
    // basis — the mirror spine has no carrier-observed task fact and never
    // pretends one). An unrecorded task excludes the row for a task-scoped
    // query rather than counting it compatible.
    let task_compatibility = if let Some(wanted) = query_task_type {
        match non_empty(view.run.requested_task_type.as_deref()) {
            Some(recorded) if recorded != wanted => {
                return Disposition::Excluded("task_type_mismatch");
            }
            None => return Disposition::Excluded("task_type_unrecorded"),
            Some(_) => DimensionCompatibility::Confirmed,
        }
    } else {
        DimensionCompatibility::Unscoped
    };
    let (observed_revision, revision_basis) = observed_revision_of(observation, effective_model);
    let (revision_compatibility, revision_status) = match candidate.model_revision.as_deref() {
        Some(wanted) => match observed_revision.as_deref() {
            Some(observed) if observed == wanted => (
                DimensionCompatibility::Confirmed,
                RevisionStatus::Confirmed {
                    basis: revision_basis.expect("a present revision always carries a basis"),
                },
            ),
            Some(_) => return Disposition::Excluded("revision_mismatch"),
            // Candidate declares a revision but the run observed none (in
            // either representation): the row may inform, but only as
            // explicitly unresolved historical evidence — never counted as
            // confirmed-compatible.
            None => (
                DimensionCompatibility::Unresolved,
                RevisionStatus::Unresolved {
                    basis: revision_basis,
                },
            ),
        },
        None => match observed_revision.as_deref() {
            Some(_) => (
                // No declared revision: the dimension is unscoped, not
                // confirmed (an observed revision is surfaced but does not
                // confirm a declaration that was never made).
                DimensionCompatibility::Unscoped,
                RevisionStatus::Observed {
                    basis: revision_basis.expect("a present revision always carries a basis"),
                },
            ),
            None => (DimensionCompatibility::Unscoped, RevisionStatus::Unrecorded),
        },
    };
    Disposition::Eligible {
        revision_status,
        compatibility: SampleCompatibility {
            role: role_compatibility,
            task_type: task_compatibility,
            model_revision: revision_compatibility,
        },
    }
}

/// One scoping dimension's compatibility for an eligible sample.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DimensionCompatibility {
    /// The query scoped this dimension and the observed/declared facts
    /// match.
    Confirmed,
    /// The query did not scope this dimension — visibly unscoped, which is
    /// NOT full confirmation.
    Unscoped,
    /// The query scoped this dimension and the run cannot confirm it
    /// (revision declared, none observed).
    Unresolved,
}

impl DimensionCompatibility {
    fn as_str(self) -> &'static str {
        match self {
            DimensionCompatibility::Confirmed => "confirmed",
            DimensionCompatibility::Unscoped => "unscoped",
            DimensionCompatibility::Unresolved => "unresolved",
        }
    }
}

/// The per-sample compatibility verdict across every scoping dimension.
/// Model and harness are always `confirmed` for an eligible sample (they
/// are hard gates); the overall verdict is `fully_confirmed` only when
/// role, task, AND revision are all `Confirmed` — any `Unresolved`
/// dimension makes it `unresolved`, otherwise any `Unscoped` dimension
/// makes it `unscoped` (adjudication quality and compatibility are
/// reported separately; nothing is an unqualified "verified").
struct SampleCompatibility {
    role: DimensionCompatibility,
    task_type: DimensionCompatibility,
    model_revision: DimensionCompatibility,
}

impl SampleCompatibility {
    fn overall(&self) -> &'static str {
        let dims = [self.role, self.task_type, self.model_revision];
        if dims.contains(&DimensionCompatibility::Unresolved) {
            "unresolved"
        } else if dims
            .iter()
            .all(|dim| *dim == DimensionCompatibility::Confirmed)
        {
            "fully_confirmed"
        } else {
            "unscoped"
        }
    }
}

/// Whole-or-dropped advisory budget shared across the whole response.
struct AdvisoryBudget {
    remaining: usize,
}

impl AdvisoryBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_TOTAL_ADVISORY_CHARS,
        }
    }

    /// Returns the value (whole text, or Null when dropped) and the omission
    /// reason (None when kept). Never slices.
    fn take(&mut self, text: &str) -> (Value, Option<&'static str>) {
        let chars = text.chars().count();
        if chars > MAX_ADVISORY_CHARS {
            return (Value::Null, Some("per_field_limit"));
        }
        if chars > self.remaining {
            return (Value::Null, Some("total_budget_exhausted"));
        }
        self.remaining -= chars;
        (json!(text), None)
    }
}

fn rubric_json(rubric: Option<&memcore::EvalRubricScoreRow>) -> Value {
    match rubric {
        Some(row) => json!({
            "present": true,
            "rubric_hash": row.rubric_hash,
            "dimensions": {
                "contract_correctness": row.contract_correctness,
                "evidence_quality": row.evidence_quality,
                "safety": row.safety,
                "scope_discipline": row.scope_discipline,
                "intervention_burden": row.intervention_burden,
                "completion_integrity": row.completion_integrity,
            },
            "adjudication_confidence": row.adjudication_confidence,
            "adjudicator_actor": row.adjudicator_actor,
            "adjudicator_vendor": row.adjudicator_vendor,
            "independence_basis": row.independence_basis,
            "occurred_at": row.occurred_at,
        }),
        None => json!({
            "present": false,
            "note": "no structured rubric recorded for the current adjudication event",
        }),
    }
}

/// One eligible run as a response sample. Whole-or-dropped advisory fields;
/// bounded artifact refs; superseded advisories preserved separately. Every
/// timestamped fact included here is bounded strictly before `cutoff` — a
/// superseded advisory or rubric stamped at/after the cutoff is dropped
/// with a visible count, never silently included.
fn sample_json(
    view: &memcore::MirrorEvalRunView,
    revision_status: &RevisionStatus,
    compatibility: &SampleCompatibility,
    rubric: Option<&memcore::EvalRubricScoreRow>,
    cutoff: &str,
    budget: &mut AdvisoryBudget,
) -> Value {
    let run = &view.run;
    let observation = view
        .observation
        .as_ref()
        .expect("eligibility requires an observation");
    let adjudication = view
        .current_adjudication()
        .expect("eligibility requires a current adjudication");

    let effective_model = observation.effective_model.clone().unwrap_or_default();
    let (observed_release, _, _) =
        tachi_dispatch::provider_model_parts(Some(effective_model.as_str()));
    let (observed_revision, revision_basis) = observed_revision_of(observation, &effective_model);
    let observed_revision_value = match observed_revision.as_deref() {
        Some(revision) => json!(revision),
        None => Value::Null,
    };

    let (next_prompt_delta, delta_omitted_reason) = match adjudication.next_prompt_delta.as_deref()
    {
        None => (Value::Null, None),
        Some(text) => budget.take(text),
    };

    // Superseded advisories: only events strictly before the cutoff are
    // preserved; future ones (a later-stamped correction mid-flight) are
    // counted and flagged, never shown as history. The CURRENT
    // event's own cutoff eligibility was already enforced upstream — it is
    // the last-appended one, with no fallback to an older usable event.
    let bounded_superseded: Vec<&memcore::MirrorEvalAdjudication> = view
        .adjudications
        .iter()
        .rev()
        .skip(1)
        .filter(|event| strictly_before_cutoff(&event.created_at, cutoff))
        .collect();
    let future_superseded_dropped = (view.adjudications.len() - 1) - bounded_superseded.len();
    let qualifying: Vec<&memcore::MirrorEvalAdjudication> = bounded_superseded
        .iter()
        .copied()
        .filter(|event| {
            event
                .next_prompt_delta
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
        .collect();
    let historical_truncated = qualifying.len() > MAX_HISTORICAL_ADVISORIES;
    let historical_advisories: Vec<Value> = qualifying
        .iter()
        .take(MAX_HISTORICAL_ADVISORIES)
        .map(|event| {
            let (value, reason) = budget.take(event.next_prompt_delta.as_deref().unwrap_or(""));
            json!({
                "adjudication_id": event.adjudication_id,
                "event_key": event.event_key,
                "actor": event.actor,
                "usefulness": event.usefulness,
                "created_at": event.created_at,
                "insertion_seq": event.insertion_seq,
                "next_prompt_delta": value,
                "next_prompt_delta_omitted_reason": reason,
                "superseded": true,
            })
        })
        .collect();

    let artifacts_truncated = observation.artifacts.len() > MAX_ARTIFACT_REFS;
    let artifact_refs: Vec<&String> = observation
        .artifacts
        .iter()
        .take(MAX_ARTIFACT_REFS)
        .collect();

    let (verifier_release, _, verifier_revision) =
        tachi_dispatch::provider_model_parts(adjudication.verifier_model.as_deref());
    let verifier_revision_value = if verifier_revision == tachi_dispatch::UNKNOWN_IDENTITY {
        Value::Null
    } else {
        json!(verifier_revision)
    };

    // Rubric provenance is bounded to the cutoff too: a rubric stamped at or
    // after the projection instant (or without a usable occurred_at) is
    // omitted with the reason, not presented as established judgment.
    let rubric_in_window =
        rubric.is_some_and(|row| strictly_before_cutoff(&row.occurred_at, cutoff));

    json!({
        "eval_run_id": run.eval_run_id,
        "native_child_id": run.native_child_id,
        "frozen_contract_ref": run.frozen_contract_ref,
        "execution_origin": run.execution_origin,
        "lifecycle_owner": run.lifecycle_owner,
        "occurred_at": run.created_at,
        "occurred_at_basis": "legacy_created_at",
        "identity": {
            "model": effective_model,
            "model_release": observed_release,
            "model_revision": observed_revision_value,
            "revision_basis": revision_basis,
            "backend": observation.effective_backend,
            "harness": observation.effective_harness,
            "role": observation.effective_role,
            "task_type": run.requested_task_type,
            "requested_model": run.requested_model,
            "requested_profile": run.requested_profile,
            "requested_agent": run.requested_agent,
            "requested_role": run.requested_role,
        },
        "identity_basis": {
            "model": "observed",
            "harness": "observed",
            "role": if observation.effective_role.as_deref().is_some_and(|role| !role.trim().is_empty()) { "observed" } else { "unrecorded" },
            "task_type": if run.requested_task_type.as_deref().is_some_and(|task| !task.trim().is_empty()) { "register_requested" } else { "unrecorded" },
            "model_revision": revision_status.as_str(),
            "model_revision_basis": revision_status.basis(),
        },
        "note_requested_fields_are_display_only": "requested_model/requested_profile/\
         requested_role/requested_task_type are register-time intent, displayed for \
         context; they never confirm identity, role, or task, and requested_profile \
         is a profile, not a role",
        "outcome": {
            "terminal_outcome": observation.terminal_outcome,
            "usefulness": adjudication.usefulness,
            "failure_mode": adjudication.failure_mode,
            "used_in_final_claim": adjudication.used_in_final_claim,
            "human_override": adjudication.human_override,
        },
        "adjudication": {
            "adjudication_id": adjudication.adjudication_id,
            "actor": adjudication.actor,
            "verifier_model": adjudication.verifier_model,
            "verifier_model_release": verifier_release,
            "verifier_model_revision": verifier_revision_value,
            "verifier_lineage": lineage_of(adjudication.verifier_model.as_deref()),
            "created_at": adjudication.created_at,
            "insertion_seq": adjudication.insertion_seq,
            // Counted over cutoff-bounded events only; the current event is
            // always among them (a future current event excludes the whole
            // sample upstream, with no fallback to an older one).
            "event_count": bounded_superseded.len() + 1,
            "future_events_dropped": future_superseded_dropped,
            "is_overturn": bounded_superseded.len() + 1 > 1,
        },
        "observation_refs": {
            "observation_id": observation.observation_id,
            "observed_at": observation.created_at,
            "result_ref": observation.result_ref,
            "artifact_refs": artifact_refs,
            "artifact_refs_truncated": artifacts_truncated,
            "artifact_refs_total": observation.artifacts.len(),
            "duration_ms": observation.duration_ms,
            "cost_tokens": observation.cost_tokens,
            "cost_usd": observation.cost_usd,
        },
        "rubric": if rubric_in_window {
            rubric_json(rubric)
        } else {
            json!({
                "present": false,
                "note": if rubric.is_some() {
                    "rubric occurred_at is not before the projection cutoff"
                } else {
                    "no structured rubric recorded for the current adjudication event"
                },
            })
        },
        "compatibility": {
            "model": "confirmed",
            "harness": "confirmed",
            "role": compatibility.role.as_str(),
            "task_type": compatibility.task_type.as_str(),
            "model_revision": compatibility.model_revision.as_str(),
            "overall": compatibility.overall(),
        },
        "next_prompt_delta": next_prompt_delta,
        "next_prompt_delta_omitted_reason": delta_omitted_reason,
        "historical_advisories": historical_advisories,
        "historical_advisories_truncated": historical_truncated,
        "future_superseded_advisories_dropped": future_superseded_dropped,
    })
}

pub(crate) fn handle_candidate_projection(
    server: &MemoryServer,
    params: CandidateProjectionParams,
    limit: Option<usize>,
) -> Result<String, String> {
    let prepared = prepare_candidates(&params.candidates)?;
    // Same facade-wide bound the other `tachi_agent_eval` read actions use.
    let row_limit = super::capped_eval_limit(limit);
    // The cutoff is EXACTLY the projection instant, public as the upper
    // end of [since, generated_at) with no granularity tolerance. The lower
    // end applies only to the RUN cohort; the associated facts (observation,
    // current adjudication, superseded advisories, rubric provenance) are
    // bounded only from above, strictly before the cutoff, and may
    // legitimately predate `since`. A fact stamped at or after the cutoff,
    // or carrying an unusable (blank/malformed) timestamp, never enters.
    let now = projection_now();
    let window_days = params
        .window_days
        .unwrap_or(rules::DEFAULT_WINDOW_DAYS)
        .clamp(1, rules::MAX_WINDOW_DAYS);
    let since = rules::window_since(&now, window_days)
        .ok_or_else(|| format!("candidate_projection: cannot derive a window start from {now}"))?;
    let until = now.clone();
    let task_type = non_empty(params.task_type.as_deref()).map(|task| scrub(task.to_string()));

    // ONE store checkout: the windowed run views and their rubric rows are
    // read together so the projection cannot stitch a later state onto
    // earlier evidence.
    let (views, rubrics) = server.with_global_store_read(|store| {
        let conn = store.connection();
        let views = memcore::list_mirror_eval_run_views(conn, &since, Some(&until), row_limit)
            .map_err(|err| format!("candidate_projection: read mirror eval ledger: {err}"))?;
        let mut rubrics = BTreeMap::new();
        for view in &views {
            if let Some(adjudication) = view.current_adjudication() {
                match memcore::get_eval_rubric_score(conn, "mirror", &adjudication.adjudication_id)
                {
                    Ok(Some(row)) => {
                        rubrics.insert(adjudication.adjudication_id.clone(), row);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        return Err(format!(
                            "candidate_projection: read rubric row for adjudication '{}': {err}",
                            adjudication.adjudication_id
                        ))
                    }
                }
            }
        }
        Ok((views, rubrics))
    })?;
    let rows_truncated = views.len() >= row_limit;

    let mut budget = AdvisoryBudget::new();
    let mut candidates_json = Vec::with_capacity(prepared.len());
    for candidate in &prepared {
        let mut excluded_counts: BTreeMap<&str, usize> = EXCLUSION_REASONS
            .iter()
            .map(|reason| (*reason, 0))
            .collect();
        let mut revision_counts: BTreeMap<&str, usize> =
            REVISION_STATES.iter().map(|state| (*state, 0)).collect();
        let mut distinct_observed_revisions: BTreeSet<String> = BTreeSet::new();
        let mut eligible: Vec<(
            &memcore::MirrorEvalRunView,
            RevisionStatus,
            SampleCompatibility,
        )> = Vec::new();

        for view in &views {
            match classify_for_candidate(view, candidate, task_type.as_deref(), &since, &now) {
                Disposition::Eligible {
                    revision_status,
                    compatibility,
                } => {
                    *revision_counts.entry(revision_status.as_str()).or_insert(0) += 1;
                    if let Some(observed) = view.observation.as_ref() {
                        if let Some(model) = non_empty(observed.effective_model.as_deref()) {
                            let (revision, _) = observed_revision_of(observed, model);
                            if let Some(revision) = revision {
                                distinct_observed_revisions.insert(revision);
                            }
                        }
                    }
                    eligible.push((view, revision_status, compatibility));
                }
                Disposition::Excluded(reason) => {
                    *excluded_counts.entry(reason).or_insert(0) += 1;
                }
            }
        }

        let samples_total = eligible.len();
        let samples_truncated = samples_total > MAX_SAMPLES_PER_CANDIDATE;
        let samples: Vec<Value> = eligible
            .iter()
            .take(MAX_SAMPLES_PER_CANDIDATE)
            .map(|(view, revision_status, compatibility)| {
                let rubric = view
                    .current_adjudication()
                    .and_then(|adjudication| rubrics.get(&adjudication.adjudication_id));
                sample_json(
                    view,
                    revision_status,
                    compatibility,
                    rubric,
                    &now,
                    &mut budget,
                )
            })
            .collect();

        // Adjudication quality and compatibility are separate
        // statuses. `adjudication_status` says whether eligible
        // terminal/adjudicated/usable/non-self-eval rows exist at all;
        // `compatibility_status` says how far those rows CONFIRM the
        // candidate identity. A row only counts confirmed-compatible when
        // every scoping dimension (model, harness, role, task, revision) is
        // explicitly confirmed; unresolved (historical) rows are counted
        // separately and never reported as an unqualified "verified".
        let confirmed_samples = eligible
            .iter()
            .filter(|(_, _, compatibility)| compatibility.overall() == "fully_confirmed")
            .count();
        let unresolved_samples = eligible
            .iter()
            .filter(|(_, _, compatibility)| compatibility.overall() == "unresolved")
            .count();
        let adjudication_status = if samples_total > 0 {
            "verified"
        } else {
            "insufficient"
        };
        let compatibility_status = if samples_total == 0 {
            "insufficient"
        } else if confirmed_samples > 0 && confirmed_samples < samples_total {
            "mixed"
        } else if confirmed_samples > 0 {
            "fully_confirmed"
        } else if unresolved_samples > 0 {
            "unresolved"
        } else {
            // Every eligible row passed the hard gates but at least one
            // scoping dimension was left unscoped by the query — visibly
            // unscoped, never fully confirmed.
            "unscoped"
        };

        candidates_json.push(json!({
            "candidate_id": candidate.candidate_id,
            "requested_identity": {
                "model": candidate.model,
                "role": candidate.role,
                "harness": candidate.harness,
                "model_revision": candidate.model_revision,
            },
            "identity_confirmation": {
                "model": "matched_against_observed_effective_model_release",
                "harness": "matched_against_observed_effective_harness",
                "role": if candidate.role.is_some() {
                    "matched_against_observed_effective_role"
                } else {
                    "unscoped_by_query"
                },
                "task_type": if task_type.is_some() {
                    "scoped_against_register_requested_task_type"
                } else {
                    "unscoped_by_query"
                },
                "model_revision": "per_sample_see_identity_basis",
            },
            "adjudication_status": adjudication_status,
            "compatibility_status": compatibility_status,
            "confirmed_compatible_samples": confirmed_samples,
            "unresolved_samples": unresolved_samples,
            "samples_total": samples_total,
            "samples_returned": samples.len(),
            "samples_truncated": samples_truncated,
            "excluded_counts": excluded_counts,
            "model_revision_status_counts": revision_counts,
            "distinct_observed_revisions": distinct_observed_revisions,
            "samples": samples,
        }));
    }

    let payload = json!({
        "action": "candidate_projection",
        "evidence_source": "mirror_eval_lifecycle",
        "advisory_only": true,
        "launch_authority": false,
        "ranking": false,
        "read_only": true,
        "generated_at": now,
        "window": {
            "since": since,
            "until": until,
            "days": window_days,
            "note": "two-layer time contract: `since` bounds only the RUN cohort (a run's \
             created_at must be an actual instant in [since, generated_at), validated \
             RFC3339-authoritatively after the store's julianday preselect); every \
             associated fact — observation, current adjudication, superseded advisories, \
             rubric provenance — is bounded only from above, strictly before generated_at, \
             and may predate `since`. A fact stamped at/after the cutoff, or with an \
             unusable timestamp, never enters evidence",
        },
        "task_type": {
            "requested": task_type,
            "rows_recording_task_type": views
                .iter()
                .filter(|view| {
                    view.run
                        .requested_task_type
                        .as_deref()
                        .is_some_and(|task| !task.trim().is_empty())
                })
                .count(),
            "basis": "register_requested",
            "note": "the task anchor is the register-time requested_task_type (frozen \
             contract metadata), never a carrier-observed fact; a task-scoped query \
             excludes unrecorded rows with counts instead of counting them compatible",
        },
        "rows_considered": views.len(),
        "rows_limit": row_limit,
        "rows_truncated": rows_truncated,
        "candidates_supplied": prepared.len(),
        "candidates": candidates_json,
        "storage_gaps": {
            "legacy_rows": "rows recorded before the v34 columns exist carry NULL \
             requested_task_type/requested_role/effective_role/effective_model_revision; \
             those stay explicitly unknown/historical — they are excluded from \
             compatible samples for scoped queries (with counts/reasons), never \
             backfilled and never counted as confirmed-compatible",
            "role": "confirmation requires the carrier-observed effective_role; \
             requested_role/requested_profile are display-only and never confirm a \
             role (requested_profile is a profile, not a role)",
            "model_revision": "the explicit observed effective_model_revision column is \
             authoritative; a legacy '@version' suffix on the observed effective_model \
             is a fallback only where the column is NULL; new writes refuse conflicting \
             representations",
            "task_type": "register-time requested_task_type is the only task anchor \
             (register_requested basis); the mirror spine has no observed task fact",
        },
        "provisional_limits": {
            "max_candidates": MAX_CANDIDATES,
            "max_samples_per_candidate": MAX_SAMPLES_PER_CANDIDATE,
            "max_next_prompt_delta_chars": MAX_ADVISORY_CHARS,
            "max_total_advisory_chars": MAX_TOTAL_ADVISORY_CHARS,
            "max_artifact_refs_per_sample": MAX_ARTIFACT_REFS,
            "max_historical_advisories_per_sample": MAX_HISTORICAL_ADVISORIES,
            "note": "advisory texts are kept whole or dropped with an omission flag — never \
             sliced; candidate count and identity always survive the bounds",
        },
        "notes": [
            "advisory pre-dispatch evidence only: the native host retains model choice, spawn, \
             and every launch decision; this projection grants no authority and writes nothing",
            "no score, rate, or ranking is computed — a candidate with no eligible rows is \
             reported 'insufficient', never interpolated or ranked against others",
            "adjudication_status and compatibility_status are separate: 'verified' adjudication \
             with only revision-unresolved rows reports compatibility 'unresolved' (historical), \
             never an unqualified verified compatibility; a sample counts confirmed-compatible \
             only when model, harness, role, task, and revision are all explicitly confirmed, \
             and dimensions the query left unscoped are labeled 'unscoped', not confirmed",
            "identity matching uses carrier-OBSERVED effective model/harness/role only; \
             register-time requested identity is displayed, never matched",
            "explicitly different requested aliases share evidence only when their OBSERVED \
             effective identity is the same; never guessed from names, families, or aliases",
            "eligible rows are terminal, adjudicated (current last-appended event strictly \
             before the projection instant — no fallback to an older usable event when the \
             latest is future), evidence_usable, and non-self-eval; rejected/failed outcomes \
             stay failures",
            "a candidate model string's '@version' suffix IS its declared revision when the \
             separate model_revision field is omitted; disagreeing representations are a \
             validation error, never silently reconciled",
            "an explicitly mismatched role, task, harness, or revision excludes the row with a \
             count and reason — it is never shown as compatible advice",
            "future facts are not eligible: associated facts are strictly before \
             generated_at (upper bound only — they may predate `since`; only the run's \
             created_at must fall in [since, generated_at) as an actual instant), and \
             unusable timestamps fail the same bounds",
            "the store's windowed read preselects runs by SQLite julianday instant with \
             the row cap applied BEFORE this projection's RFC3339 validation — a bounded \
             scan: rows rejected here as run_timestamp_malformed or run_out_of_window are \
             not backfilled by rescanning past the cap",
            "no raw transcripts, credentials, or task corpus are returned; stored fields were \
             scrubbed at write time and artifact entries are references only",
        ],
    });
    serde_json::to_string(&payload).map_err(|err| format!("serialize candidate_projection: {err}"))
}
