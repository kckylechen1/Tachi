//! The D2 pilot report (#1073 "Kill gate").
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
//! numbers — thresholds are per-action, named, provisional"). `PilotReport`
//! reports the numbers the frozen contract asks for; whether that yield
//! constitutes "material cold-start decision change" is the reader's
//! (leader/owner's) call, made against the reported evidence.

use tachi_params::LessonEngineReceiptV1;

use super::discrimination::{AdjudicatorReceipt, CaseOutcome};

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

#[derive(Debug, Clone, Default)]
pub struct PilotReport {
    pub cases: Vec<CaseReport>,
}

impl PilotReport {
    pub fn passed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| matches!(c.outcome, CaseOutcome::Pass))
            .count()
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

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# D2 50-row pilot report (#1073)\n\n");
        out.push_str(&format!(
            "Pass yield: {}/{} ({:.1}%) — {} failed, {} inconclusive\n\n",
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
        out.push_str("| case_id | outcome | reasons |\n|---|---|---|\n");
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
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                case.case_id, outcome_label, reasons
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson_forge_ops::discrimination::FailReason;

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
            effective_provider: Some("openai".to_string()),
            effective_model: Some("gpt".to_string()),
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
        use crate::lesson_forge_ops::discrimination::{ArmRunSet, CaseInput, ColdRunScore};

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
            dual_track_attested: true,
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
            dual_track_attested: true,
        };
        assert_eq!(
            super::super::discrimination::evaluate_case(&real_case),
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
        let md = report.to_markdown();
        assert!(!md.to_ascii_lowercase().contains("verdict: stop"));
        assert!(!md.to_ascii_lowercase().contains("verdict: proceed"));
        assert!(md.contains("leader/owner adjudication"));
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
