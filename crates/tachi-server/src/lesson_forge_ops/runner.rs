//! Spend-gated execution seam for the #1073 pilot.
//!
//! The runner checks manifest membership, resolves and verifies the complete
//! source binding, and only then calls an injected producer.  The injected
//! traits make the complete 50-row flow testable without a live engine,
//! credentials, Vault access, or spend.

use serde::Serialize;
use sha2::{Digest, Sha256};
use tachi_params::{
    EvidenceRefV1, EvidenceRelationV1, ImmutableRevisionV1, LessonCandidateV1, SourceKindV1,
};

use super::discrimination::{
    blind_case, evaluate_case, unblind_scores, AdjudicatorReceipt, BlindedCase, CaseOutcome,
    ColdRunScore, ColdRunText,
};
use super::forge::{forge_lesson_candidate, ForgeDraft, SourceBundle};
use super::pilot::{
    DurablePilotManifestV1, PilotManifestV1, PilotRowV1, PilotSourceRouteV1, PILOT_SIZE,
};
use super::privacy::{screen_source_for_public_pilot, PilotPrivacyErrorV1};
use super::progress::{
    PilotCallArmV1, PilotCallKeyV1, PilotCallRoleV1, PilotCallStateV1, PilotEngineReceiptV1,
    PilotProgressErrorV1, PilotProgressLedgerV1,
};
use super::source::{
    verify_resolved_source_v1, PilotSourceResolverV1, ResolvedPilotSourceV1, SourceResolveError,
};

pub const PILOT_CALLS_PER_CASE_V1: usize = 1 + 6 + 1;
pub const PILOT_COMPLETED_CALLS_V1: usize = PILOT_SIZE * PILOT_CALLS_PER_CASE_V1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotCaseKeyV1 {
    pub source_route: PilotSourceRouteV1,
    pub source_id: String,
    pub source_revision: i64,
}

#[derive(Debug, Clone)]
pub struct ProducedDraftV1 {
    pub draft: ForgeDraft,
    pub receipt: PilotEngineReceiptV1,
}

#[derive(Debug, Clone)]
pub struct ColdRunResultV1 {
    /// Kept in-process for blind adjudication only; it is never reportable.
    pub text: String,
    pub receipt: PilotEngineReceiptV1,
}

#[derive(Debug, Clone)]
pub struct AdjudicationResultV1 {
    pub scores: Vec<(String, ColdRunScore)>,
    pub receipt: PilotEngineReceiptV1,
}

/// Categorical stage failure only. No provider/model text can enter errors or
/// the durable ledger. `retry_safe` may be true only when the implementation
/// knows no billable call was accepted.
#[derive(Debug, Clone)]
pub struct PilotCallErrorV1 {
    pub receipt: Option<Box<PilotEngineReceiptV1>>,
    pub retry_safe: bool,
    pub failure_code: &'static str,
}

pub trait PilotProducerV1 {
    fn produce(
        &mut self,
        binding: &PilotRowV1,
        source: &ResolvedPilotSourceV1,
    ) -> Result<ProducedDraftV1, PilotCallErrorV1>;

    /// Recover output for an already completed call without model spend.
    fn recover_produced(
        &mut self,
        binding: &PilotRowV1,
        source: &ResolvedPilotSourceV1,
    ) -> Result<ProducedDraftV1, PilotCallErrorV1>;
}

pub trait PilotColdRunnerV1 {
    fn run_treated(
        &mut self,
        binding: &PilotRowV1,
        candidate: &LessonCandidateV1,
        invocation: usize,
    ) -> Result<ColdRunResultV1, PilotCallErrorV1>;

    fn run_baseline(
        &mut self,
        binding: &PilotRowV1,
        invocation: usize,
    ) -> Result<ColdRunResultV1, PilotCallErrorV1>;

    /// Recover output for an attested completed call without model spend.
    fn recover_treated(
        &mut self,
        binding: &PilotRowV1,
        candidate: &LessonCandidateV1,
        invocation: usize,
    ) -> Result<ColdRunResultV1, PilotCallErrorV1>;

    fn recover_baseline(
        &mut self,
        binding: &PilotRowV1,
        invocation: usize,
    ) -> Result<ColdRunResultV1, PilotCallErrorV1>;
}

pub trait PilotAdjudicatorV1 {
    fn adjudicate(
        &mut self,
        binding: &PilotRowV1,
        blinded: &BlindedCase,
    ) -> Result<AdjudicationResultV1, PilotCallErrorV1>;

    /// Recover output for an attested completed call without model spend.
    fn recover_adjudication(
        &mut self,
        binding: &PilotRowV1,
        blinded: &BlindedCase,
    ) -> Result<AdjudicationResultV1, PilotCallErrorV1>;
}

#[derive(Debug)]
pub enum PilotRunError {
    NotInManifest(PilotCaseKeyV1),
    Source(SourceResolveError),
    Privacy(PilotPrivacyErrorV1),
    Progress(PilotProgressErrorV1),
    RecordedIndeterminateCall {
        source_id: String,
        stage: &'static str,
    },
    ReceiptNotAttested {
        source_id: String,
        stage: &'static str,
    },
    RecoveryMismatch {
        source_id: String,
        stage: &'static str,
    },
    StageFailed {
        source_id: String,
        stage: &'static str,
    },
    Forge(String),
    Unblind(String),
    ReportAccounting(PilotRunReportErrorV1),
}

impl std::fmt::Display for PilotRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInManifest(key) => write!(
                f,
                "{} source {}@{} is not in the frozen pilot manifest",
                key.source_route.as_str(),
                key.source_id,
                key.source_revision
            ),
            Self::Source(err) => err.fmt(f),
            Self::Privacy(err) => err.fmt(f),
            Self::Progress(err) => err.fmt(f),
            Self::RecordedIndeterminateCall { source_id, stage } => write!(
                f,
                "pilot {stage} call for source {source_id} has indeterminate spend state"
            ),
            Self::ReceiptNotAttested { source_id, stage } => write!(
                f,
                "pilot {stage} receipt for source {source_id} is preview-only or lacks complete accounting"
            ),
            Self::RecoveryMismatch { source_id, stage } => write!(
                f,
                "pilot {stage} recovery for source {source_id} did not match its ledger attestation"
            ),
            Self::StageFailed { source_id, stage } => {
                write!(f, "pilot {stage} stage failed for source {source_id}")
            }
            Self::Forge(message) => write!(f, "pilot forge gate refused candidate: {message}"),
            Self::Unblind(message) => write!(
                f,
                "pilot blind adjudication could not be verified: {message}"
            ),
            Self::ReportAccounting(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PilotRunError {}

/// Public report rows deliberately contain a binding, result, and receipts
/// but no `SourceBundle`, draft, cold-run text, or adjudicator text.
#[derive(Debug, Clone, Serialize)]
pub struct PilotRunCaseReportV1 {
    pub binding: PilotRowV1,
    pub outcome: CaseOutcome,
    pub producer_receipt: PilotEngineReceiptV1,
    pub treated_receipts: [PilotEngineReceiptV1; 3],
    pub baseline_receipts: [PilotEngineReceiptV1; 3],
    pub adjudicator_receipt: PilotEngineReceiptV1,
}

#[derive(Debug, Clone, Serialize)]
pub struct PilotRunReportV1 {
    manifest_digest: String,
    cases: Vec<PilotRunCaseReportV1>,
    completed_calls: Vec<PilotRunCallReportV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PilotRunCallReportV1 {
    pub key: PilotCallKeyV1,
    pub receipt: PilotEngineReceiptV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PilotRunReportErrorV1 {
    WrongCaseCount { expected: usize, actual: usize },
    WrongCompletedCallCount { expected: usize, actual: usize },
    DuplicateCallKey,
    UnexpectedOrMissingCall,
    IncompleteAccounting,
    ReceiptMismatch,
}

impl std::fmt::Display for PilotRunReportErrorV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCaseCount { expected, actual } => {
                write!(f, "pilot run report requires {expected} cases, got {actual}")
            }
            Self::WrongCompletedCallCount { expected, actual } => write!(
                f,
                "pilot run report requires {expected} completed calls, got {actual}"
            ),
            Self::DuplicateCallKey => write!(f, "pilot run report has a duplicate call key"),
            Self::UnexpectedOrMissingCall => write!(
                f,
                "pilot run report call keys do not exactly cover the frozen case matrix"
            ),
            Self::IncompleteAccounting => write!(
                f,
                "pilot run report contains a call without complete token, cost, and latency accounting"
            ),
            Self::ReceiptMismatch => write!(
                f,
                "pilot run report call receipt does not match its per-case receipt"
            ),
        }
    }
}

impl PilotRunReportV1 {
    fn from_cases(
        manifest_digest: String,
        cases: Vec<PilotRunCaseReportV1>,
    ) -> Result<Self, PilotRunReportErrorV1> {
        let completed_calls = expected_call_reports(&manifest_digest, &cases);
        let report = Self {
            manifest_digest,
            cases,
            completed_calls,
        };
        report.validate_complete_accounting()?;
        Ok(report)
    }

    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    pub fn cases(&self) -> &[PilotRunCaseReportV1] {
        &self.cases
    }

    pub fn completed_calls(&self) -> &[PilotRunCallReportV1] {
        &self.completed_calls
    }

    pub fn passed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|case| matches!(case.outcome, CaseOutcome::Pass))
            .count()
    }

    pub fn privacy_safe_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn validate_complete_accounting(&self) -> Result<(), PilotRunReportErrorV1> {
        if self.cases.len() != PILOT_SIZE {
            return Err(PilotRunReportErrorV1::WrongCaseCount {
                expected: PILOT_SIZE,
                actual: self.cases.len(),
            });
        }
        if self.completed_calls.len() != PILOT_COMPLETED_CALLS_V1 {
            return Err(PilotRunReportErrorV1::WrongCompletedCallCount {
                expected: PILOT_COMPLETED_CALLS_V1,
                actual: self.completed_calls.len(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        if self
            .completed_calls
            .iter()
            .any(|call| !seen.insert(call.key.clone()))
        {
            return Err(PilotRunReportErrorV1::DuplicateCallKey);
        }
        let expected = expected_call_reports(&self.manifest_digest, &self.cases);
        if expected
            .iter()
            .any(|call| !call.receipt.is_fully_attested())
            || self
                .completed_calls
                .iter()
                .any(|call| !call.receipt.is_fully_attested())
        {
            return Err(PilotRunReportErrorV1::IncompleteAccounting);
        }
        for expected_call in &expected {
            let Some(actual) = self
                .completed_calls
                .iter()
                .find(|call| call.key == expected_call.key)
            else {
                return Err(PilotRunReportErrorV1::UnexpectedOrMissingCall);
            };
            if actual.receipt != expected_call.receipt {
                return Err(PilotRunReportErrorV1::ReceiptMismatch);
            }
        }
        Ok(())
    }
}

fn expected_call_reports(
    manifest_digest: &str,
    cases: &[PilotRunCaseReportV1],
) -> Vec<PilotRunCallReportV1> {
    let mut calls = Vec::with_capacity(cases.len() * PILOT_CALLS_PER_CASE_V1);
    for case in cases {
        let mut push = |role, arm, ordinal, receipt: &PilotEngineReceiptV1| {
            calls.push(PilotRunCallReportV1 {
                key: PilotCallKeyV1::new(manifest_digest, &case.binding, role, arm, ordinal),
                receipt: receipt.clone(),
            });
        };
        push(
            PilotCallRoleV1::Producer,
            PilotCallArmV1::None,
            0,
            &case.producer_receipt,
        );
        for (ordinal, receipt) in case.treated_receipts.iter().enumerate() {
            push(
                PilotCallRoleV1::ColdRun,
                PilotCallArmV1::Treated,
                ordinal as u8,
                receipt,
            );
        }
        for (ordinal, receipt) in case.baseline_receipts.iter().enumerate() {
            push(
                PilotCallRoleV1::ColdRun,
                PilotCallArmV1::Baseline,
                ordinal as u8,
                receipt,
            );
        }
        push(
            PilotCallRoleV1::Adjudicator,
            PilotCallArmV1::Blinded,
            0,
            &case.adjudicator_receipt,
        );
    }
    calls
}

/// Execute the exact frozen 50-row flow.  This remains generic over all model
/// seams so tests can execute 50 producer + 300 cold + 50 adjudicator calls
/// synthetically, while a phase-2 caller can install a spend-authorized
/// implementation without changing the gates.
pub fn run_manifest<R, P, C, A>(
    manifest: &DurablePilotManifestV1,
    progress_path: impl AsRef<std::path::Path>,
    resolver: &R,
    producer: &mut P,
    cold_runner: &mut C,
    adjudicator: &mut A,
) -> Result<PilotRunReportV1, PilotRunError>
where
    R: PilotSourceResolverV1,
    P: PilotProducerV1,
    C: PilotColdRunnerV1,
    A: PilotAdjudicatorV1,
{
    let digest = manifest
        .contract_digest()
        .map_err(|_| stage_error_for_id("manifest", "digest"))?;
    let mut ledger =
        PilotProgressLedgerV1::open(progress_path, &digest).map_err(PilotRunError::Progress)?;
    run_manifest_inner(
        manifest.manifest(),
        &digest,
        &mut ledger,
        resolver,
        producer,
        cold_runner,
        adjudicator,
    )
}

fn run_manifest_inner<R, P, C, A>(
    manifest: &PilotManifestV1,
    digest: &str,
    ledger: &mut PilotProgressLedgerV1,
    resolver: &R,
    producer: &mut P,
    cold_runner: &mut C,
    adjudicator: &mut A,
) -> Result<PilotRunReportV1, PilotRunError>
where
    R: PilotSourceResolverV1,
    P: PilotProducerV1,
    C: PilotColdRunnerV1,
    A: PilotAdjudicatorV1,
{
    let keys: Vec<PilotCaseKeyV1> = manifest
        .rows()
        .iter()
        .map(|row| PilotCaseKeyV1 {
            source_route: row.source_route,
            source_id: row.source_id.clone(),
            source_revision: row.source_revision,
        })
        .collect();
    let mut cases = Vec::with_capacity(keys.len());
    for key in keys {
        cases.push(run_case_inner(
            manifest,
            digest,
            ledger,
            key,
            resolver,
            producer,
            cold_runner,
            adjudicator,
        )?);
    }
    PilotRunReportV1::from_cases(digest.to_string(), cases).map_err(PilotRunError::ReportAccounting)
}

/// Explicit synthetic seam: tests may exercise the complete runner from an
/// in-memory freeze, while the production API above requires durable origin.
#[cfg(test)]
fn run_manifest_for_test<R, P, C, A>(
    manifest: &PilotManifestV1,
    progress_path: impl AsRef<std::path::Path>,
    resolver: &R,
    producer: &mut P,
    cold_runner: &mut C,
    adjudicator: &mut A,
) -> Result<PilotRunReportV1, PilotRunError>
where
    R: PilotSourceResolverV1,
    P: PilotProducerV1,
    C: PilotColdRunnerV1,
    A: PilotAdjudicatorV1,
{
    let digest = manifest
        .contract_digest()
        .map_err(|_| stage_error_for_id("manifest", "digest"))?;
    let mut ledger =
        PilotProgressLedgerV1::open(progress_path, &digest).map_err(PilotRunError::Progress)?;
    run_manifest_inner(
        manifest,
        &digest,
        &mut ledger,
        resolver,
        producer,
        cold_runner,
        adjudicator,
    )
}

/// Execute one case only after an exact route/id/revision membership check and
/// a full-content source verification.  No producer call appears before those
/// two gates.
pub fn run_case<R, P, C, A>(
    manifest: &DurablePilotManifestV1,
    progress_path: impl AsRef<std::path::Path>,
    key: PilotCaseKeyV1,
    resolver: &R,
    producer: &mut P,
    cold_runner: &mut C,
    adjudicator: &mut A,
) -> Result<PilotRunCaseReportV1, PilotRunError>
where
    R: PilotSourceResolverV1,
    P: PilotProducerV1,
    C: PilotColdRunnerV1,
    A: PilotAdjudicatorV1,
{
    let digest = manifest
        .contract_digest()
        .map_err(|_| stage_error_for_id("manifest", "digest"))?;
    let mut ledger =
        PilotProgressLedgerV1::open(progress_path, &digest).map_err(PilotRunError::Progress)?;
    run_case_inner(
        manifest.manifest(),
        &digest,
        &mut ledger,
        key,
        resolver,
        producer,
        cold_runner,
        adjudicator,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_case_inner<R, P, C, A>(
    manifest: &PilotManifestV1,
    digest: &str,
    ledger: &mut PilotProgressLedgerV1,
    key: PilotCaseKeyV1,
    resolver: &R,
    producer: &mut P,
    cold_runner: &mut C,
    adjudicator: &mut A,
) -> Result<PilotRunCaseReportV1, PilotRunError>
where
    R: PilotSourceResolverV1,
    P: PilotProducerV1,
    C: PilotColdRunnerV1,
    A: PilotAdjudicatorV1,
{
    let binding = manifest
        .find_binding(key.source_route, &key.source_id, key.source_revision)
        .cloned()
        .ok_or(PilotRunError::NotInManifest(key))?;
    let source = resolver
        .resolve_verified(&binding)
        .map_err(PilotRunError::Source)?;
    verify_resolved_source_v1(&binding, &source).map_err(PilotRunError::Source)?;
    screen_source_for_public_pilot(&source.full_text).map_err(PilotRunError::Privacy)?;
    let produced = run_producer(digest, ledger, &binding, &source, producer)?;
    let candidate = forge_lesson_candidate(
        "lesson-forge-pilot",
        manifest,
        &source_bundle(&binding, source),
        binding.target_kind,
        &produced.draft,
        Some(produced.receipt.identity.clone()),
    )
    .map_err(|err| PilotRunError::Forge(err.to_string()))?;

    let (treated, treated_receipts) =
        run_treated(digest, ledger, &binding, &candidate, cold_runner)?;
    let (baseline, baseline_receipts) = run_baseline(digest, ledger, &binding, cold_runner)?;
    let (blinded, key) = blind_case(&binding.source_id, treated, baseline, blind_seed(&binding));
    let adjudicated = run_adjudicator(digest, ledger, &binding, &blinded, adjudicator)?;
    let (treated, baseline) = unblind_scores(&key, adjudicated.scores)
        .map_err(|err| PilotRunError::Unblind(format!("{err:?}")))?;
    let outcome = evaluate_case(&super::discrimination::CaseInput {
        case_id: binding.source_id.clone(),
        treated,
        baseline,
        candidate_cites_source_refs: candidate.cites_source_refs(),
        candidate_claims_establishment: candidate.claims_establishment(),
        producer_receipt: Some(produced.receipt.identity.clone()),
        adjudicator_receipt: AdjudicatorReceipt::clone(&adjudicated.receipt.identity),
    });

    Ok(PilotRunCaseReportV1 {
        binding,
        outcome,
        producer_receipt: produced.receipt,
        treated_receipts,
        baseline_receipts,
        adjudicator_receipt: adjudicated.receipt,
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_case_for_test<R, P, C, A>(
    manifest: &PilotManifestV1,
    progress_path: impl AsRef<std::path::Path>,
    key: PilotCaseKeyV1,
    resolver: &R,
    producer: &mut P,
    cold_runner: &mut C,
    adjudicator: &mut A,
) -> Result<PilotRunCaseReportV1, PilotRunError>
where
    R: PilotSourceResolverV1,
    P: PilotProducerV1,
    C: PilotColdRunnerV1,
    A: PilotAdjudicatorV1,
{
    let digest = manifest
        .contract_digest()
        .map_err(|_| stage_error_for_id("manifest", "digest"))?;
    let mut ledger =
        PilotProgressLedgerV1::open(progress_path, &digest).map_err(PilotRunError::Progress)?;
    run_case_inner(
        manifest,
        &digest,
        &mut ledger,
        key,
        resolver,
        producer,
        cold_runner,
        adjudicator,
    )
}

fn source_bundle(binding: &PilotRowV1, source: ResolvedPilotSourceV1) -> SourceBundle {
    SourceBundle {
        source_route: binding.source_route,
        row_id: binding.source_id.clone(),
        revision: binding.source_revision,
        full_text: source.full_text,
        refs: vec![EvidenceRefV1 {
            relation: EvidenceRelationV1::DerivedFrom,
            target_kind: SourceKindV1::EpisodicMemory,
            target_ref: format!("{}:{}", binding.source_route.as_str(), binding.source_id),
            immutable_revision: ImmutableRevisionV1::MemoryRevision(
                binding.source_revision.to_string(),
            ),
            section_or_span: None,
            captured_at: binding.capture_timestamp.clone(),
        }],
    }
}

fn run_producer<P: PilotProducerV1>(
    digest: &str,
    ledger: &mut PilotProgressLedgerV1,
    binding: &PilotRowV1,
    source: &ResolvedPilotSourceV1,
    producer: &mut P,
) -> Result<ProducedDraftV1, PilotRunError> {
    let key = PilotCallKeyV1::new(
        digest,
        binding,
        PilotCallRoleV1::Producer,
        PilotCallArmV1::None,
        0,
    );
    execute_attested(
        ledger,
        key,
        binding,
        "producer",
        |recover| {
            if recover {
                producer.recover_produced(binding, source)
            } else {
                producer.produce(binding, source)
            }
        },
        |result| draft_digest(&result.draft),
        |result| &result.receipt,
    )
}

fn run_treated<C: PilotColdRunnerV1>(
    digest: &str,
    ledger: &mut PilotProgressLedgerV1,
    binding: &PilotRowV1,
    candidate: &LessonCandidateV1,
    runner: &mut C,
) -> Result<([ColdRunText; 3], [PilotEngineReceiptV1; 3]), PilotRunError> {
    let mut call = |invocation: usize| {
        let key = PilotCallKeyV1::new(
            digest,
            binding,
            PilotCallRoleV1::ColdRun,
            PilotCallArmV1::Treated,
            invocation as u8,
        );
        execute_attested(
            ledger,
            key,
            binding,
            "treated-cold",
            |recover| {
                if recover {
                    runner.recover_treated(binding, candidate, invocation)
                } else {
                    runner.run_treated(binding, candidate, invocation)
                }
            },
            |result| text_digest(&result.text),
            |result| &result.receipt,
        )
    };
    let one = call(0)?;
    let two = call(1)?;
    let three = call(2)?;
    Ok((
        [
            ColdRunText { text: one.text },
            ColdRunText { text: two.text },
            ColdRunText { text: three.text },
        ],
        [one.receipt, two.receipt, three.receipt],
    ))
}

fn run_baseline<C: PilotColdRunnerV1>(
    digest: &str,
    ledger: &mut PilotProgressLedgerV1,
    binding: &PilotRowV1,
    runner: &mut C,
) -> Result<([ColdRunText; 3], [PilotEngineReceiptV1; 3]), PilotRunError> {
    let mut call = |invocation: usize| {
        let key = PilotCallKeyV1::new(
            digest,
            binding,
            PilotCallRoleV1::ColdRun,
            PilotCallArmV1::Baseline,
            invocation as u8,
        );
        execute_attested(
            ledger,
            key,
            binding,
            "baseline-cold",
            |recover| {
                if recover {
                    runner.recover_baseline(binding, invocation)
                } else {
                    runner.run_baseline(binding, invocation)
                }
            },
            |result| text_digest(&result.text),
            |result| &result.receipt,
        )
    };
    let one = call(0)?;
    let two = call(1)?;
    let three = call(2)?;
    Ok((
        [
            ColdRunText { text: one.text },
            ColdRunText { text: two.text },
            ColdRunText { text: three.text },
        ],
        [one.receipt, two.receipt, three.receipt],
    ))
}

fn run_adjudicator<A: PilotAdjudicatorV1>(
    digest: &str,
    ledger: &mut PilotProgressLedgerV1,
    binding: &PilotRowV1,
    blinded: &BlindedCase,
    adjudicator: &mut A,
) -> Result<AdjudicationResultV1, PilotRunError> {
    let key = PilotCallKeyV1::new(
        digest,
        binding,
        PilotCallRoleV1::Adjudicator,
        PilotCallArmV1::Blinded,
        0,
    );
    execute_attested(
        ledger,
        key,
        binding,
        "adjudicator",
        |recover| {
            if recover {
                adjudicator.recover_adjudication(binding, blinded)
            } else {
                adjudicator.adjudicate(binding, blinded)
            }
        },
        |result| scores_digest(&result.scores),
        |result| &result.receipt,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_attested<T>(
    ledger: &mut PilotProgressLedgerV1,
    key: PilotCallKeyV1,
    binding: &PilotRowV1,
    stage: &'static str,
    call: impl FnOnce(bool) -> Result<T, PilotCallErrorV1>,
    output_digest: impl Fn(&T) -> String,
    receipt: impl Fn(&T) -> &PilotEngineReceiptV1,
) -> Result<T, PilotRunError> {
    match ledger.get(&key).cloned() {
        Some(PilotCallStateV1::Completed {
            receipt: expected_receipt,
            output_sha256,
        }) => {
            if !expected_receipt.is_fully_attested() {
                ledger
                    .record(
                        key,
                        PilotCallStateV1::Failed {
                            receipt: Some(expected_receipt),
                            failure_code: "receipt_not_attested".to_string(),
                            retry_safe: false,
                        },
                    )
                    .map_err(PilotRunError::Progress)?;
                return Err(PilotRunError::ReceiptNotAttested {
                    source_id: binding.source_id.clone(),
                    stage,
                });
            }
            let recovered = call(true).map_err(|_| PilotRunError::RecoveryMismatch {
                source_id: binding.source_id.clone(),
                stage,
            })?;
            if receipt(&recovered) != &expected_receipt
                || output_digest(&recovered) != output_sha256
            {
                return Err(PilotRunError::RecoveryMismatch {
                    source_id: binding.source_id.clone(),
                    stage,
                });
            }
            return Ok(recovered);
        }
        Some(PilotCallStateV1::Started)
        | Some(PilotCallStateV1::Failed {
            retry_safe: false, ..
        }) => {
            return Err(PilotRunError::RecordedIndeterminateCall {
                source_id: binding.source_id.clone(),
                stage,
            });
        }
        Some(PilotCallStateV1::Failed {
            retry_safe: true, ..
        })
        | None => {}
    }

    ledger
        .record(key.clone(), PilotCallStateV1::Started)
        .map_err(PilotRunError::Progress)?;
    let result = match call(false) {
        Ok(result) => result,
        Err(error) => {
            ledger
                .record(
                    key,
                    PilotCallStateV1::Failed {
                        receipt: error.receipt.map(|receipt| *receipt),
                        failure_code: error.failure_code.to_string(),
                        retry_safe: error.retry_safe,
                    },
                )
                .map_err(PilotRunError::Progress)?;
            return Err(stage_error(binding, stage));
        }
    };
    let actual_receipt = receipt(&result).clone();
    if !actual_receipt.is_fully_attested() {
        ledger
            .record(
                key,
                PilotCallStateV1::Failed {
                    receipt: Some(actual_receipt),
                    failure_code: "receipt_not_attested".to_string(),
                    retry_safe: false,
                },
            )
            .map_err(PilotRunError::Progress)?;
        return Err(PilotRunError::ReceiptNotAttested {
            source_id: binding.source_id.clone(),
            stage,
        });
    }
    ledger
        .record(
            key,
            PilotCallStateV1::Completed {
                receipt: actual_receipt,
                output_sha256: output_digest(&result),
            },
        )
        .map_err(PilotRunError::Progress)?;
    Ok(result)
}

fn draft_digest(draft: &ForgeDraft) -> String {
    digest_fields([
        draft.situation.as_str(),
        draft.proposed_ruling.as_str(),
        draft.why.as_str(),
        draft.how_to_apply.as_str(),
    ])
}

fn text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn scores_digest(scores: &[(String, ColdRunScore)]) -> String {
    let fields: Vec<String> = scores
        .iter()
        .map(|(blind_id, score)| {
            format!(
                "{}:{}:{}:{}",
                blind_id.len(),
                blind_id,
                u8::from(score.matches_reference_decision),
                score.unsupported_claims
            )
        })
        .collect();
    digest_fields(fields.iter().map(String::as_str))
}

fn digest_fields<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update(field.len().to_be_bytes());
        hasher.update(field.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn stage_error(binding: &PilotRowV1, stage: &'static str) -> PilotRunError {
    stage_error_for_id(&binding.source_id, stage)
}

fn stage_error_for_id(source_id: &str, stage: &'static str) -> PilotRunError {
    PilotRunError::StageFailed {
        source_id: source_id.to_string(),
        stage,
    }
}

fn blind_seed(binding: &PilotRowV1) -> u64 {
    let digest = Sha256::digest(
        format!(
            "{}\0{}\0{}",
            binding.source_route.as_str(),
            binding.source_id,
            binding.source_revision
        )
        .as_bytes(),
    );
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("sha256 prefix has eight bytes"),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use super::*;
    use crate::lesson_forge_ops::pilot::{
        freeze_pilot_manifest, PilotRowKindV1, PilotSourceRouteV1, PilotStratumV1,
    };
    use tachi_params::LessonCandidateKindV1;

    fn receipt(role: &str, provider: &str, model: &str) -> PilotEngineReceiptV1 {
        PilotEngineReceiptV1 {
            identity: tachi_params::LessonEngineReceiptV1 {
                requested_role: role.to_string(),
                effective_provider: Some(provider.to_string()),
                effective_model: Some(model.to_string()),
                effective_version: Some("test-v1".to_string()),
                fallback_chain: Vec::new(),
                degraded: false,
            },
            usage: Some(super::super::progress::PilotEngineUsageV1 {
                tokens: Some(11),
                cost_usd_micros: Some(17),
                latency_ms: Some(23),
            }),
        }
    }

    fn progress_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("sigil-1073-progress-{}.json", uuid::Uuid::new_v4()))
    }

    fn rows() -> Vec<PilotRowV1> {
        (0..50)
            .map(|index| {
                let text = format!("SYNTHETIC_SOURCE_{index}");
                PilotRowV1 {
                    source_route: if index < 25 {
                        PilotSourceRouteV1::Antigravity
                    } else {
                        PilotSourceRouteV1::Hapi
                    },
                    source_id: format!("source-{index}"),
                    source_revision: 1,
                    content_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
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
                    selection_reason: "public-safe synthetic rationale".to_string(),
                    reference_decision: "public-safe synthetic decision".to_string(),
                    target_kind: LessonCandidateKindV1::Precedent,
                }
            })
            .collect()
    }

    struct Resolver {
        texts: BTreeMap<(PilotSourceRouteV1, String, i64), String>,
    }

    impl Resolver {
        fn from_rows(rows: &[PilotRowV1]) -> Self {
            Self {
                texts: rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        (
                            (row.source_route, row.source_id.clone(), row.source_revision),
                            format!("SYNTHETIC_SOURCE_{index}"),
                        )
                    })
                    .collect(),
            }
        }
    }

    impl PilotSourceResolverV1 for Resolver {
        fn resolve_verified(
            &self,
            binding: &PilotRowV1,
        ) -> Result<ResolvedPilotSourceV1, SourceResolveError> {
            let key = (
                binding.source_route,
                binding.source_id.clone(),
                binding.source_revision,
            );
            let full_text = self.texts.get(&key).cloned().ok_or_else(|| {
                SourceResolveError::MissingExactRevision {
                    source_route: binding.source_route,
                    source_id: binding.source_id.clone(),
                    source_revision: binding.source_revision,
                }
            })?;
            let resolved = ResolvedPilotSourceV1 {
                source_route: binding.source_route,
                source_id: binding.source_id.clone(),
                source_revision: binding.source_revision,
                full_text,
            };
            if resolved.content_sha256() != binding.content_sha256 {
                return Err(SourceResolveError::DigestMismatch {
                    source_route: binding.source_route,
                    source_id: binding.source_id.clone(),
                    source_revision: binding.source_revision,
                    expected: binding.content_sha256.clone(),
                    actual: resolved.content_sha256(),
                });
            }
            Ok(resolved)
        }
    }

    #[derive(Default)]
    struct Producer {
        calls: Cell<usize>,
    }

    impl PilotProducerV1 for Producer {
        fn produce(
            &mut self,
            _binding: &PilotRowV1,
            _source: &ResolvedPilotSourceV1,
        ) -> Result<ProducedDraftV1, PilotCallErrorV1> {
            self.calls.set(self.calls.get() + 1);
            self.recover_produced(_binding, _source)
        }

        fn recover_produced(
            &mut self,
            _binding: &PilotRowV1,
            _source: &ResolvedPilotSourceV1,
        ) -> Result<ProducedDraftV1, PilotCallErrorV1> {
            Ok(ProducedDraftV1 {
                draft: ForgeDraft {
                    situation: "synthetic situation".to_string(),
                    proposed_ruling: "synthetic ruling".to_string(),
                    why: "synthetic why".to_string(),
                    how_to_apply: "synthetic apply".to_string(),
                },
                receipt: receipt("producer", "producer-provider", "producer-model"),
            })
        }
    }

    struct MissingAccountingProducer;

    impl PilotProducerV1 for MissingAccountingProducer {
        fn produce(
            &mut self,
            binding: &PilotRowV1,
            source: &ResolvedPilotSourceV1,
        ) -> Result<ProducedDraftV1, PilotCallErrorV1> {
            self.recover_produced(binding, source)
        }

        fn recover_produced(
            &mut self,
            _binding: &PilotRowV1,
            _source: &ResolvedPilotSourceV1,
        ) -> Result<ProducedDraftV1, PilotCallErrorV1> {
            let mut incomplete = receipt("producer", "producer-provider", "producer-model");
            incomplete.usage = None;
            Ok(ProducedDraftV1 {
                draft: ForgeDraft {
                    situation: "synthetic situation".to_string(),
                    proposed_ruling: "synthetic ruling".to_string(),
                    why: "synthetic why".to_string(),
                    how_to_apply: "synthetic apply".to_string(),
                },
                receipt: incomplete,
            })
        }
    }

    struct Cold {
        calls: usize,
        fail_once_at: Option<(String, usize)>,
        failed_once: bool,
        receipt_override: Option<PilotEngineReceiptV1>,
    }

    impl Default for Cold {
        fn default() -> Self {
            Self {
                calls: 0,
                fail_once_at: None,
                failed_once: false,
                receipt_override: None,
            }
        }
    }

    impl PilotColdRunnerV1 for Cold {
        fn run_treated(
            &mut self,
            _binding: &PilotRowV1,
            _candidate: &LessonCandidateV1,
            _invocation: usize,
        ) -> Result<ColdRunResultV1, PilotCallErrorV1> {
            if !self.failed_once
                && self.fail_once_at.as_ref() == Some(&(_binding.source_id.clone(), _invocation))
            {
                self.failed_once = true;
                return Err(PilotCallErrorV1 {
                    receipt: None,
                    retry_safe: true,
                    failure_code: "synthetic_no_spend_failure",
                });
            }
            self.calls += 1;
            self.recover_treated(_binding, _candidate, _invocation)
        }

        fn recover_treated(
            &mut self,
            _binding: &PilotRowV1,
            _candidate: &LessonCandidateV1,
            _invocation: usize,
        ) -> Result<ColdRunResultV1, PilotCallErrorV1> {
            Ok(ColdRunResultV1 {
                text: "treated synthetic result".to_string(),
                receipt: self
                    .receipt_override
                    .clone()
                    .unwrap_or_else(|| receipt("cold", "cold-provider", "cold-model")),
            })
        }
        fn run_baseline(
            &mut self,
            _binding: &PilotRowV1,
            _invocation: usize,
        ) -> Result<ColdRunResultV1, PilotCallErrorV1> {
            self.calls += 1;
            self.recover_baseline(_binding, _invocation)
        }

        fn recover_baseline(
            &mut self,
            _binding: &PilotRowV1,
            _invocation: usize,
        ) -> Result<ColdRunResultV1, PilotCallErrorV1> {
            Ok(ColdRunResultV1 {
                text: "baseline synthetic result".to_string(),
                receipt: self
                    .receipt_override
                    .clone()
                    .unwrap_or_else(|| receipt("cold", "cold-provider", "cold-model")),
            })
        }
    }

    struct Adjudicator {
        calls: usize,
        fail_unknown_spend: bool,
        receipt_override: Option<PilotEngineReceiptV1>,
    }

    impl Default for Adjudicator {
        fn default() -> Self {
            Self {
                calls: 0,
                fail_unknown_spend: false,
                receipt_override: None,
            }
        }
    }

    impl PilotAdjudicatorV1 for Adjudicator {
        fn adjudicate(
            &mut self,
            _binding: &PilotRowV1,
            blinded: &BlindedCase,
        ) -> Result<AdjudicationResultV1, PilotCallErrorV1> {
            self.calls += 1;
            if self.fail_unknown_spend {
                return Err(PilotCallErrorV1 {
                    receipt: Some(Box::new(receipt(
                        "adjudicator",
                        "adjudicator-provider",
                        "adjudicator-model",
                    ))),
                    retry_safe: false,
                    failure_code: "synthetic_spend_unknown",
                });
            }
            self.recover_adjudication(_binding, blinded)
        }

        fn recover_adjudication(
            &mut self,
            _binding: &PilotRowV1,
            blinded: &BlindedCase,
        ) -> Result<AdjudicationResultV1, PilotCallErrorV1> {
            Ok(AdjudicationResultV1 {
                scores: blinded
                    .items
                    .iter()
                    .map(|item| {
                        (
                            item.blind_id.clone(),
                            ColdRunScore {
                                matches_reference_decision: item.text.starts_with("treated"),
                                unsupported_claims: 0,
                            },
                        )
                    })
                    .collect(),
                receipt: self.receipt_override.clone().unwrap_or_else(|| {
                    receipt("adjudicator", "adjudicator-provider", "adjudicator-model")
                }),
            })
        }
    }

    #[test]
    fn exact_fifty_row_synthetic_flow_spends_only_through_injected_seams() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();
        let report = run_manifest_for_test(
            &manifest,
            &progress,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap();
        assert_eq!(report.cases.len(), 50);
        assert_eq!(report.completed_calls.len(), PILOT_COMPLETED_CALLS_V1);
        assert_eq!(PILOT_COMPLETED_CALLS_V1, 50 * (1 + 6 + 1));
        report.validate_complete_accounting().unwrap();
        assert_eq!(report.passed_count(), 50);
        assert_eq!(producer.calls.get(), 50);
        assert_eq!(cold.calls, 300);
        assert_eq!(adjudicator.calls, 50);
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn complete_accounting_requires_tokens_cost_and_latency() {
        let valid = receipt("producer", "provider", "model");
        assert!(valid.is_fully_attested());
        for field in ["tokens", "cost", "latency"] {
            let mut incomplete = valid.clone();
            let usage = incomplete.usage.as_mut().unwrap();
            match field {
                "tokens" => usage.tokens = None,
                "cost" => usage.cost_usd_micros = None,
                "latency" => usage.latency_ms = None,
                _ => unreachable!(),
            }
            assert!(!incomplete.is_fully_attested(), "accepted missing {field}");
        }
    }

    #[test]
    fn incomplete_accounting_is_refused_for_every_call_role() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);

        let mut producer = MissingAccountingProducer;
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let producer_progress = progress_path();
        assert!(run_case_for_test(
            &manifest,
            &producer_progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "source-0".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .is_err());
        let _ = std::fs::remove_file(producer_progress);

        let mut missing_cost = receipt("cold", "cold-provider", "cold-model");
        missing_cost.usage.as_mut().unwrap().cost_usd_micros = None;
        let mut producer = Producer::default();
        let mut cold = Cold {
            receipt_override: Some(missing_cost),
            ..Cold::default()
        };
        let mut adjudicator = Adjudicator::default();
        let cold_progress = progress_path();
        assert!(run_case_for_test(
            &manifest,
            &cold_progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "source-0".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .is_err());
        let _ = std::fs::remove_file(cold_progress);

        let mut missing_latency =
            receipt("adjudicator", "adjudicator-provider", "adjudicator-model");
        missing_latency.usage.as_mut().unwrap().latency_ms = None;
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator {
            receipt_override: Some(missing_latency),
            ..Adjudicator::default()
        };
        let adjudicator_progress = progress_path();
        assert!(run_case_for_test(
            &manifest,
            &adjudicator_progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "source-0".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .is_err());
        let _ = std::fs::remove_file(adjudicator_progress);
    }

    #[test]
    fn report_rejects_inexact_duplicate_unaccounted_and_incomplete_calls() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();
        let report = run_manifest_for_test(
            &manifest,
            &progress,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap();
        let _ = std::fs::remove_file(progress);

        let mut fewer = report.clone();
        fewer.completed_calls.pop();
        assert!(matches!(
            fewer.validate_complete_accounting(),
            Err(PilotRunReportErrorV1::WrongCompletedCallCount { actual: 399, .. })
        ));

        let mut more = report.clone();
        more.completed_calls.push(report.completed_calls[0].clone());
        assert!(matches!(
            more.validate_complete_accounting(),
            Err(PilotRunReportErrorV1::WrongCompletedCallCount { actual: 401, .. })
        ));

        let mut duplicate = report.clone();
        duplicate.completed_calls[1] = duplicate.completed_calls[0].clone();
        assert!(matches!(
            duplicate.validate_complete_accounting(),
            Err(PilotRunReportErrorV1::DuplicateCallKey)
        ));

        let mut unaccounted = report.clone();
        unaccounted.completed_calls[0].key.ordinal = 9;
        assert!(matches!(
            unaccounted.validate_complete_accounting(),
            Err(PilotRunReportErrorV1::UnexpectedOrMissingCall)
        ));

        let mut incomplete = report.clone();
        incomplete.completed_calls[0]
            .receipt
            .usage
            .as_mut()
            .unwrap()
            .tokens = None;
        assert!(matches!(
            incomplete.validate_complete_accounting(),
            Err(PilotRunReportErrorV1::IncompleteAccounting)
        ));

        let mut mismatched = report;
        mismatched.completed_calls[0]
            .receipt
            .usage
            .as_mut()
            .unwrap()
            .tokens = Some(12);
        assert!(matches!(
            mismatched.validate_complete_accounting(),
            Err(PilotRunReportErrorV1::ReceiptMismatch)
        ));
    }

    #[test]
    fn identity_only_producer_receipt_cannot_complete_a_case() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = MissingAccountingProducer;
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();
        let result = run_case_for_test(
            &manifest,
            &progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "source-0".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        );
        let ledger_json = std::fs::read_to_string(&progress).unwrap();
        let _ = std::fs::remove_file(progress);
        assert!(result.is_err(), "missing accounting must refuse completion");
        assert!(ledger_json.contains("receipt_not_attested"));
        assert!(!ledger_json.contains("\"status\": \"completed\""));
    }

    #[test]
    fn partial_full_run_restarts_without_double_spending_completed_calls() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = Producer::default();
        let mut cold = Cold {
            fail_once_at: Some(("source-0".to_string(), 1)),
            ..Cold::default()
        };
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();

        let first = run_manifest_for_test(
            &manifest,
            &progress,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        );
        assert!(matches!(first, Err(PilotRunError::StageFailed { .. })));
        assert_eq!(producer.calls.get(), 1);
        assert_eq!(cold.calls, 1);
        assert_eq!(adjudicator.calls, 0);

        let report = run_manifest_for_test(
            &manifest,
            &progress,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .expect("retry-safe failure resumes");
        assert_eq!(report.cases.len(), 50);
        assert_eq!(producer.calls.get(), 50, "completed producer was recovered");
        assert_eq!(cold.calls, 300, "completed cold run was recovered");
        assert_eq!(adjudicator.calls, 50);
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn unknown_spend_failure_receipt_is_preserved_and_retry_is_blocked() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator {
            fail_unknown_spend: true,
            ..Adjudicator::default()
        };
        let progress = progress_path();
        let case = PilotCaseKeyV1 {
            source_route: PilotSourceRouteV1::Antigravity,
            source_id: "source-0".to_string(),
            source_revision: 1,
        };

        let first = run_case_for_test(
            &manifest,
            &progress,
            case.clone(),
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        );
        assert!(matches!(first, Err(PilotRunError::StageFailed { .. })));
        let ledger_json = std::fs::read_to_string(&progress).unwrap();
        assert!(ledger_json.contains("synthetic_spend_unknown"));
        assert!(ledger_json.contains("adjudicator-model"));
        assert!(!ledger_json.contains("treated synthetic result"));

        let second = run_case_for_test(
            &manifest,
            &progress,
            case,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        );
        assert!(matches!(
            second,
            Err(PilotRunError::RecordedIndeterminateCall { .. })
        ));
        assert_eq!(producer.calls.get(), 1);
        assert_eq!(cold.calls, 6);
        assert_eq!(adjudicator.calls, 1, "retry must not call the adjudicator");
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn progress_ledger_rejects_a_different_manifest_digest_before_spend() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let progress = progress_path();
        let digest = manifest.contract_digest().unwrap();
        let _ledger = PilotProgressLedgerV1::open(&progress, &digest).unwrap();

        let mut changed_rows = rows.clone();
        changed_rows[0].selection_reason = "different public-safe reason".to_string();
        let changed = freeze_pilot_manifest(changed_rows).unwrap();
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let error = run_manifest_for_test(
            &changed,
            &progress,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PilotRunError::Progress(PilotProgressErrorV1::ContractMismatch)
        ));
        assert_eq!(producer.calls.get(), 0);
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn started_call_from_interrupted_persistence_is_never_spent_again() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let progress = progress_path();
        let digest = manifest.contract_digest().unwrap();
        let mut ledger = PilotProgressLedgerV1::open(&progress, &digest).unwrap();
        ledger
            .record(
                PilotCallKeyV1::new(
                    &digest,
                    &manifest.rows()[0],
                    PilotCallRoleV1::Producer,
                    PilotCallArmV1::None,
                    0,
                ),
                PilotCallStateV1::Started,
            )
            .unwrap();
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let error = run_case_for_test(
            &manifest,
            &progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "source-0".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PilotRunError::RecordedIndeterminateCall { .. }
        ));
        assert_eq!(producer.calls.get(), 0);
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn non_member_is_refused_before_producer_spend() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();
        let error = run_case_for_test(
            &manifest,
            &progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "not-a-member".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap_err();
        assert!(matches!(error, PilotRunError::NotInManifest(_)));
        assert_eq!(
            producer.calls.get(),
            0,
            "membership must gate before model spend"
        );
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn privacy_rejection_happens_before_producer_spend() {
        let mut rows = rows();
        rows[0].content_sha256 = format!("{:x}", Sha256::digest(b"api_key=not-for-public-pilot"));
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let mut resolver = Resolver::from_rows(&rows);
        resolver.texts.insert(
            (PilotSourceRouteV1::Antigravity, "source-0".to_string(), 1),
            "api_key=not-for-public-pilot".to_string(),
        );
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();
        let error = run_case_for_test(
            &manifest,
            &progress,
            PilotCaseKeyV1 {
                source_route: PilotSourceRouteV1::Antigravity,
                source_id: "source-0".to_string(),
                source_revision: 1,
            },
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PilotRunError::Privacy(PilotPrivacyErrorV1::SecretOrCredentialLike)
        ));
        assert_eq!(
            producer.calls.get(),
            0,
            "privacy must gate before model spend"
        );
        let _ = std::fs::remove_file(progress);
    }

    #[test]
    fn public_report_contains_receipts_but_never_source_or_model_text() {
        let mut rows = rows();
        rows[0].content_sha256 =
            format!("{:x}", Sha256::digest(b"SOURCE_TEXT_MUST_NOT_BE_REPORTED"));
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let mut resolver = Resolver::from_rows(&rows);
        resolver.texts.insert(
            (PilotSourceRouteV1::Antigravity, "source-0".to_string(), 1),
            "SOURCE_TEXT_MUST_NOT_BE_REPORTED".to_string(),
        );
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let progress = progress_path();
        let report = run_manifest_for_test(
            &manifest,
            &progress,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap();
        let json = report.privacy_safe_json().unwrap();
        assert!(json.contains("producer-model"));
        assert!(!json.contains("SOURCE_TEXT_MUST_NOT_BE_REPORTED"));
        assert!(!json.contains("treated synthetic result"));
        assert!(!json.contains("synthetic situation"));
        assert!(json.contains("cost_usd_micros"));
        let ledger_json = std::fs::read_to_string(&progress).unwrap();
        assert!(!ledger_json.contains("SOURCE_TEXT_MUST_NOT_BE_REPORTED"));
        assert!(!ledger_json.contains("treated synthetic result"));
        assert!(!ledger_json.contains("synthetic situation"));
        let _ = std::fs::remove_file(progress);
    }
}
