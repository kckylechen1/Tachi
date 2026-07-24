//! Blinded A/B discrimination for one pilot case (#1073 "Blinded
//! discrimination").
//!
//! "Run three cold tasks with the candidate and three without it; randomize
//! arm labels. An independent adjudicator blind to arm identity scores
//! decision, citation/grounding, and unsupported claims." — frozen
//! contract.
//!
//! Two structural guarantees this module enforces, both load-bearing:
//!
//! 1. **Blinding is a real information barrier, not a naming convention.**
//!    [`blind_case`] hands the adjudicator [`BlindedItem`]s keyed by an
//!    opaque `blind_id` — the arm mapping lives only in the
//!    [`UnblindKey`] the adjudicator never sees, and the six items are
//!    shuffled with a seeded RNG so their *order* doesn't leak the arm
//!    either.
//! 2. **The adjudicator is not the producer.** "the behavior-change
//!    adjudicator must have an identifiable engine independent from the
//!    producer" (frozen contract, `Execution: dual-track`).
//!    [`dual_track_attested`] is `false` whenever either engine identity is
//!    unknown OR the two identities match — [`evaluate_case`] downgrades
//!    such a case to [`CaseOutcome::Inconclusive`] rather than letting an
//!    unattested run count toward a PASS.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::Serialize;

use tachi_params::LessonEngineReceiptV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmLabel {
    /// Baseline: the existing Summary/old-distill output.
    A,
    /// Treated: the forged `LessonCandidateV1`.
    B,
}

/// One cold task's raw output, pre-blinding. No `arm` field — see
/// `blind_case`'s doc for why arm assignment is structural (argument
/// position), never a settable field on this type.
#[derive(Debug, Clone)]
pub struct ColdRunText {
    pub text: String,
}

/// One item as presented to the blind adjudicator — no arm label, no
/// ordering tell.
#[derive(Debug, Clone)]
pub struct BlindedItem {
    pub blind_id: String,
    pub text: String,
}

/// The arm mapping. Deliberately never handed to the adjudicator — only to
/// the code that scores the adjudicator's per-`blind_id` verdicts back onto
/// arms afterward.
#[derive(Debug, Clone)]
pub struct UnblindKey {
    mapping: std::collections::HashMap<String, ArmLabel>,
}

impl UnblindKey {
    pub fn arm_for(&self, blind_id: &str) -> Option<ArmLabel> {
        self.mapping.get(blind_id).copied()
    }
}

#[derive(Debug, Clone)]
pub struct BlindedCase {
    pub case_id: String,
    pub items: Vec<BlindedItem>,
}

/// Blind + shuffle exactly 3 treated (candidate) and 3 baseline (old
/// summary) runs for one case. `seed` makes the shuffle reproducible for
/// tests; a live caller should derive it from something unpredictable to
/// the adjudicator (e.g. a per-run nonce), never from the case id or arm
/// content itself (which would make "random" order derivable by anyone who
/// can read the case id).
///
/// Two structural fixes over a flat `[ColdRunText; 6]` + settable `arm`
/// field (cross-vendor review finding 4):
///
/// 1. **3-and-3 balance is a type-level guarantee, not a runtime
///    convention.** Two separate `[ColdRunText; 3]` arrays make "not
///    exactly 3 per arm" a compile error, not a possible caller mistake a
///    test could silently pass around.
/// 2. **`blind_id` carries no positional information at all.** The old
///    `format!("blind-{i}-{uuid}")` baked the PRE-shuffle enumeration index
///    into the id string itself — shuffling the `Vec` afterward reordered
///    the items but never touched the ids, so a caller who (as every
///    realistic caller would) always builds `treated` before `baseline`
///    could read the arm straight off `blind_id`'s leading digit. The id is
///    now a bare random UUID; arm membership lives ONLY in `UnblindKey`.
pub fn blind_case(
    case_id: &str,
    treated: [ColdRunText; 3],
    baseline: [ColdRunText; 3],
    seed: u64,
) -> (BlindedCase, UnblindKey) {
    let mut items: Vec<(String, ArmLabel, String)> = treated
        .into_iter()
        .map(|run| (ArmLabel::B, run.text))
        .chain(baseline.into_iter().map(|run| (ArmLabel::A, run.text)))
        .map(|(arm, text)| (format!("blind-{}", uuid::Uuid::new_v4()), arm, text))
        .collect();

    let mut rng = StdRng::seed_from_u64(seed);
    items.shuffle(&mut rng);

    let mut mapping = std::collections::HashMap::new();
    let mut blinded = Vec::with_capacity(items.len());
    for (blind_id, arm, text) in items {
        mapping.insert(blind_id.clone(), arm);
        blinded.push(BlindedItem { blind_id, text });
    }

    (
        BlindedCase {
            case_id: case_id.to_string(),
            items: blinded,
        },
        UnblindKey { mapping },
    )
}

/// Whether unblinding a case's scored runs failed to reconstruct a valid
/// 3-treated / 3-baseline split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnblindError {
    /// A scored `blind_id` doesn't appear in the `UnblindKey` at all — it
    /// wasn't one of the ids `blind_case` produced for this case.
    UnknownBlindId(String),
    /// After grouping by arm, one side didn't land on exactly 3 —
    /// duplicate/missing scores for a `blind_id`, or scores from a
    /// different case's key.
    WrongCount {
        arm: ArmLabel,
        expected: usize,
        actual: usize,
    },
}

/// The typed bridge from a blind adjudicator's per-`blind_id` scores back
/// to a scoreable `(treated, baseline)` [`ArmRunSet`] pair (cross-vendor
/// review finding 4: "no typed bridge from blinded adjudications +
/// `UnblindKey` into scored `CaseInput`" — this is that bridge). Every
/// `blind_id` is resolved through `key`, never trusted at face value from
/// the adjudicator (who never saw the arm mapping in the first place), and
/// the result only exists if each arm lands on exactly 3 scores.
pub fn unblind_scores(
    key: &UnblindKey,
    scored: Vec<(String, ColdRunScore)>,
) -> Result<(ArmRunSet, ArmRunSet), UnblindError> {
    let mut treated: Vec<ColdRunScore> = Vec::new();
    let mut baseline: Vec<ColdRunScore> = Vec::new();
    for (blind_id, score) in scored {
        match key.arm_for(&blind_id) {
            Some(ArmLabel::B) => treated.push(score),
            Some(ArmLabel::A) => baseline.push(score),
            None => return Err(UnblindError::UnknownBlindId(blind_id)),
        }
    }
    let treated: [ColdRunScore; 3] =
        treated
            .try_into()
            .map_err(|v: Vec<ColdRunScore>| UnblindError::WrongCount {
                arm: ArmLabel::B,
                expected: 3,
                actual: v.len(),
            })?;
    let baseline: [ColdRunScore; 3] =
        baseline
            .try_into()
            .map_err(|v: Vec<ColdRunScore>| UnblindError::WrongCount {
                arm: ArmLabel::A,
                expected: 3,
                actual: v.len(),
            })?;
    Ok((ArmRunSet { runs: treated }, ArmRunSet { runs: baseline }))
}

/// The adjudicator must carry the same complete engine identity attestation
/// as the producer. The alias keeps the role explicit at call sites without
/// reducing the receipt to provider/model.
pub type AdjudicatorReceipt = LessonEngineReceiptV1;

/// True only when the producer has a FULLY known, non-fallback,
/// non-degraded identity (`LessonEngineReceiptV1::has_known_identity` —
/// provider + model + version, no fallback chain, not degraded) AND the
/// adjudicator has a fully known non-fallback/non-degraded identity AND those identities
/// differ. An unknown/fallback/degraded producer identity, an unknown
/// adjudicator identity, or matching identities all fail this check — see
/// module doc guarantee 2.
///
/// Reuses `has_known_identity` rather than re-checking provider/model
/// presence by hand: a producer receipt with a non-empty `fallback_chain` or
/// `degraded: true` (or a missing `effective_version`) must fail this check
/// exactly as it fails `LessonCandidateV1::identity_status`'s "preview_only"
/// collapse — a candidate that's preview-only can't simultaneously count as
/// dual-track attested (cross-vendor review finding 3/8: this function used
/// to accept a degraded/fallback producer as long as provider+model were
/// merely present, silently ignoring the frozen contract's "version/
/// fallback/degraded" clause).
pub fn dual_track_attested(
    producer: Option<&LessonEngineReceiptV1>,
    adjudicator: &AdjudicatorReceipt,
) -> bool {
    let Some(producer) = producer else {
        return false;
    };
    if !producer.has_known_identity() || !adjudicator.has_known_identity() {
        return false;
    }
    producer.effective_provider != adjudicator.effective_provider
        || producer.effective_model != adjudicator.effective_model
}

/// One cold run's adjudicator score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdRunScore {
    pub matches_reference_decision: bool,
    pub unsupported_claims: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ArmRunSet {
    pub runs: [ColdRunScore; 3],
}

impl ArmRunSet {
    pub fn reference_hit_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| r.matches_reference_decision)
            .count()
    }

    pub fn avg_unsupported_claims(&self) -> f64 {
        let total: usize = self.runs.iter().map(|r| r.unsupported_claims).sum();
        total as f64 / self.runs.len() as f64
    }
}

/// Everything [`evaluate_case`] needs for one pilot row's blinded
/// discrimination outcome.
///
/// `producer_receipt`/`adjudicator_receipt` carry the ACTUAL receipts —
/// `evaluate_case` computes dual-track attestation itself via
/// [`dual_track_attested`], rather than trusting a caller-supplied bool.
/// (Cross-vendor review finding 3: the prior shape had a bare
/// `dual_track_attested: bool` field a caller could set to `true`
/// regardless of the real receipts, forging the pass gate. Authority here
/// must be earned from the receipts, never caller-asserted.)
#[derive(Debug, Clone)]
pub struct CaseInput {
    pub case_id: String,
    pub treated: ArmRunSet,
    pub baseline: ArmRunSet,
    pub candidate_cites_source_refs: bool,
    pub candidate_claims_establishment: bool,
    pub producer_receipt: Option<LessonEngineReceiptV1>,
    pub adjudicator_receipt: AdjudicatorReceipt,
}

/// The minimum treated-arm reference-decision hit count required to pass
/// (frozen contract criterion 1: "at least two of three treated runs").
/// Named, not a bare `2` scattered through match arms.
pub const REQUIRED_TREATED_HITS: usize = 2;
/// The baseline-arm hit count AT OR ABOVE WHICH the pilot fails to show a
/// change (frozen contract criterion 2: "baseline does not already do so in
/// at least two of three runs").
pub const BASELINE_ALREADY_SUFFICIENT_HITS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FailReason {
    TreatedBelowThreshold {
        hits: usize,
        required: usize,
    },
    BaselineAlreadySufficient {
        hits: usize,
    },
    UnsupportedClaimRateIncreased {
        baseline_rate_x1000: i64,
        treated_rate_x1000: i64,
    },
    MissingSourceCitation,
    ClaimsEstablishment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CaseOutcome {
    Pass,
    Fail(Vec<FailReason>),
    /// The case cannot be scored as PASS or FAIL because the dual-track
    /// independence guarantee (module doc, guarantee 2) doesn't hold —
    /// e.g. the adjudicator and producer engine identity are unknown or
    /// identical. A kill-gate report must never count this toward a pass.
    Inconclusive(&'static str),
}

/// Evaluate one case against the frozen contract's 4 pass criteria. All 4
/// are checked (not short-circuited) so a caller sees every reason a case
/// failed, not just the first.
pub fn evaluate_case(input: &CaseInput) -> CaseOutcome {
    if !dual_track_attested(input.producer_receipt.as_ref(), &input.adjudicator_receipt) {
        return CaseOutcome::Inconclusive(
            "adjudicator engine identity is not independently attested from the producer",
        );
    }

    let mut reasons = Vec::new();

    let treated_hits = input.treated.reference_hit_count();
    if treated_hits < REQUIRED_TREATED_HITS {
        reasons.push(FailReason::TreatedBelowThreshold {
            hits: treated_hits,
            required: REQUIRED_TREATED_HITS,
        });
    }

    let baseline_hits = input.baseline.reference_hit_count();
    if baseline_hits >= BASELINE_ALREADY_SUFFICIENT_HITS {
        reasons.push(FailReason::BaselineAlreadySufficient {
            hits: baseline_hits,
        });
    }

    // Fixed-point (x1000) comparison — avoids float equality pitfalls while
    // keeping the reported rate exact for the recorded 3-run sample.
    let baseline_rate_x1000 = (input.baseline.avg_unsupported_claims() * 1000.0).round() as i64;
    let treated_rate_x1000 = (input.treated.avg_unsupported_claims() * 1000.0).round() as i64;
    if treated_rate_x1000 > baseline_rate_x1000 {
        reasons.push(FailReason::UnsupportedClaimRateIncreased {
            baseline_rate_x1000,
            treated_rate_x1000,
        });
    }

    if !input.candidate_cites_source_refs {
        reasons.push(FailReason::MissingSourceCitation);
    }
    if input.candidate_claims_establishment {
        reasons.push(FailReason::ClaimsEstablishment);
    }

    if reasons.is_empty() {
        CaseOutcome::Pass
    } else {
        CaseOutcome::Fail(reasons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(matches: bool, unsupported: usize) -> ColdRunScore {
        ColdRunScore {
            matches_reference_decision: matches,
            unsupported_claims: unsupported,
        }
    }

    fn known_producer() -> LessonEngineReceiptV1 {
        LessonEngineReceiptV1 {
            requested_role: "producer".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
            ..Default::default()
        }
    }

    fn different_adjudicator() -> AdjudicatorReceipt {
        AdjudicatorReceipt {
            requested_role: "adjudicator".to_string(),
            effective_provider: Some("openai".to_string()),
            effective_model: Some("gpt".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        }
    }

    #[test]
    fn dual_track_false_when_producer_unknown() {
        assert!(!dual_track_attested(None, &different_adjudicator()));
    }

    #[test]
    fn dual_track_false_when_adjudicator_identity_unknown() {
        let adjudicator = AdjudicatorReceipt {
            requested_role: "adjudicator".to_string(),
            effective_provider: None,
            effective_model: None,
            ..different_adjudicator()
        };
        assert!(!dual_track_attested(Some(&known_producer()), &adjudicator));
    }

    #[test]
    fn dual_track_false_when_producer_and_adjudicator_are_the_same_engine() {
        let same = AdjudicatorReceipt {
            requested_role: "adjudicator".to_string(),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
            effective_version: Some("v1".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        };
        assert!(!dual_track_attested(Some(&known_producer()), &same));
    }

    #[test]
    fn dual_track_true_when_engines_genuinely_differ() {
        assert!(dual_track_attested(
            Some(&known_producer()),
            &different_adjudicator()
        ));
    }

    #[test]
    fn dual_track_false_for_each_incomplete_adjudicator_identity_shape() {
        let producer = known_producer();
        let mut missing_version = different_adjudicator();
        missing_version.effective_version = None;
        assert!(!dual_track_attested(Some(&producer), &missing_version));

        let mut fallback = different_adjudicator();
        fallback.fallback_chain = vec!["backup".to_string()];
        assert!(!dual_track_attested(Some(&producer), &fallback));

        let mut degraded = different_adjudicator();
        degraded.degraded = true;
        assert!(!dual_track_attested(Some(&producer), &degraded));

        assert!(dual_track_attested(
            Some(&producer),
            &different_adjudicator()
        ));
    }

    #[test]
    fn dual_track_false_when_producer_receipt_has_a_fallback_chain() {
        // Cross-vendor review finding 3/8: a producer receipt with a
        // fallback chain (or `degraded: true`, or a missing
        // `effective_version`) must NOT count as attested even though
        // provider+model are both present and differ from the adjudicator.
        let mut producer = known_producer();
        producer.fallback_chain = vec!["backup-provider".to_string()];
        assert!(!dual_track_attested(
            Some(&producer),
            &different_adjudicator()
        ));
    }

    #[test]
    fn dual_track_false_when_producer_receipt_is_degraded() {
        let mut producer = known_producer();
        producer.degraded = true;
        assert!(!dual_track_attested(
            Some(&producer),
            &different_adjudicator()
        ));
    }

    #[test]
    fn dual_track_false_when_producer_receipt_has_no_version() {
        let mut producer = known_producer();
        producer.effective_version = None;
        assert!(!dual_track_attested(
            Some(&producer),
            &different_adjudicator()
        ));
    }

    fn treated_runs(labels: [&str; 3]) -> [ColdRunText; 3] {
        labels.map(|t| ColdRunText {
            text: t.to_string(),
        })
    }

    #[test]
    fn blinding_never_leaks_the_arm_label_into_the_blind_id_or_text() {
        let treated = treated_runs(["b1", "b2", "b3"]);
        let baseline = treated_runs(["a1", "a2", "a3"]);
        let (blinded, key) = blind_case("case-1", treated, baseline, 42);
        assert_eq!(blinded.items.len(), 6);
        for item in &blinded.items {
            // The blind id is a bare random uuid — it carries no
            // pre-shuffle positional index and no "A"/"B" marker at all, by
            // construction (see `blind_case`).
            assert!(item.blind_id.starts_with("blind-"));
            assert!(
                !item.blind_id.contains("-0-")
                    && !item.blind_id.contains("-1-")
                    && !item.blind_id.contains("-2-")
                    && !item.blind_id.contains("-3-")
                    && !item.blind_id.contains("-4-")
                    && !item.blind_id.contains("-5-"),
                "blind_id must not embed a pre-shuffle positional index: {}",
                item.blind_id
            );
            // The unblind key must resolve every item to a real arm.
            assert!(key.arm_for(&item.blind_id).is_some());
        }
        // Both arms must be represented among the blinded items.
        let a_count = blinded
            .items
            .iter()
            .filter(|i| key.arm_for(&i.blind_id) == Some(ArmLabel::A))
            .count();
        let b_count = blinded
            .items
            .iter()
            .filter(|i| key.arm_for(&i.blind_id) == Some(ArmLabel::B))
            .count();
        assert_eq!(a_count, 3);
        assert_eq!(b_count, 3);
    }

    #[test]
    fn different_seeds_produce_different_presentation_order() {
        let (blinded1, _) = blind_case(
            "case-1",
            treated_runs(["b1", "b2", "b3"]),
            treated_runs(["a1", "a2", "a3"]),
            1,
        );
        let (blinded2, _) = blind_case(
            "case-1",
            treated_runs(["b1", "b2", "b3"]),
            treated_runs(["a1", "a2", "a3"]),
            2,
        );
        let order1: Vec<&str> = blinded1.items.iter().map(|i| i.text.as_str()).collect();
        let order2: Vec<&str> = blinded2.items.iter().map(|i| i.text.as_str()).collect();
        assert_ne!(order1, order2, "different seeds must shuffle differently");
    }

    #[test]
    fn unblind_scores_groups_by_arm_into_a_scoreable_pair() {
        let (blinded, key) = blind_case(
            "case-1",
            treated_runs(["b1", "b2", "b3"]),
            treated_runs(["a1", "a2", "a3"]),
            7,
        );
        let scored: Vec<(String, ColdRunScore)> = blinded
            .items
            .iter()
            .map(|item| {
                let matches = key.arm_for(&item.blind_id) == Some(ArmLabel::B);
                (item.blind_id.clone(), hit(matches, 0))
            })
            .collect();
        let (treated, baseline) = unblind_scores(&key, scored).expect("must unblind cleanly");
        assert_eq!(treated.reference_hit_count(), 3);
        assert_eq!(baseline.reference_hit_count(), 0);
    }

    #[test]
    fn unblind_scores_rejects_an_unknown_blind_id() {
        let (_, key) = blind_case(
            "case-1",
            treated_runs(["b1", "b2", "b3"]),
            treated_runs(["a1", "a2", "a3"]),
            7,
        );
        let scored = vec![("not-a-real-id".to_string(), hit(true, 0))];
        let err = unblind_scores(&key, scored).expect_err("unknown id must be refused");
        assert_eq!(
            err,
            UnblindError::UnknownBlindId("not-a-real-id".to_string())
        );
    }

    #[test]
    fn unblind_scores_rejects_a_lopsided_split() {
        let (blinded, key) = blind_case(
            "case-1",
            treated_runs(["b1", "b2", "b3"]),
            treated_runs(["a1", "a2", "a3"]),
            7,
        );
        // Score only 5 of the 6 items — one arm ends up short.
        let scored: Vec<(String, ColdRunScore)> = blinded
            .items
            .iter()
            .take(5)
            .map(|item| (item.blind_id.clone(), hit(false, 0)))
            .collect();
        let err = unblind_scores(&key, scored).expect_err("lopsided split must be refused");
        assert!(matches!(err, UnblindError::WrongCount { .. }));
    }

    fn attested_case(treated: ArmRunSet, baseline: ArmRunSet) -> CaseInput {
        CaseInput {
            case_id: "case-1".to_string(),
            treated,
            baseline,
            candidate_cites_source_refs: true,
            candidate_claims_establishment: false,
            producer_receipt: Some(known_producer()),
            adjudicator_receipt: different_adjudicator(),
        }
    }

    #[test]
    fn unattested_case_is_inconclusive_even_if_all_other_criteria_pass() {
        let mut input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(true, 0)],
            },
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            },
        );
        input.producer_receipt = None;
        assert_eq!(
            evaluate_case(&input),
            CaseOutcome::Inconclusive(
                "adjudicator engine identity is not independently attested from the producer"
            )
        );
    }

    #[test]
    fn a_forged_dual_track_claim_cannot_pass_a_degraded_producer() {
        // The specific forgery cross-vendor review finding 3 named: a
        // caller can no longer just assert attestation — `evaluate_case`
        // recomputes it from the actual receipts, so a degraded producer
        // receipt is Inconclusive no matter how favorable the run scores
        // are.
        let mut input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(true, 0)],
            },
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            },
        );
        input.producer_receipt.as_mut().unwrap().degraded = true;
        assert_eq!(
            evaluate_case(&input),
            CaseOutcome::Inconclusive(
                "adjudicator engine identity is not independently attested from the producer"
            )
        );
    }

    #[test]
    fn all_four_criteria_met_passes() {
        let input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(false, 0)],
            }, // 2/3 hits
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            }, // 0/3 hits
        );
        assert_eq!(evaluate_case(&input), CaseOutcome::Pass);
    }

    #[test]
    fn treated_below_threshold_fails_criterion_1() {
        let input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(false, 0), hit(false, 0)],
            }, // 1/3 hits
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            },
        );
        let outcome = evaluate_case(&input);
        assert_eq!(
            outcome,
            CaseOutcome::Fail(vec![FailReason::TreatedBelowThreshold {
                hits: 1,
                required: REQUIRED_TREATED_HITS
            }])
        );
    }

    #[test]
    fn baseline_already_hitting_reference_fails_criterion_2() {
        let input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(true, 0)],
            },
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(false, 0)],
            }, // 2/3 hits already
        );
        let outcome = evaluate_case(&input);
        assert_eq!(
            outcome,
            CaseOutcome::Fail(vec![FailReason::BaselineAlreadySufficient { hits: 2 }])
        );
    }

    #[test]
    fn increased_unsupported_claim_rate_fails_criterion_3() {
        let input = attested_case(
            ArmRunSet {
                runs: [hit(true, 3), hit(true, 3), hit(true, 3)],
            }, // avg 3
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            }, // avg 0
        );
        let outcome = evaluate_case(&input);
        assert!(matches!(
            outcome,
            CaseOutcome::Fail(reasons) if reasons.iter().any(|r| matches!(
                r,
                FailReason::UnsupportedClaimRateIncreased { .. }
            ))
        ));
    }

    #[test]
    fn missing_source_citation_fails_criterion_4() {
        let mut input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(true, 0)],
            },
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            },
        );
        input.candidate_cites_source_refs = false;
        let outcome = evaluate_case(&input);
        assert!(matches!(
            outcome,
            CaseOutcome::Fail(reasons) if reasons.contains(&FailReason::MissingSourceCitation)
        ));
    }

    #[test]
    fn claiming_establishment_fails_criterion_4() {
        let mut input = attested_case(
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(true, 0)],
            },
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            },
        );
        input.candidate_claims_establishment = true;
        let outcome = evaluate_case(&input);
        assert!(matches!(
            outcome,
            CaseOutcome::Fail(reasons) if reasons.contains(&FailReason::ClaimsEstablishment)
        ));
    }

    #[test]
    fn multiple_failing_criteria_are_all_reported_not_just_the_first() {
        let mut input = attested_case(
            ArmRunSet {
                runs: [hit(false, 0), hit(false, 0), hit(false, 0)],
            }, // 0/3, fails crit 1
            ArmRunSet {
                runs: [hit(true, 0), hit(true, 0), hit(false, 0)],
            }, // 2/3, fails crit 2
        );
        input.candidate_cites_source_refs = false; // fails crit 4
        let outcome = evaluate_case(&input);
        match outcome {
            CaseOutcome::Fail(reasons) => assert_eq!(reasons.len(), 3, "{reasons:?}"),
            other => panic!("expected multi-reason Fail, got {other:?}"),
        }
    }
}
