//! Legacy, non-authoritative pilot preview aggregation.
//!
//! "Report pass yield, per-case outcomes, token/cost/latency, provider
//! receipts, and old-vs-new recall simulation before any scale decision." —
//! frozen contract.
//!
//! This module deliberately does NOT compute an automatic stop/proceed
//! verdict. The frozen contract's kill gate ("If the pilot cannot show
//! material cold-start decision change, stop...") names no numeric pass-
//! yield threshold, and inventing one here would be exactly the kind of
//! flat, un-adjudicated magic number this repo's own engineering discipline
//! forbids for a gate this consequential (AGENTS.md: "No flat magic
//! numbers — thresholds are per-action, named, provisional"). This legacy
//! module predates exact 400-call accounting and cannot publish the D2 pilot
//! completion artifact. [`super::runner::PilotRunReportV1`] is the only
//! authoritative report surface.

use tachi_params::LessonEngineReceiptV1;

use super::discrimination::{AdjudicatorReceipt, CaseOutcome};
use super::pilot::PilotManifestV1;

/// Caller-supplied summary of an old-vs-new `recall_simulate` comparison
/// for one case. This module does not run `recall_simulate` itself (that
/// tool already exists independently — see `facade_memory_ops::recall_simulate_ops`
/// — and wiring a live simulate call into this leaf's report is left to the
/// harness runner that actually executes the pilot, not this leaf's
/// deterministic report-aggregation code).
#[derive(Debug, Clone)]
pub struct RecallSimulationNote {
    pub old_arm_hit: bool,
    pub new_arm_hit: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct CaseReport {
    pub case_id: String,
    pub outcome: CaseOutcome,
    pub treated_tokens: Option<u64>,
    pub baseline_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
    pub latency_ms: Option<u64>,
    pub producer_receipt: Option<LessonEngineReceiptV1>,
    pub adjudicator_receipt: AdjudicatorReceipt,
    pub recall_simulation: Option<RecallSimulationNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PilotReportError {
    WrongCaseCount {
        expected: usize,
        actual: usize,
    },
    DuplicateCaseId(String),
    /// A case's `case_id` doesn't match any row in the frozen manifest —
    /// evidence for a row that was never selected/spend-gated cannot count
    /// toward this pilot's kill-gate decision.
    CaseNotInManifest(String),
    AmbiguousLegacyCaseId(String),
}

impl std::fmt::Display for PilotReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCaseCount { expected, actual } => write!(
                f,
                "pilot report must have exactly {expected} cases (one per frozen manifest row), got {actual}"
            ),
            Self::DuplicateCaseId(id) => write!(f, "duplicate case_id in pilot report: {id}"),
            Self::CaseNotInManifest(id) => write!(
                f,
                "case_id {id} is not a row in the frozen pilot manifest — evidence for an \
                 unselected row cannot count toward the kill-gate decision"
            ),
            Self::AmbiguousLegacyCaseId(id) => write!(
                f,
                "legacy case_id {id} is ambiguous; use the canonical route/id/revision binding"
            ),
        }
    }
}

#[derive(Debug, Clone, Default)]
/// Legacy aggregate preview. It intentionally cannot render an authoritative
/// D2 completion report because its optional totals do not attest 400 calls.
pub struct PilotReport {
    cases: Vec<CaseReport>,
}

impl PilotReport {
    /// Build a `PilotReport` structurally bound to a frozen pilot manifest
    /// (cross-vendor review finding 9: a plain `PilotReport { cases: ... }`
    /// literal accepts an arbitrary `Vec<CaseReport>` with no relationship
    /// to the actual frozen 50-row pilot — a caller could report 3 cases,
    /// duplicate one case_id, or report cases for rows that were never
    /// frozen, and nothing would object). Refuses unless the case count
    /// matches the manifest's row count exactly, every `case_id` resolves to
    /// one route/id/revision binding, and no binding repeats. Legacy bare
    /// source ids are accepted only when exactly one manifest row has that
    /// id; accepted rows are normalized to their canonical full binding.
    ///
    /// This is the only public constructor for a populated report. The cases
    /// field stays private so callers cannot bypass binding normalization;
    /// this module's unit tests still use hand-built fixtures to exercise the
    /// aggregation logic independently.
    pub fn from_manifest(
        manifest: &PilotManifestV1,
        mut cases: Vec<CaseReport>,
    ) -> Result<Self, Vec<PilotReportError>> {
        let mut errors = Vec::new();
        if cases.len() != manifest.rows().len() {
            errors.push(PilotReportError::WrongCaseCount {
                expected: manifest.rows().len(),
                actual: cases.len(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        for case in &mut cases {
            let matches: Vec<&super::pilot::PilotRowV1> = manifest
                .rows()
                .iter()
                .filter(|row| {
                    row.canonical_case_id() == case.case_id || row.source_id == case.case_id
                })
                .collect();
            let Some(binding) = (matches.len() == 1).then(|| matches[0]) else {
                if matches.is_empty() {
                    errors.push(PilotReportError::CaseNotInManifest(case.case_id.clone()));
                } else {
                    errors.push(PilotReportError::AmbiguousLegacyCaseId(
                        case.case_id.clone(),
                    ));
                }
                continue;
            };
            let canonical_id = binding.canonical_case_id();
            if !seen.insert(binding.binding_key()) {
                errors.push(PilotReportError::DuplicateCaseId(canonical_id.clone()));
            }
            case.case_id = canonical_id;
        }
        if errors.is_empty() {
            Ok(Self { cases })
        } else {
            Err(errors)
        }
    }

    pub fn passed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| matches!(c.outcome, CaseOutcome::Pass))
            .count()
    }

    pub fn cases(&self) -> &[CaseReport] {
        &self.cases
    }

    pub fn failed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| matches!(c.outcome, CaseOutcome::Fail(_)))
            .count()
    }

    pub fn inconclusive_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| matches!(c.outcome, CaseOutcome::Inconclusive(_)))
            .count()
    }

    pub fn total(&self) -> usize {
        self.cases.len()
    }

    /// passed / total. Inconclusive cases count in the denominator, not the
    /// numerator — a case whose dual-track independence couldn't be
    /// attested is never counted as evidence of a material decision change
    /// (conservative by construction, matching the frozen contract's
    /// "Failing candidates are rejected, not averaged away").
    pub fn pass_yield(&self) -> f64 {
        if self.cases.is_empty() {
            return 0.0;
        }
        self.passed_count() as f64 / self.total() as f64
    }

    pub fn total_cost_usd(&self) -> f64 {
        self.cases.iter().filter_map(|c| c.total_cost_usd).sum()
    }

    pub fn total_treated_tokens(&self) -> u64 {
        self.cases.iter().filter_map(|c| c.treated_tokens).sum()
    }

    pub fn total_baseline_tokens(&self) -> u64 {
        self.cases.iter().filter_map(|c| c.baseline_tokens).sum()
    }

    /// Cases whose producer receipt is not a fully known, non-fallback,
    /// non-degraded identity — these are preview-only per the frozen
    /// contract and should be called out prominently in any report a
    /// reader uses to make a scale decision.
    pub fn preview_only_case_ids(&self) -> Vec<&str> {
        self.cases
            .iter()
            .filter(|c| {
                !c.producer_receipt
                    .as_ref()
                    .map(LessonEngineReceiptV1::has_known_identity)
                    .unwrap_or(false)
            })
            .map(|c| c.case_id.as_str())
            .collect()
    }

    pub fn to_preview_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# NON-AUTHORITATIVE LEGACY PREVIEW (#1073)\n\n");
        out.push_str(
            "This preview is not the D2 50-row pilot report, cannot establish pilot completion, \
             and does not replace the authoritative 400-call runner report.\n\n",
        );
        out.push_str(&format!(
            "Preview pass yield: {}/{} ({:.1}%) — {} failed, {} inconclusive\n\n",
            self.passed_count(),
            self.total(),
            self.pass_yield() * 100.0,
            self.failed_count(),
            self.inconclusive_count(),
        ));
        out.push_str(&format!(
            "Total cost: ${:.4} — treated tokens: {} — baseline tokens: {}\n\n",
            self.total_cost_usd(),
            self.total_treated_tokens(),
            self.total_baseline_tokens(),
        ));
        let preview_only = self.preview_only_case_ids();
        if !preview_only.is_empty() {
            out.push_str(&format!(
                "**{} case(s) have a preview-only (unknown/fallback/degraded) producer \
                 identity and cannot be attributed to a specific engine:** {}\n\n",
                preview_only.len(),
                preview_only.join(", ")
            ));
        }
        out.push_str(
            "This report states the measured numbers only. Whether the pass yield above \
             constitutes a \"material cold-start decision change\" sufficient to proceed past \
             the kill gate is a leader/owner adjudication against this evidence, not an \
             automatic verdict this report computes.\n\n",
        );
        // Cross-vendor review finding 9: the table used to carry only
        // case_id/outcome/reasons, dropping the per-case tokens/cost/
        // latency/receipts/recall-simulation the frozen contract's
        // "Verification" ask explicitly names ("Report pass yield, per-case
        // outcomes, token/cost/latency, provider receipts, and old-vs-new
        // recall simulation").
        out.push_str(
            "| source_binding | outcome | reasons | treated_tokens | baseline_tokens | cost_usd | \
             latency_ms | producer_identity | adjudicator | recall_sim |\n\
             |---|---|---|---|---|---|---|---|---|---|\n",
        );
        for case in &self.cases {
            let (outcome_label, reasons) = match &case.outcome {
                CaseOutcome::Pass => ("PASS".to_string(), String::new()),
                CaseOutcome::Fail(reasons) => (
                    "FAIL".to_string(),
                    reasons
                        .iter()
                        .map(|r| format!("{r:?}"))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
                CaseOutcome::Inconclusive(reason) => {
                    ("INCONCLUSIVE".to_string(), (*reason).to_string())
                }
            };
            let producer_identity = case
                .producer_receipt
                .as_ref()
                .map(LessonEngineReceiptV1::identity_status)
                .unwrap_or("preview_only");
            let adjudicator = match (
                &case.adjudicator_receipt.effective_provider,
                &case.adjudicator_receipt.effective_model,
            ) {
                (Some(provider), Some(model)) => format!("{provider}/{model}"),
                _ => "unknown".to_string(),
            };
            let recall_sim = case
                .recall_simulation
                .as_ref()
                .map(|note| format!("old={} new={}", note.old_arm_hit, note.new_arm_hit))
                .unwrap_or_else(|| "n/a".to_string());
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                case.case_id,
                outcome_label,
                reasons,
                case.treated_tokens
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                case.baseline_tokens
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                case.total_cost_usd
                    .map(|v| format!("{v:.4}"))
                    .unwrap_or_else(|| "n/a".to_string()),
                case.latency_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                producer_identity,
                adjudicator,
                recall_sim,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discrimination::FailReason;

    fn known_receipt() -> LessonEngineReceiptV1 {
        LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        }
    }

    fn adjudicator() -> AdjudicatorReceipt {
        AdjudicatorReceipt {
            requested_role: "adjudicator".to_string(),
            effective_provider: Some("openai".to_string()),
            effective_model: Some("gpt".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        }
    }

    fn case(id: &str, outcome: CaseOutcome, receipt: Option<LessonEngineReceiptV1>) -> CaseReport {
        CaseReport {
            case_id: id.to_string(),
            outcome,
            treated_tokens: Some(100),
            baseline_tokens: Some(80),
            total_cost_usd: Some(0.05),
            latency_ms: Some(1200),
            producer_receipt: receipt,
            adjudicator_receipt: adjudicator(),
            recall_simulation: None,
        }
    }

    /// A 50-row frozen manifest whose row ids are `row-0..row-49`, for the
    /// `PilotReport::from_manifest` binding tests below.
    fn fifty_row_manifest() -> PilotManifestV1 {
        use crate::pilot::{
            freeze_pilot_manifest, PilotRowKindV1, PilotRowV1, PilotSourceRouteV1, PilotStratumV1,
        };
        use tachi_params::LessonCandidateKindV1;

        let rows = (0..50)
            .map(|index| PilotRowV1 {
                source_route: if index < 25 {
                    PilotSourceRouteV1::Antigravity
                } else {
                    PilotSourceRouteV1::Hapi
                },
                source_id: format!("row-{index}"),
                source_revision: 1,
                content_sha256: format!("{index:064x}"),
                capture_timestamp: "2026-07-24T00:00:00Z".to_string(),
                kind: if index % 2 == 0 {
                    PilotRowKindV1::Narrative
                } else {
                    PilotRowKindV1::StructuredControl
                },
                stratum: match index {
                    0..=16 => PilotStratumV1::CorrectionAlignment,
                    17..=33 => PilotStratumV1::VerificationRecovery,
                    _ => PilotStratumV1::RoutingStoreProvenance,
                },
                selection_reason: "public-safe fixture reason".to_string(),
                reference_decision: "public-safe fixture decision".to_string(),
                target_kind: LessonCandidateKindV1::Precedent,
            })
            .collect();
        freeze_pilot_manifest(rows).expect("test manifest must freeze")
    }

    #[test]
    fn from_manifest_refuses_a_case_count_mismatch() {
        let manifest = fifty_row_manifest();
        let cases = vec![case("row-0", CaseOutcome::Pass, Some(known_receipt()))];
        let errors = PilotReport::from_manifest(&manifest, cases)
            .expect_err("1 case against a 50-row manifest must be refused");
        assert!(errors.iter().any(|e| matches!(
            e,
            PilotReportError::WrongCaseCount {
                expected: 50,
                actual: 1
            }
        )));
    }

    #[test]
    fn from_manifest_refuses_a_case_id_not_in_the_manifest() {
        let manifest = fifty_row_manifest();
        let mut cases: Vec<CaseReport> = (0..50)
            .map(|i| {
                case(
                    &format!("row-{i}"),
                    CaseOutcome::Pass,
                    Some(known_receipt()),
                )
            })
            .collect();
        cases[0].case_id = "not-a-real-row".to_string();
        let errors = PilotReport::from_manifest(&manifest, cases)
            .expect_err("a case_id outside the manifest must be refused");
        assert!(errors.iter().any(
            |e| matches!(e, PilotReportError::CaseNotInManifest(id) if id == "not-a-real-row")
        ));
    }

    #[test]
    fn from_manifest_refuses_duplicate_case_ids() {
        let manifest = fifty_row_manifest();
        let mut cases: Vec<CaseReport> = (0..50)
            .map(|i| {
                case(
                    &format!("row-{i}"),
                    CaseOutcome::Pass,
                    Some(known_receipt()),
                )
            })
            .collect();
        cases[1].case_id = cases[0].case_id.clone();
        let errors = PilotReport::from_manifest(&manifest, cases)
            .expect_err("duplicate case_id must be refused");
        assert!(errors
            .iter()
            .any(|e| matches!(e, PilotReportError::DuplicateCaseId(_))));
    }

    #[test]
    fn from_manifest_accepts_exactly_the_manifest_rows() {
        let manifest = fifty_row_manifest();
        let cases: Vec<CaseReport> = (0..50)
            .map(|i| {
                case(
                    &format!("row-{i}"),
                    CaseOutcome::Pass,
                    Some(known_receipt()),
                )
            })
            .collect();
        let report =
            PilotReport::from_manifest(&manifest, cases).expect("exactly-matching cases must bind");
        assert_eq!(report.total(), 50);
    }

    #[test]
    fn from_manifest_keeps_cross_route_same_id_rows_distinct() {
        let mut rows = fifty_row_manifest().rows().to_vec();
        rows[0].source_id = "shared-id".to_string();
        rows[25].source_id = "shared-id".to_string();
        let manifest =
            crate::pilot::freeze_pilot_manifest(rows).expect("cross-route ids may overlap");
        let cases: Vec<CaseReport> = manifest
            .rows()
            .iter()
            .map(|row| {
                case(
                    &format!(
                        "{}:{}@{}",
                        row.source_route.as_str(),
                        row.source_id,
                        row.source_revision
                    ),
                    CaseOutcome::Pass,
                    Some(known_receipt()),
                )
            })
            .collect();
        let report = PilotReport::from_manifest(&manifest, cases)
            .expect("full route/id/revision bindings must remain distinct");
        assert_eq!(report.total(), 50);
        let markdown = report.to_preview_markdown();
        assert!(markdown.contains("antigravity:shared-id@1"));
        assert!(markdown.contains("hapi:shared-id@1"));
    }

    #[test]
    fn from_manifest_rejects_ambiguous_legacy_source_id() {
        let mut rows = fifty_row_manifest().rows().to_vec();
        rows[0].source_id = "shared-id".to_string();
        rows[25].source_id = "shared-id".to_string();
        let manifest =
            crate::pilot::freeze_pilot_manifest(rows).expect("cross-route ids may overlap");
        let mut cases: Vec<CaseReport> = manifest
            .rows()
            .iter()
            .map(|row| {
                case(
                    &row.canonical_case_id(),
                    CaseOutcome::Pass,
                    Some(known_receipt()),
                )
            })
            .collect();
        cases[0].case_id = "shared-id".to_string();
        let errors = PilotReport::from_manifest(&manifest, cases)
            .expect_err("ambiguous legacy id must not alias either source route");
        assert!(errors.iter().any(
            |error| matches!(error, PilotReportError::AmbiguousLegacyCaseId(id) if id == "shared-id")
        ));
    }

    #[test]
    fn empty_report_has_zero_yield_not_a_divide_by_zero_panic() {
        let report = PilotReport::default();
        assert_eq!(report.pass_yield(), 0.0);
        assert_eq!(report.total(), 0);
    }

    #[test]
    fn pass_yield_counts_inconclusive_in_denominator_not_numerator() {
        let report = PilotReport {
            cases: vec![
                case("c1", CaseOutcome::Pass, Some(known_receipt())),
                case(
                    "c2",
                    CaseOutcome::Inconclusive("no dual-track attestation"),
                    Some(known_receipt()),
                ),
            ],
        };
        assert_eq!(report.passed_count(), 1);
        assert_eq!(report.inconclusive_count(), 1);
        assert_eq!(report.total(), 2);
        assert!((report.pass_yield() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn old_summary_baseline_case_fails_where_candidate_case_passes_in_a_report() {
        // Report-aggregation half of the Verification section's explicit
        // ask ("Fixtures must demonstrate that old summary output fails the
        // target-decision discrimination where the accepted candidate
        // passes"). The mechanism-level proof — the SAME `evaluate_case`
        // call substituting the old-summary arm as treated versus the real
        // candidate-as-treated case — lives in
        // `old_summary_arm_fails_as_treated_while_the_real_candidate_case_passes`
        // below; this test only proves `PilotReport` counts/labels a failed
        // case and a passed case correctly once `evaluate_case` has run.
        let baseline_case = case(
            "row-1-baseline-summary",
            CaseOutcome::Fail(vec![FailReason::TreatedBelowThreshold {
                hits: 0,
                required: 2,
            }]),
            None, // old Summary lane has no producer-engine receipt to attest
        );
        let candidate_case = case("row-1-candidate", CaseOutcome::Pass, Some(known_receipt()));
        let report = PilotReport {
            cases: vec![baseline_case, candidate_case],
        };
        assert_eq!(report.failed_count(), 1);
        assert_eq!(report.passed_count(), 1);
    }

    #[test]
    fn old_summary_arm_fails_as_treated_while_the_real_candidate_case_passes() {
        // The Verification section's explicit ask, proved against the real
        // `evaluate_case` decision function (not a fabricated outcome):
        // "Fixtures must demonstrate that old summary output fails the
        // target-decision discrimination where the accepted candidate
        // passes."
        use crate::discrimination::{ArmRunSet, CaseInput, ColdRunScore};

        fn hit(matches: bool) -> ColdRunScore {
            ColdRunScore {
                matches_reference_decision: matches,
                unsupported_claims: 0,
            }
        }

        let old_summary_arm = ArmRunSet {
            runs: [hit(false), hit(false), hit(false)], // 0/3 reference hits
        };
        let candidate_arm = ArmRunSet {
            runs: [hit(true), hit(true), hit(false)], // 2/3 reference hits
        };

        // Substitute the old-summary arm in as the TREATED arm: it fails
        // criterion 1 outright (0/3 < the required 2/3) and cites no refs.
        let old_summary_as_treated = CaseInput {
            case_id: "row-1".to_string(),
            treated: old_summary_arm,
            baseline: ArmRunSet {
                runs: [hit(false), hit(false), hit(false)],
            },
            candidate_cites_source_refs: false,
            candidate_claims_establishment: false,
            producer_receipt: Some(known_receipt()),
            adjudicator_receipt: adjudicator(),
        };
        assert!(matches!(
            super::super::discrimination::evaluate_case(&old_summary_as_treated),
            CaseOutcome::Fail(_)
        ));

        // The real case — the forged candidate as treated, the old summary
        // as baseline — passes.
        let real_case = CaseInput {
            case_id: "row-1".to_string(),
            treated: candidate_arm,
            baseline: old_summary_arm,
            candidate_cites_source_refs: true,
            candidate_claims_establishment: false,
            producer_receipt: Some(known_receipt()),
            adjudicator_receipt: adjudicator(),
        };
        assert_eq!(
            super::super::discrimination::evaluate_case(&real_case),
            CaseOutcome::Pass
        );
    }

    #[test]
    fn old_summary_fails_target_decision_discrimination_in_isolation_from_citation() {
        // Cross-vendor review finding 10: the combined fixture above also
        // flips `candidate_cites_source_refs` between the two cases, so a
        // broken criterion-1 (reference-hit) check could hide behind a
        // still-correct criterion-4 (citation) check and this test would
        // never notice. This isolates criterion 1 ALONE: both arms cite
        // refs identically (both `true`) and neither claims establishment,
        // so the ONLY variable between "old summary as treated" (fails) and
        // "candidate as treated" (passes) is the reference-hit count itself
        // — a break in criterion-1's own logic cannot hide behind a
        // different criterion's failure here.
        use crate::discrimination::{ArmRunSet, CaseInput, ColdRunScore, FailReason};

        fn hit(matches: bool) -> ColdRunScore {
            ColdRunScore {
                matches_reference_decision: matches,
                unsupported_claims: 0,
            }
        }

        let old_summary_arm = ArmRunSet {
            runs: [hit(false), hit(false), hit(false)], // 0/3 reference hits
        };
        let candidate_arm = ArmRunSet {
            runs: [hit(true), hit(true), hit(false)], // 2/3 reference hits
        };
        let neutral_baseline = ArmRunSet {
            runs: [hit(false), hit(false), hit(false)],
        };

        let old_summary_as_treated = CaseInput {
            case_id: "row-1-isolated".to_string(),
            treated: old_summary_arm,
            baseline: neutral_baseline,
            candidate_cites_source_refs: true,
            candidate_claims_establishment: false,
            producer_receipt: Some(known_receipt()),
            adjudicator_receipt: adjudicator(),
        };
        let old_summary_outcome =
            super::super::discrimination::evaluate_case(&old_summary_as_treated);
        assert_eq!(
            old_summary_outcome,
            CaseOutcome::Fail(vec![FailReason::TreatedBelowThreshold {
                hits: 0,
                required: 2,
            }]),
            "old summary must fail ONLY on the reference-hit criterion, not a citation confound: \
             {old_summary_outcome:?}"
        );

        let candidate_as_treated = CaseInput {
            case_id: "row-1-isolated".to_string(),
            treated: candidate_arm,
            baseline: neutral_baseline,
            candidate_cites_source_refs: true,
            candidate_claims_establishment: false,
            producer_receipt: Some(known_receipt()),
            adjudicator_receipt: adjudicator(),
        };
        assert_eq!(
            super::super::discrimination::evaluate_case(&candidate_as_treated),
            CaseOutcome::Pass
        );
    }

    #[test]
    fn preview_only_cases_are_flagged_by_missing_or_unknown_receipt() {
        let report = PilotReport {
            cases: vec![
                case("c1", CaseOutcome::Pass, Some(known_receipt())),
                case("c2", CaseOutcome::Pass, None),
            ],
        };
        assert_eq!(report.preview_only_case_ids(), vec!["c2"]);
    }

    #[test]
    fn markdown_never_states_an_automatic_stop_or_proceed_verdict() {
        let report = PilotReport {
            cases: vec![case("c1", CaseOutcome::Pass, Some(known_receipt()))],
        };
        let md = report.to_preview_markdown();
        assert!(!md.to_ascii_lowercase().contains("verdict: stop"));
        assert!(!md.to_ascii_lowercase().contains("verdict: proceed"));
        assert!(md.contains("leader/owner adjudication"));
    }

    #[test]
    fn legacy_markdown_cannot_claim_d2_completion_without_400_call_accounting() {
        let manifest = fifty_row_manifest();
        let cases: Vec<CaseReport> = manifest
            .rows()
            .iter()
            .map(|row| {
                case(
                    &row.canonical_case_id(),
                    CaseOutcome::Pass,
                    Some(known_receipt()),
                )
            })
            .collect();
        let report = PilotReport::from_manifest(&manifest, cases).unwrap();
        let markdown = report.to_preview_markdown();
        assert!(markdown.contains("NON-AUTHORITATIVE LEGACY PREVIEW"));
        assert!(!markdown.contains("# D2 50-row pilot report"));
    }

    #[test]
    fn total_cost_and_tokens_sum_across_cases() {
        let report = PilotReport {
            cases: vec![
                case("c1", CaseOutcome::Pass, Some(known_receipt())),
                case("c2", CaseOutcome::Pass, Some(known_receipt())),
            ],
        };
        assert!((report.total_cost_usd() - 0.10).abs() < 1e-9);
        assert_eq!(report.total_treated_tokens(), 200);
        assert_eq!(report.total_baseline_tokens(), 160);
    }
}
