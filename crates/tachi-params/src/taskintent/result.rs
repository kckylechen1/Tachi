//! `ResultProjectionV1` for `collect` (tachi#1840, zeroclaw #205 TB-13).
//!
//! Artifact/evidence FIRST: relay prose is not the canonical verdict. A
//! worker `success` without the required artifact/evidence does NOT satisfy
//! the contract — contract violations are computed by comparing the
//! SUBMITTED intent's `expected_artifacts` against the terminal outcome's
//! evidence facts (`dispatch_outcomes.verification_present` /
//! `diff_present` / `evidence_refs`), never against the worker's
//! self-report.
//!
//! Result revisions are Tachi-minted and monotonic: the revision is the
//! count of result-bearing canonical facts (outcome + adjudication events)
//! for the task, so it can only advance. `collect()` with no argument
//! returns the latest revision; a pinned revision returns exactly that
//! revision or a typed `not_found`; a stale older revision never overwrites
//! a newer projection (the projection is derived, never stored, so
//! "overwriting" is structurally impossible — but the law is also pinned by
//! test).
//!
//! Pull-only for V2 (RULING-205 §8): no durable requester-delivery surface
//! exists here; tachi#1679 stays its own leaf.

use serde::{Deserialize, Serialize};

use super::refs::{AttemptRef, TaskRef};

/// Typed collect failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CollectError {
    /// The task does not exist.
    #[error("task not found")]
    NotFound,
    /// No result projection exists yet (nothing terminal to collect).
    #[error("result not ready")]
    NotReady,
    /// The pinned result revision does not exist.
    #[error("result revision not found")]
    ResultRevisionNotFound,
    /// The bridge truth source is unavailable (TB-20).
    #[error("bridge unavailable")]
    Unavailable,
}

/// The artifact/evidence-first result projection (TB-13 field list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultProjectionV1 {
    /// 1) Task identity.
    pub task_ref: TaskRef,
    /// 2) The attempt this result terminates (typed over existing
    ///    `dispatch_outcomes` truth).
    pub attempt_ref: AttemptRef,
    /// 3) Machine-resolved terminal classification (never the worker's
    ///    self-report).
    pub terminal_classification: String,
    /// 4) Canonical result artifact ref, when one exists.
    pub canonical_artifact_ref: Option<String>,
    /// 5) Artifact/evidence refs bound to the terminal outcome.
    pub artifact_evidence_refs: Vec<String>,
    /// 6) Verification summary from the outcome's evidence facts.
    pub verification_summary: VerificationSummary,
    /// 7) Evaluation/adjudication state (adjudication dimension mapping).
    pub adjudication_state: super::mapping::adjudication::AdjudicationState,
    /// 8) Contract violations (expected vs observed evidence).
    pub contract_violations: Vec<ContractViolation>,
    /// 9) Provenance projection (vendor/model/identity attribution basis).
    pub provenance: ProvenanceProjection,
    /// 10) Pending user action, if the result waits on a human.
    pub pending_user_action: Option<String>,
    /// 11) Tachi-minted monotonic result revision.
    pub result_revision: u64,
}

/// Verification summary derived from `dispatch_outcomes` evidence facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationSummary {
    /// Whether verification evidence was present on the terminal outcome.
    pub verification_present: bool,
    /// Whether a diff was present on the terminal outcome.
    pub diff_present: bool,
    /// Count of evidence refs bound to the outcome.
    pub evidence_ref_count: usize,
}

/// One contract violation: the intent expected an artifact class that the
/// terminal outcome's evidence does not cover (TB-13: worker prose does not
/// satisfy this).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractViolation {
    /// The unsatisfied artifact class.
    pub artifact_class: super::wire::ArtifactClass,
    /// Machine-checkable statement of the violation.
    pub violation: String,
}

/// Provenance projection from the outcome row's identity facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceProjection {
    /// Vendor observed at the outcome (or `unknown`).
    pub vendor: String,
    /// Model, if recorded.
    pub model: Option<String>,
    /// Identity attribution basis (`observed`, `planned_unconfirmed`, … —
    /// the frozen #1065 option D basis vocabulary).
    pub identity_attribution_basis: String,
    /// The raw self-reported outcome kept verbatim for audit (never the
    /// canonical verdict).
    pub reported_outcome: Option<String>,
}

/// Compute contract violations: required expected artifacts vs the
/// terminal outcome's evidence facts. A worker `success` with a required
/// artifact missing yields a violation — this is the TB-13 "success without
/// required artifact" intercept.
pub fn contract_violations(
    expected: &[super::events::ExpectedArtifactProjection],
    evidence: &VerificationSummary,
) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    for artifact in expected.iter().filter(|a| a.required) {
        let satisfied = match artifact.artifact_class {
            super::wire::ArtifactClass::Report => evidence.evidence_ref_count > 0,
            super::wire::ArtifactClass::Diff => evidence.diff_present,
            super::wire::ArtifactClass::VerificationLog => evidence.verification_present,
        };
        if !satisfied {
            violations.push(ContractViolation {
                artifact_class: artifact.artifact_class,
                violation: format!(
                    "required {:?} artifact absent from terminal evidence; worker self-report does not satisfy the contract",
                    artifact.artifact_class
                ),
            });
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taskintent::events::ExpectedArtifactProjection;
    use crate::taskintent::wire::ArtifactClass;

    fn evidence(verification: bool, diff: bool, refs: usize) -> VerificationSummary {
        VerificationSummary {
            verification_present: verification,
            diff_present: diff,
            evidence_ref_count: refs,
        }
    }

    #[test]
    fn success_without_required_artifact_violates_the_contract() {
        // Owner vertical test 7: missing required artifact fails the
        // evaluation contract regardless of worker prose.
        let expected = [ExpectedArtifactProjection {
            artifact_class: ArtifactClass::VerificationLog,
            required: true,
        }];
        let violations = contract_violations(&expected, &evidence(false, false, 0));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].artifact_class, ArtifactClass::VerificationLog);
    }

    #[test]
    fn required_artifacts_satisfied_by_evidence_do_not_violate() {
        let expected = [
            ExpectedArtifactProjection {
                artifact_class: ArtifactClass::Diff,
                required: true,
            },
            ExpectedArtifactProjection {
                artifact_class: ArtifactClass::Report,
                required: false,
            },
        ];
        assert!(contract_violations(&expected, &evidence(false, true, 1)).is_empty());
        // Optional artifacts never violate.
        let optional = [ExpectedArtifactProjection {
            artifact_class: ArtifactClass::VerificationLog,
            required: false,
        }];
        assert!(contract_violations(&optional, &evidence(false, false, 0)).is_empty());
    }
}
