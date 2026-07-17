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

use tachi_params::LessonEngineReceiptV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmLabel {
    /// Baseline: the existing Summary/old-distill output.
    A,
    /// Treated: the forged `LessonCandidateV1`.
    B,
}

/// One cold task's raw output, pre-blinding.
#[derive(Debug, Clone)]
pub struct ColdRunText {
    pub arm: ArmLabel,
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

/// Blind + shuffle 3 treated and 3 baseline runs for one case. `seed` makes
/// the shuffle reproducible for tests; a live caller should derive it from
/// something unpredictable to the adjudicator (e.g. a per-run nonce), never
/// from the case id or arm content itself (which would make "random" order
/// derivable by anyone who can read the case id).
pub fn blind_case(case_id: &str, runs: [ColdRunText; 6], seed: u64) -> (BlindedCase, UnblindKey) {
    let mut items: Vec<(String, ArmLabel, String)> = runs
        .into_iter()
        .enumerate()
        .map(|(i, run)| {
            let blind_id = format!("blind-{i}-{}", uuid::Uuid::new_v4());
            (blind_id, run.arm, run.text)
        })
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

/// The adjudicator's engine identity — deliberately a separate type from
/// `LessonEngineReceiptV1` (the producer's receipt): a caller must supply
/// an actual, distinct value, not reuse the producer's receipt type as a
/// stand-in and risk accidentally aliasing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjudicatorReceipt {
    pub effective_provider: Option<String>,
    pub effective_model: Option<String>,
}

/// True only when BOTH the producer and the adjudicator have a fully known
/// (provider + model) identity AND those identities differ. An unknown
/// producer identity, an unknown adjudicator identity, or matching
/// identities all fail this check — see module doc guarantee 2.
pub fn dual_track_attested(
    producer: Option<&LessonEngineReceiptV1>,
    adjudicator: &AdjudicatorReceipt,
) -> bool {
    let Some(producer) = producer else {
        return false;
    };
    let (Some(p_provider), Some(p_model)) =
        (&producer.effective_provider, &producer.effective_model)
    else {
        return false;
    };
    let (Some(a_provider), Some(a_model)) = (
        &adjudicator.effective_provider,
        &adjudicator.effective_model,
    ) else {
        return false;
    };
    p_provider != a_provider || p_model != a_model
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
/// discrimination outcome. `dual_track_attested` must be computed by the
/// caller via [`dual_track_attested`] against the SAME receipts used to
/// actually produce/adjudicate this case — it is threaded in rather than
/// recomputed here so this function stays a pure decision over already-
/// verified inputs.
#[derive(Debug, Clone)]
pub struct CaseInput {
    pub case_id: String,
    pub treated: ArmRunSet,
    pub baseline: ArmRunSet,
    pub candidate_cites_source_refs: bool,
    pub candidate_claims_establishment: bool,
    pub dual_track_attested: bool,
}

/// The minimum treated-arm reference-decision hit count required to pass
/// (frozen contract criterion 1: "at least two of three treated runs").
/// Named, not a bare `2` scattered through match arms.
pub const REQUIRED_TREATED_HITS: usize = 2;
/// The baseline-arm hit count AT OR ABOVE WHICH the pilot fails to show a
/// change (frozen contract criterion 2: "baseline does not already do so in
/// at least two of three runs").
pub const BASELINE_ALREADY_SUFFICIENT_HITS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    if !input.dual_track_attested {
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
        }
    }

    fn different_adjudicator() -> AdjudicatorReceipt {
        AdjudicatorReceipt {
            effective_provider: Some("openai".to_string()),
            effective_model: Some("gpt".to_string()),
        }
    }

    #[test]
    fn dual_track_false_when_producer_unknown() {
        assert!(!dual_track_attested(None, &different_adjudicator()));
    }

    #[test]
    fn dual_track_false_when_adjudicator_identity_unknown() {
        let adjudicator = AdjudicatorReceipt {
            effective_provider: None,
            effective_model: None,
        };
        assert!(!dual_track_attested(Some(&known_producer()), &adjudicator));
    }

    #[test]
    fn dual_track_false_when_producer_and_adjudicator_are_the_same_engine() {
        let same = AdjudicatorReceipt {
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude".to_string()),
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
    fn blinding_never_leaks_the_arm_label_into_the_blind_id_or_text() {
        let runs = [
            ColdRunText {
                arm: ArmLabel::A,
                text: "a1".to_string(),
            },
            ColdRunText {
                arm: ArmLabel::A,
                text: "a2".to_string(),
            },
            ColdRunText {
                arm: ArmLabel::A,
                text: "a3".to_string(),
            },
            ColdRunText {
                arm: ArmLabel::B,
                text: "b1".to_string(),
            },
            ColdRunText {
                arm: ArmLabel::B,
                text: "b2".to_string(),
            },
            ColdRunText {
                arm: ArmLabel::B,
                text: "b3".to_string(),
            },
        ];
        let (blinded, key) = blind_case("case-1", runs, 42);
        assert_eq!(blinded.items.len(), 6);
        for item in &blinded.items {
            // The blind id is an opaque index+uuid — it carries no "A"/"B"
            // marker at all, by construction (see `blind_case`).
            assert!(item.blind_id.starts_with("blind-"));
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
        let make_runs = || {
            [
                ColdRunText {
                    arm: ArmLabel::A,
                    text: "a1".to_string(),
                },
                ColdRunText {
                    arm: ArmLabel::A,
                    text: "a2".to_string(),
                },
                ColdRunText {
                    arm: ArmLabel::A,
                    text: "a3".to_string(),
                },
                ColdRunText {
                    arm: ArmLabel::B,
                    text: "b1".to_string(),
                },
                ColdRunText {
                    arm: ArmLabel::B,
                    text: "b2".to_string(),
                },
                ColdRunText {
                    arm: ArmLabel::B,
                    text: "b3".to_string(),
                },
            ]
        };
        let (blinded1, _) = blind_case("case-1", make_runs(), 1);
        let (blinded2, _) = blind_case("case-1", make_runs(), 2);
        let order1: Vec<&str> = blinded1.items.iter().map(|i| i.text.as_str()).collect();
        let order2: Vec<&str> = blinded2.items.iter().map(|i| i.text.as_str()).collect();
        assert_ne!(order1, order2, "different seeds must shuffle differently");
    }

    fn attested_case(treated: ArmRunSet, baseline: ArmRunSet) -> CaseInput {
        CaseInput {
            case_id: "case-1".to_string(),
            treated,
            baseline,
            candidate_cites_source_refs: true,
            candidate_claims_establishment: false,
            dual_track_attested: true,
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
        input.dual_track_attested = false;
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
