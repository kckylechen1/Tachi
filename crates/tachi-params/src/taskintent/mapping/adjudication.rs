//! ADJUDICATION dimension mapping table (TB-16; tachi#1636 law; TB-17).
//!
//! Canonical inputs and their existing sources:
//!
//! | Canonical input | Existing truth surface |
//! |---|---|
//! | (no rows) | `unreviewed` — worker `submit`/process exit is never semantic acceptance (tachi#1678) |
//! | verdict rows | `dispatch_adjudications.verdict` + `not_required_reason` (`crates/memcore/src/db/dispatch_adjudications.rs`), `mirror_eval` adjudications for harness-native runs |
//! | conflict | differing verdicts over one outcome (append-only spine) |
//!
//! Also owns the TB-17 independence-class law: which independence classes
//! can satisfy an independent-review requirement (`SameSessionContinuation`
//! never can).

use std::borrow::Borrow;

use serde::{Deserialize, Serialize};

use crate::taskintent::wire::IndependenceClass;

/// Canonical adjudication fact from the adjudication spine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalAdjudicationFact {
    /// An adjudicator accepted the outcome (evidence-bound verdict).
    Accepted,
    /// An adjudicator rejected the outcome.
    Rejected,
    /// Adjudication was recorded as not required, with a reason.
    NotRequired {
        /// The recorded reason (from `dispatch_adjudications.not_required_reason`).
        reason: String,
    },
    /// Follow-up adjudication was requested.
    NeedsFollowUp,
    /// Differing verdicts exist over the same outcome.
    ConflictingVerdicts,
}

/// Bridge-visible adjudication state (tachi#1679 adjudication dimension).
/// Defined only in this mapping table (TB-16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjudicationState {
    /// No adjudication yet; a worker success alone is NOT acceptance.
    Unreviewed,
    /// Accepted by an adjudicator.
    Accepted,
    /// Rejected by an adjudicator.
    Rejected,
    /// Recorded as not required (with reason).
    NotRequired {
        /// The recorded reason.
        reason: String,
    },
    /// Follow-up requested.
    NeedsFollowUp,
    /// Conflicting verdicts under reconciliation.
    Inconsistent,
}

/// The adjudication mapping table: canonical fact → bridge adjudication
/// state. The ONLY place an [`AdjudicationState`] is produced.
pub fn map_adjudication(fact: &CanonicalAdjudicationFact) -> AdjudicationState {
    match fact {
        CanonicalAdjudicationFact::Accepted => AdjudicationState::Accepted,
        CanonicalAdjudicationFact::Rejected => AdjudicationState::Rejected,
        CanonicalAdjudicationFact::NotRequired { reason } => AdjudicationState::NotRequired {
            reason: reason.clone(),
        },
        CanonicalAdjudicationFact::NeedsFollowUp => AdjudicationState::NeedsFollowUp,
        CanonicalAdjudicationFact::ConflictingVerdicts => AdjudicationState::Inconsistent,
    }
}

/// Fold adjudication facts into the current adjudication projection. A
/// later conflicting verdict yields `Inconsistent`; nothing here reads
/// execution or delivery state (tachi#1636 law).
pub fn project_adjudication<I>(facts: I) -> AdjudicationState
where
    I: IntoIterator,
    I::Item: std::borrow::Borrow<CanonicalAdjudicationFact>,
{
    let mut seen: Option<AdjudicationState> = None;
    for fact in facts {
        let fact = fact.borrow();
        let state = map_adjudication(fact);
        seen = match seen {
            None => Some(state),
            Some(existing) if existing == state => Some(existing),
            Some(_) => Some(AdjudicationState::Inconsistent),
        };
    }
    seen.unwrap_or(AdjudicationState::Unreviewed)
}

/// TB-17 independence-class ordering: whether `satisfier` meets the
/// independence requirement `required`.
///
/// `SameSessionContinuation` can never satisfy an independent-review
/// requirement — continuation is not independent review (RULING-205 §9).
/// A [`IndependenceClass::HumanReview`] requirement is only satisfied by
/// itself; deterministic checks only satisfy themselves (a deterministic
/// check is not human review and not cross-vendor review).
pub fn independence_satisfied(required: IndependenceClass, satisfier: IndependenceClass) -> bool {
    if satisfier == IndependenceClass::SameSessionContinuation {
        // A continuation only ever meets a continuation-level bar, and even
        // then it is explicitly labeled continuation, never independent
        // review — no stricter requirement can be satisfied by it.
        return required == IndependenceClass::SameSessionContinuation;
    }
    meets_bar(required, satisfier)
}

/// Strictness ranking for the non-continuation classes.
fn rank(class: IndependenceClass) -> u8 {
    match class {
        IndependenceClass::DeterministicCheck => 0,
        IndependenceClass::SameSessionContinuation => 1,
        IndependenceClass::FreshContextSameHarness => 2,
        IndependenceClass::FreshContextCrossModelSameVendor => 3,
        IndependenceClass::FreshContextCrossVendor => 4,
        IndependenceClass::HumanReview => 5,
    }
}

fn meets_bar(required: IndependenceClass, satisfier: IndependenceClass) -> bool {
    rank(satisfier) >= rank(required)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_facts_is_unreviewed_not_accepted() {
        // tachi#1678: worker submit/exit without adjudication is unreviewed.
        assert_eq!(
            project_adjudication(Vec::<CanonicalAdjudicationFact>::new()),
            AdjudicationState::Unreviewed
        );
    }

    #[test]
    fn conflicting_verdicts_are_inconsistent() {
        let facts = [
            CanonicalAdjudicationFact::Accepted,
            CanonicalAdjudicationFact::Rejected,
        ];
        assert_eq!(
            project_adjudication(facts.iter()),
            AdjudicationState::Inconsistent
        );
    }

    #[test]
    fn continuation_never_satisfies_independent_review() {
        // TB-17 / owner vertical test 6 companion: an independence
        // requirement satisfied only by a continuation is rejected.
        for required in [
            IndependenceClass::FreshContextSameHarness,
            IndependenceClass::FreshContextCrossModelSameVendor,
            IndependenceClass::FreshContextCrossVendor,
            IndependenceClass::HumanReview,
        ] {
            assert!(!independence_satisfied(
                required,
                IndependenceClass::SameSessionContinuation
            ));
        }
        // Cross-vendor satisfies same-harness bar; same-harness does not
        // satisfy cross-vendor.
        assert!(independence_satisfied(
            IndependenceClass::FreshContextSameHarness,
            IndependenceClass::FreshContextCrossVendor
        ));
        assert!(!independence_satisfied(
            IndependenceClass::FreshContextCrossVendor,
            IndependenceClass::FreshContextSameHarness
        ));
        // Human review is required only by itself in the strict sense — a
        // machine class cannot stand in for a human.
        assert!(!independence_satisfied(
            IndependenceClass::HumanReview,
            IndependenceClass::FreshContextCrossVendor
        ));
    }
}
