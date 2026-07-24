//! Spend-gated execution seam for the #1073 pilot.
//!
//! The runner checks manifest membership, resolves and verifies the complete
//! source binding, and only then calls an injected producer.  The injected
//! traits make the complete 50-row flow testable without a live engine,
//! credentials, Vault access, or spend.

use serde::Serialize;
use sha2::{Digest, Sha256};
use tachi_params::{
    EvidenceRefV1, EvidenceRelationV1, ImmutableRevisionV1, LessonCandidateV1,
    LessonEngineReceiptV1, SourceKindV1,
};

use super::discrimination::{
    blind_case, evaluate_case, unblind_scores, AdjudicatorReceipt, BlindedCase, CaseOutcome,
    ColdRunScore, ColdRunText,
};
use super::forge::{forge_lesson_candidate, ForgeDraft, SourceBundle};
use super::pilot::{PilotManifestV1, PilotRowV1, PilotSourceRouteV1};
use super::privacy::{screen_source_for_public_pilot, PilotPrivacyErrorV1};
use super::source::{PilotSourceResolverV1, ResolvedPilotSourceV1, SourceResolveError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotCaseKeyV1 {
    pub source_route: PilotSourceRouteV1,
    pub source_id: String,
    pub source_revision: i64,
}

#[derive(Debug, Clone)]
pub struct ProducedDraftV1 {
    pub draft: ForgeDraft,
    pub receipt: LessonEngineReceiptV1,
}

#[derive(Debug, Clone)]
pub struct ColdRunResultV1 {
    /// Kept in-process for blind adjudication only; it is never reportable.
    pub text: String,
    pub receipt: LessonEngineReceiptV1,
}

#[derive(Debug, Clone)]
pub struct AdjudicationResultV1 {
    pub scores: Vec<(String, ColdRunScore)>,
    pub receipt: LessonEngineReceiptV1,
}

pub trait PilotProducerV1 {
    fn produce(
        &mut self,
        binding: &PilotRowV1,
        source: &ResolvedPilotSourceV1,
    ) -> Result<ProducedDraftV1, String>;
}

pub trait PilotColdRunnerV1 {
    fn run_treated(
        &mut self,
        binding: &PilotRowV1,
        candidate: &LessonCandidateV1,
        invocation: usize,
    ) -> Result<ColdRunResultV1, String>;

    fn run_baseline(
        &mut self,
        binding: &PilotRowV1,
        invocation: usize,
    ) -> Result<ColdRunResultV1, String>;
}

pub trait PilotAdjudicatorV1 {
    fn adjudicate(
        &mut self,
        binding: &PilotRowV1,
        blinded: &BlindedCase,
    ) -> Result<AdjudicationResultV1, String>;
}

#[derive(Debug)]
pub enum PilotRunError {
    NotInManifest(PilotCaseKeyV1),
    Source(SourceResolveError),
    Privacy(PilotPrivacyErrorV1),
    StageFailed {
        source_id: String,
        stage: &'static str,
    },
    Forge(String),
    Unblind(String),
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
            Self::StageFailed { source_id, stage } => {
                write!(f, "pilot {stage} stage failed for source {source_id}")
            }
            Self::Forge(message) => write!(f, "pilot forge gate refused candidate: {message}"),
            Self::Unblind(message) => write!(
                f,
                "pilot blind adjudication could not be verified: {message}"
            ),
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
    pub producer_receipt: LessonEngineReceiptV1,
    pub treated_receipts: [LessonEngineReceiptV1; 3],
    pub baseline_receipts: [LessonEngineReceiptV1; 3],
    pub adjudicator_receipt: LessonEngineReceiptV1,
}

#[derive(Debug, Clone, Serialize)]
pub struct PilotRunReportV1 {
    pub manifest_digest: String,
    pub cases: Vec<PilotRunCaseReportV1>,
}

impl PilotRunReportV1 {
    pub fn passed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|case| matches!(case.outcome, CaseOutcome::Pass))
            .count()
    }

    pub fn privacy_safe_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Execute the exact frozen 50-row flow.  This remains generic over all model
/// seams so tests can execute 50 producer + 300 cold + 50 adjudicator calls
/// synthetically, while a phase-2 caller can install a spend-authorized
/// implementation without changing the gates.
pub fn run_manifest<R, P, C, A>(
    manifest: &PilotManifestV1,
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
        cases.push(run_case(
            manifest,
            key,
            resolver,
            producer,
            cold_runner,
            adjudicator,
        )?);
    }
    Ok(PilotRunReportV1 {
        manifest_digest: manifest
            .contract_digest()
            .map_err(|_| PilotRunError::StageFailed {
                source_id: "manifest".to_string(),
                stage: "digest",
            })?,
        cases,
    })
}

/// Execute one case only after an exact route/id/revision membership check and
/// a full-content source verification.  No producer call appears before those
/// two gates.
pub fn run_case<R, P, C, A>(
    manifest: &PilotManifestV1,
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
    screen_source_for_public_pilot(&source.full_text).map_err(PilotRunError::Privacy)?;
    let produced = producer
        .produce(&binding, &source)
        .map_err(|_| PilotRunError::StageFailed {
            source_id: binding.source_id.clone(),
            stage: "producer",
        })?;
    let candidate = forge_lesson_candidate(
        "lesson-forge-pilot",
        manifest,
        &source_bundle(&binding, source),
        binding.target_kind,
        &produced.draft,
        Some(produced.receipt.clone()),
    )
    .map_err(|err| PilotRunError::Forge(err.to_string()))?;

    let (treated, treated_receipts) = run_treated(&binding, &candidate, cold_runner)?;
    let (baseline, baseline_receipts) = run_baseline(&binding, cold_runner)?;
    let (blinded, key) = blind_case(&binding.source_id, treated, baseline, blind_seed(&binding));
    let adjudicated =
        adjudicator
            .adjudicate(&binding, &blinded)
            .map_err(|_| PilotRunError::StageFailed {
                source_id: binding.source_id.clone(),
                stage: "adjudicator",
            })?;
    let (treated, baseline) = unblind_scores(&key, adjudicated.scores)
        .map_err(|err| PilotRunError::Unblind(format!("{err:?}")))?;
    let outcome = evaluate_case(&super::discrimination::CaseInput {
        case_id: binding.source_id.clone(),
        treated,
        baseline,
        candidate_cites_source_refs: candidate.cites_source_refs(),
        candidate_claims_establishment: candidate.claims_establishment(),
        producer_receipt: Some(produced.receipt.clone()),
        adjudicator_receipt: AdjudicatorReceipt {
            effective_provider: adjudicated.receipt.effective_provider.clone(),
            effective_model: adjudicated.receipt.effective_model.clone(),
        },
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

fn run_treated<C: PilotColdRunnerV1>(
    binding: &PilotRowV1,
    candidate: &LessonCandidateV1,
    runner: &mut C,
) -> Result<([ColdRunText; 3], [LessonEngineReceiptV1; 3]), PilotRunError> {
    let one = runner
        .run_treated(binding, candidate, 0)
        .map_err(|_| stage_error(binding, "treated-cold"))?;
    let two = runner
        .run_treated(binding, candidate, 1)
        .map_err(|_| stage_error(binding, "treated-cold"))?;
    let three = runner
        .run_treated(binding, candidate, 2)
        .map_err(|_| stage_error(binding, "treated-cold"))?;
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
    binding: &PilotRowV1,
    runner: &mut C,
) -> Result<([ColdRunText; 3], [LessonEngineReceiptV1; 3]), PilotRunError> {
    let one = runner
        .run_baseline(binding, 0)
        .map_err(|_| stage_error(binding, "baseline-cold"))?;
    let two = runner
        .run_baseline(binding, 1)
        .map_err(|_| stage_error(binding, "baseline-cold"))?;
    let three = runner
        .run_baseline(binding, 2)
        .map_err(|_| stage_error(binding, "baseline-cold"))?;
    Ok((
        [
            ColdRunText { text: one.text },
            ColdRunText { text: two.text },
            ColdRunText { text: three.text },
        ],
        [one.receipt, two.receipt, three.receipt],
    ))
}

fn stage_error(binding: &PilotRowV1, stage: &'static str) -> PilotRunError {
    PilotRunError::StageFailed {
        source_id: binding.source_id.clone(),
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

    fn receipt(role: &str, provider: &str, model: &str) -> LessonEngineReceiptV1 {
        LessonEngineReceiptV1 {
            requested_role: role.to_string(),
            effective_provider: Some(provider.to_string()),
            effective_model: Some(model.to_string()),
            effective_version: Some("test-v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
            tokens: Some(11),
            cost_usd_micros: Some(17),
            latency_ms: Some(23),
        }
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
        ) -> Result<ProducedDraftV1, String> {
            self.calls.set(self.calls.get() + 1);
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

    #[derive(Default)]
    struct Cold {
        calls: usize,
    }

    impl PilotColdRunnerV1 for Cold {
        fn run_treated(
            &mut self,
            _binding: &PilotRowV1,
            _candidate: &LessonCandidateV1,
            _invocation: usize,
        ) -> Result<ColdRunResultV1, String> {
            self.calls += 1;
            Ok(ColdRunResultV1 {
                text: "treated synthetic result".to_string(),
                receipt: receipt("cold", "cold-provider", "cold-model"),
            })
        }
        fn run_baseline(
            &mut self,
            _binding: &PilotRowV1,
            _invocation: usize,
        ) -> Result<ColdRunResultV1, String> {
            self.calls += 1;
            Ok(ColdRunResultV1 {
                text: "baseline synthetic result".to_string(),
                receipt: receipt("cold", "cold-provider", "cold-model"),
            })
        }
    }

    #[derive(Default)]
    struct Adjudicator {
        calls: usize,
    }

    impl PilotAdjudicatorV1 for Adjudicator {
        fn adjudicate(
            &mut self,
            _binding: &PilotRowV1,
            blinded: &BlindedCase,
        ) -> Result<AdjudicationResultV1, String> {
            self.calls += 1;
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
                receipt: receipt("adjudicator", "adjudicator-provider", "adjudicator-model"),
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
        let report = run_manifest(
            &manifest,
            &resolver,
            &mut producer,
            &mut cold,
            &mut adjudicator,
        )
        .unwrap();
        assert_eq!(report.cases.len(), 50);
        assert_eq!(report.passed_count(), 50);
        assert_eq!(producer.calls.get(), 50);
        assert_eq!(cold.calls, 300);
        assert_eq!(adjudicator.calls, 50);
    }

    #[test]
    fn non_member_is_refused_before_producer_spend() {
        let rows = rows();
        let manifest = freeze_pilot_manifest(rows.clone()).unwrap();
        let resolver = Resolver::from_rows(&rows);
        let mut producer = Producer::default();
        let mut cold = Cold::default();
        let mut adjudicator = Adjudicator::default();
        let error = run_case(
            &manifest,
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
        let error = run_case(
            &manifest,
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
        let report = run_manifest(
            &manifest,
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
    }
}
