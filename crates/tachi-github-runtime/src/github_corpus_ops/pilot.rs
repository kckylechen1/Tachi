//! The frozen 20-case GitHub corpus pilot (#1059).
//!
//! "Freeze exactly 20 owner-controlled cases" — frozen contract. This is
//! the corpus adapter's own size-20 gate; it does **not** reuse
//! `lesson_forge_ops::PilotManifestV1` (the 50-row D2 gate).
//!
//! [`freeze_corpus_manifest`] is the validation/freezing act:
//! [`crate::github_corpus_ops::adapt::adapt_corpus_case`] refuses to emit a
//! candidate for any `case_id` that isn't a member of an already-frozen
//! manifest.

use tachi_params::LessonCandidateKindV1;

/// Exact pilot size named by the frozen contract ("exactly 20").
pub const CORPUS_PILOT_SIZE: usize = 20;

/// One frozen corpus case: identity, selection reason, and the predeclared
/// reference decision + cold-start material decision recorded before adapt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusCaseV1 {
    pub case_id: String,
    pub repo: String,
    pub issue_number: u64,
    pub pr_number: Option<u64>,
    /// Why this case was selected, recorded before any adapt spend.
    pub selection_reason: String,
    /// Predeclared reference ruling this case is scored against.
    pub reference_decision: String,
    pub target_kind: LessonCandidateKindV1,
    /// Cold-start material decision recorded before adapt.
    pub cold_start_material_decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusFreezeError {
    WrongCaseCount { expected: usize, actual: usize },
    DuplicateCaseId { case_id: String },
    MissingSelectionReason { case_id: String },
    MissingReferenceDecision { case_id: String },
    MissingColdStartMaterialDecision { case_id: String },
    NoPrecedentCases,
}

impl std::fmt::Display for CorpusFreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCaseCount { expected, actual } => write!(
                f,
                "corpus manifest must have exactly {expected} cases, got {actual}"
            ),
            Self::DuplicateCaseId { case_id } => {
                write!(f, "duplicate corpus case_id {case_id}")
            }
            Self::MissingSelectionReason { case_id } => {
                write!(f, "corpus case {case_id} is missing a selection_reason")
            }
            Self::MissingReferenceDecision { case_id } => write!(
                f,
                "corpus case {case_id} is missing a predeclared reference_decision"
            ),
            Self::MissingColdStartMaterialDecision { case_id } => write!(
                f,
                "corpus case {case_id} is missing a cold_start_material_decision"
            ),
            Self::NoPrecedentCases => {
                write!(f, "corpus manifest has no Precedent target_kind cases")
            }
        }
    }
}

/// A frozen, spend-gating corpus manifest. Construct only via
/// [`freeze_corpus_manifest`] — there is no public constructor that skips
/// validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusManifestV1 {
    cases: Vec<CorpusCaseV1>,
}

impl CorpusManifestV1 {
    pub fn cases(&self) -> &[CorpusCaseV1] {
        &self.cases
    }

    pub fn contains(&self, case_id: &str) -> bool {
        self.cases.iter().any(|c| c.case_id == case_id)
    }

    pub fn find(&self, case_id: &str) -> Option<&CorpusCaseV1> {
        self.cases.iter().find(|c| c.case_id == case_id)
    }
}

/// Validate + freeze a candidate corpus case set. Returns every violation
/// found (not just the first).
pub fn freeze_corpus_manifest(
    cases: Vec<CorpusCaseV1>,
) -> Result<CorpusManifestV1, Vec<CorpusFreezeError>> {
    let mut errors = Vec::new();

    if cases.len() != CORPUS_PILOT_SIZE {
        errors.push(CorpusFreezeError::WrongCaseCount {
            expected: CORPUS_PILOT_SIZE,
            actual: cases.len(),
        });
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for case in &cases {
        if !seen.insert(case.case_id.clone()) {
            errors.push(CorpusFreezeError::DuplicateCaseId {
                case_id: case.case_id.clone(),
            });
        }
        if case.selection_reason.trim().is_empty() {
            errors.push(CorpusFreezeError::MissingSelectionReason {
                case_id: case.case_id.clone(),
            });
        }
        if case.reference_decision.trim().is_empty() {
            errors.push(CorpusFreezeError::MissingReferenceDecision {
                case_id: case.case_id.clone(),
            });
        }
        if case.cold_start_material_decision.trim().is_empty() {
            errors.push(CorpusFreezeError::MissingColdStartMaterialDecision {
                case_id: case.case_id.clone(),
            });
        }
    }

    if !cases
        .iter()
        .any(|c| c.target_kind == LessonCandidateKindV1::Precedent)
    {
        errors.push(CorpusFreezeError::NoPrecedentCases);
    }

    if errors.is_empty() {
        Ok(CorpusManifestV1 { cases })
    } else {
        Err(errors)
    }
}

/// Test/fixture helper: exactly 20 valid cases (≥1 Precedent).
pub fn valid_20_cases() -> Vec<CorpusCaseV1> {
    let kinds = [
        LessonCandidateKindV1::Precedent,
        LessonCandidateKindV1::BugClass,
        LessonCandidateKindV1::VerificationPattern,
        LessonCandidateKindV1::LaneEvidence,
    ];
    (0..CORPUS_PILOT_SIZE)
        .map(|i| {
            let kind = kinds[i % kinds.len()];
            let issue_number = 1000 + i as u64;
            let pr_number = if i % 3 == 0 {
                Some(2000 + i as u64)
            } else {
                None
            };
            CorpusCaseV1 {
                case_id: format!("corpus-case-{i}"),
                repo: "owner/repo".to_string(),
                issue_number,
                pr_number,
                selection_reason: format!("selected corpus-case-{i} for provenance coverage"),
                reference_decision: format!("reference decision for corpus-case-{i}"),
                target_kind: kind,
                cold_start_material_decision: format!(
                    "cold-start material decision for corpus-case-{i}"
                ),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_requires_exactly_20() {
        let mut cases = valid_20_cases();
        cases.pop();
        let errors = freeze_corpus_manifest(cases).expect_err("19 cases must not freeze");
        assert!(errors.contains(&CorpusFreezeError::WrongCaseCount {
            expected: 20,
            actual: 19
        }));

        let mut cases = valid_20_cases();
        cases.push(CorpusCaseV1 {
            case_id: "corpus-case-extra".to_string(),
            repo: "owner/repo".to_string(),
            issue_number: 9999,
            pr_number: None,
            selection_reason: "extra".to_string(),
            reference_decision: "extra".to_string(),
            target_kind: LessonCandidateKindV1::Precedent,
            cold_start_material_decision: "extra".to_string(),
        });
        let errors = freeze_corpus_manifest(cases).expect_err("21 cases must not freeze");
        assert!(errors.contains(&CorpusFreezeError::WrongCaseCount {
            expected: 20,
            actual: 21
        }));
    }

    #[test]
    fn missing_reason_is_rejected() {
        let mut cases = valid_20_cases();
        cases[0].selection_reason = "   ".to_string();
        let errors = freeze_corpus_manifest(cases).expect_err("blank reason must not freeze");
        assert!(errors
            .iter()
            .any(|e| matches!(e, CorpusFreezeError::MissingSelectionReason { .. })));
    }

    #[test]
    fn valid_20_cases_freeze_cleanly() {
        let manifest =
            freeze_corpus_manifest(valid_20_cases()).expect("20 valid cases must freeze");
        assert_eq!(manifest.cases().len(), 20);
        assert!(manifest.contains("corpus-case-0"));
        assert!(!manifest.contains("missing"));
    }
}
