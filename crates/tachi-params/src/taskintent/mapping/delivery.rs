//! DELIVERY dimension mapping table (TB-16; tachi#1636 law; TB-13 pull-only
//! V2 per RULING-205 §8).
//!
//! Canonical inputs and their existing sources:
//!
//! | Canonical input | Existing truth surface |
//! |---|---|
//! | (no terminal result) | `not_ready` |
//! | terminal result exists | `ready` — pullable via `collect`; NO durable requester-delivery surface exists in V2a (tachi#1679 is its own leaf; RULING-205 §8: pull-only for V2) |
//!
//! The `requester_queued`/`delivered`/`blocked`/`retrying`/`dismissed`
//! inputs of the #1679 frozen delivery model are **absent by construction**
//! in this leaf: there is no durable requester delivery ledger to observe
//! (TB-13 grep check: none is introduced). When tachi#1679 lands, its
//! canonical states become additional inputs HERE — in this table, and
//! never by rewriting execution or adjudication state.

use std::borrow::Borrow;

use serde::{Deserialize, Serialize};

/// Canonical delivery fact. V2a observes only the pull-readiness half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDeliveryFact {
    /// A terminal, collectable result projection exists (pull surface).
    ResultReady,
    /// A newer result revision superseded the prior one.
    ResultRevised,
}

/// Bridge-visible delivery state (tachi#1679 delivery dimension, pull-only
/// V2 subset). Defined only in this mapping table (TB-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// No collectable result yet.
    NotReady,
    /// A result exists and is pullable via `collect` (requester pulls; no
    /// durable requester-delivery surface exists in V2a).
    Ready,
}

/// The delivery mapping table: canonical fact → bridge delivery state. The
/// ONLY place a [`DeliveryState`] is produced.
pub fn map_delivery(fact: &CanonicalDeliveryFact) -> DeliveryState {
    match fact {
        CanonicalDeliveryFact::ResultReady | CanonicalDeliveryFact::ResultRevised => {
            DeliveryState::Ready
        }
    }
}

/// Fold delivery facts into the current delivery projection. A revised
/// result keeps delivery `Ready` at the newer revision — a stale older
/// revision never overwrites a newer one (TB-13).
pub fn project_delivery<I>(facts: I) -> DeliveryState
where
    I: IntoIterator,
    I::Item: std::borrow::Borrow<CanonicalDeliveryFact>,
{
    let mut latest: Option<DeliveryState> = None;
    for fact in facts {
        let fact = fact.borrow();
        latest = Some(map_delivery(fact));
    }
    latest.unwrap_or(DeliveryState::NotReady)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_result_is_not_ready() {
        assert_eq!(
            project_delivery(Vec::<CanonicalDeliveryFact>::new()),
            DeliveryState::NotReady
        );
    }

    #[test]
    fn terminal_result_is_pull_ready() {
        let facts = [CanonicalDeliveryFact::ResultReady];
        assert_eq!(project_delivery(facts.iter()), DeliveryState::Ready);
        // A revision keeps it ready; stale revisions cannot regress it.
        let revised = [
            CanonicalDeliveryFact::ResultReady,
            CanonicalDeliveryFact::ResultRevised,
        ];
        assert_eq!(project_delivery(revised.iter()), DeliveryState::Ready);
    }
}
