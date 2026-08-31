//! DELIVERY dimension mapping table (TB-16; tachi#1636 law; TB-13 pull-only
//! V2 per RULING-205 §8).
//!
//! Canonical inputs and their existing sources:
//!
//! | Canonical input | Existing truth surface |
//! |---|---|
//! | (no terminal result) | `not_ready` |
//! | terminal result exists | `ready` — pullable via `collect` |
//! | #1679 spine state | `ready` / `requester_queued` / `delivered` / `blocked` / `retrying` / `dismissed` — observed from the v36 delivery spine via the #1693 `DeliveryObservationV1::Observed` adapter |
//!
//! The mapping is a pure table (TB-16): a delivery-state transition never
//! changes the projected execution or adjudication state, and execution or
//! adjudication facts never produce a delivery state here.

use std::borrow::Borrow;

use serde::{Deserialize, Serialize};

/// Canonical delivery fact. V2a observes the pull-readiness half; the #1679
/// spine contributes the durable delivery half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDeliveryFact {
    /// No collectable result yet.
    NoResult,
    /// A terminal, collectable result projection exists (pull surface).
    ResultReady,
    /// A newer result revision superseded the prior one.
    ResultRevised,
    /// An admitted requester holds an unexpired claim (#1679).
    ClaimedByRequester,
    /// The requester acknowledged delivery (#1679).
    DeliveredReceipt,
    /// Delivery is parked on a non-retryable blocker (e.g. an ambiguous
    /// send outcome); never auto-redispatched (#1679).
    BlockedDelivery,
    /// A retryable delivery failure is scheduled (#1679).
    RetryingDelivery,
    /// The requester dismissed the delivery; execution evidence and
    /// adjudication are untouched (#1679).
    DismissedByRequester,
}

/// Bridge-visible delivery state (the frozen #1679 seven-state vocabulary).
/// Defined only in this mapping table (TB-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// No collectable result yet.
    NotReady,
    /// A result exists and is pullable via `collect` (requester pulls; no
    /// durable requester-delivery surface exists in V2a).
    Ready,
    /// An admitted requester holds an in-flight claim.
    RequesterQueued,
    /// The requester acknowledged delivery.
    Delivered,
    /// Parked on a non-retryable blocker.
    Blocked,
    /// A retry is scheduled.
    Retrying,
    /// The requester dismissed the delivery.
    Dismissed,
}

/// The delivery mapping table: canonical fact → bridge delivery state. The
/// ONLY place a [`DeliveryState`] is produced.
pub fn map_delivery(fact: &CanonicalDeliveryFact) -> DeliveryState {
    match fact {
        CanonicalDeliveryFact::NoResult => DeliveryState::NotReady,
        CanonicalDeliveryFact::ResultReady | CanonicalDeliveryFact::ResultRevised => {
            DeliveryState::Ready
        }
        CanonicalDeliveryFact::ClaimedByRequester => DeliveryState::RequesterQueued,
        CanonicalDeliveryFact::DeliveredReceipt => DeliveryState::Delivered,
        CanonicalDeliveryFact::BlockedDelivery => DeliveryState::Blocked,
        CanonicalDeliveryFact::RetryingDelivery => DeliveryState::Retrying,
        CanonicalDeliveryFact::DismissedByRequester => DeliveryState::Dismissed,
    }
}

/// Fold delivery facts into the current delivery projection. A revised
/// result keeps delivery `Ready` at the newer revision — a stale older
/// revision never overwrites a newer one (TB-13). The fold observes facts
/// in the order the adapter supplies them; a later fact wins only because
/// the adapter ordered it, never because delivery rewrites another plane.
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

    /// tachi#1679 landed: the full frozen vocabulary maps 1:1, and the
    /// full spine lifecycle folds without ever producing an execution or
    /// adjudication state.
    #[test]
    fn delivery_spine_states_map_onto_the_frozen_seven_state_vocabulary() {
        let lifecycle = [
            CanonicalDeliveryFact::ResultReady,
            CanonicalDeliveryFact::ClaimedByRequester,
            CanonicalDeliveryFact::BlockedDelivery,
            CanonicalDeliveryFact::RetryingDelivery,
            CanonicalDeliveryFact::ClaimedByRequester,
            CanonicalDeliveryFact::DeliveredReceipt,
        ];
        let folded = project_delivery(lifecycle.iter());
        assert_eq!(folded, DeliveryState::Delivered);

        // Dismiss changes only delivery state.
        let dismissed = project_delivery([CanonicalDeliveryFact::DismissedByRequester].iter());
        assert_eq!(dismissed, DeliveryState::Dismissed);

        // The mapping is total and pure: every fact maps without touching
        // any other dimension's vocabulary (compile-time: the function's
        // return type is DeliveryState and its input admits no execution
        // or adjudication variant).
        for fact in [
            CanonicalDeliveryFact::NoResult,
            CanonicalDeliveryFact::ResultReady,
            CanonicalDeliveryFact::ResultRevised,
            CanonicalDeliveryFact::ClaimedByRequester,
            CanonicalDeliveryFact::DeliveredReceipt,
            CanonicalDeliveryFact::BlockedDelivery,
            CanonicalDeliveryFact::RetryingDelivery,
            CanonicalDeliveryFact::DismissedByRequester,
        ] {
            let _state: DeliveryState = map_delivery(&fact);
        }
    }
}
