//! The resolver-at-ingress reporting gate (tachi#1681 PR-D debt (a); codex
//! #1681 review BUG-5).
//!
//! # The hole this measures
//!
//! `call_lane_llm` takes a `model_override: Option<&str>` and sends whatever
//! it is given. Nothing between that parameter and the wire asks where the
//! string came from, so an alias — a name that only a resolution can turn into
//! a deployment — would be sent verbatim to a provider that has never heard of
//! it. The review's ruling was that the invariant belongs *at ingress*: an
//! alias must be resolved to a concrete deployment before it reaches the call,
//! and asserting that only at serialization egress is asserting it too late.
//!
//! # Why this gate reports and does not refuse
//!
//! Refusing here today would break live routing. The four env chains are the
//! production path (#1681 D3's compatibility window), `model_override` is how
//! several callers already steer within a lane, and there is no resolution in
//! this code path yet to say which strings are legitimate — the resolver
//! exists (`memcore::catalog::resolver`) but nothing has been cut over to it.
//! That cutover is #1685.
//!
//! So this is a **measurement**, deliberately: it counts how often a reference
//! reaches the wire without having been resolved, per lane, and warns once per
//! distinct reference. When #1685 flips the seam, this counter is the evidence
//! for what the flip will break, and its going to zero is the evidence the flip
//! is complete. A gate that silently changed behaviour instead would have made
//! that measurement impossible to take.
//!
//! # Why the map is bounded
//!
//! The key includes a caller-supplied string, so an unbounded map is an
//! unbounded allocation driven by input. Past the cap, distinct references
//! stop getting their own entry and are counted in `overflow` — the total
//! stays truthful, which is what a counter is for, and the memory does not.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// How many distinct (lane, reference) pairs get their own counter before the
/// rest are folded into `overflow`.
const DISTINCT_REFERENCE_CAP: usize = 64;

/// What the gate decided about one call's model reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngressDisposition {
    /// The call used the lane's own configured model. Nothing to resolve, so
    /// nothing to report.
    LaneConfigured,
    /// The caller supplied a model string that no resolution produced. Counted
    /// and warned; the call proceeds unchanged.
    UnresolvedOverride,
}

/// One (lane, reference) pair the gate has seen, and how often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedReferenceReport {
    pub lane: &'static str,
    pub reference: String,
    pub count: u64,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Default)]
struct Entry {
    count: u64,
    first_seen_at: String,
    last_seen_at: String,
}

/// The counter itself. Cloneable and shared, like `LaneOutageTracker`.
#[derive(Clone, Default)]
pub(crate) struct IngressReferenceGate {
    inner: Arc<RwLock<GateState>>,
}

#[derive(Default)]
struct GateState {
    entries: BTreeMap<(&'static str, String), Entry>,
    /// Sightings past [`DISTINCT_REFERENCE_CAP`] distinct references.
    overflow: u64,
}

impl IngressReferenceGate {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Classify one call's reference and record it if it was not resolved.
    ///
    /// `now_utc` is caller-supplied for the reason `LaneOutageTracker`'s is:
    /// this module has no business owning a clock, and a test that cannot fix
    /// the time cannot assert on it.
    pub(crate) fn observe(
        &self,
        lane: &'static str,
        model_override: Option<&str>,
        lane_model: &str,
        now_utc: &str,
    ) -> IngressDisposition {
        let Some(reference) = model_override else {
            return IngressDisposition::LaneConfigured;
        };
        if reference == lane_model {
            // The caller re-stated the lane's own model. That is not a
            // reference anything had to resolve, and counting it would bury
            // the signal this gate exists to produce.
            return IngressDisposition::LaneConfigured;
        }

        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = (lane, reference.to_string());
        let known = state.entries.contains_key(&key);
        if !known && state.entries.len() >= DISTINCT_REFERENCE_CAP {
            state.overflow += 1;
            return IngressDisposition::UnresolvedOverride;
        }
        let entry = state.entries.entry(key).or_insert_with(|| Entry {
            count: 0,
            first_seen_at: now_utc.to_string(),
            last_seen_at: now_utc.to_string(),
        });
        entry.count += 1;
        entry.last_seen_at = now_utc.to_string();
        drop(state);

        if !known {
            // Once per distinct reference, not once per call: a background
            // loop calling with the same override every minute must not turn
            // an advisory into a log flood.
            tracing::warn!(
                lane,
                reference,
                "[llm] a model reference reached the provider call without having been resolved \
                 (tachi#1681 PR-D; the cutover to the operational resolver is #1685). Behaviour \
                 is unchanged; this is a count."
            );
        }
        IngressDisposition::UnresolvedOverride
    }

    /// Everything the gate has counted, sorted by (lane, reference) so a
    /// status surface is not at the mercy of map iteration order.
    pub(crate) fn snapshot(&self) -> Vec<UnresolvedReferenceReport> {
        let state = self.inner.read().unwrap_or_else(|e| e.into_inner());
        state
            .entries
            .iter()
            .map(|((lane, reference), entry)| UnresolvedReferenceReport {
                lane,
                reference: reference.clone(),
                count: entry.count,
                first_seen_at: entry.first_seen_at.clone(),
                last_seen_at: entry.last_seen_at.clone(),
            })
            .collect()
    }

    /// Sightings that arrived after the distinct-reference cap was reached.
    /// Nonzero means the per-reference list is a sample and the totals are not.
    pub(crate) fn overflow(&self) -> u64 {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .overflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-08-13T00:00:00.000Z";
    const LATER: &str = "2026-08-13T00:05:00.000Z";

    #[test]
    fn the_lanes_own_model_is_not_an_unresolved_reference() {
        let gate = IngressReferenceGate::new();
        assert_eq!(
            gate.observe("reasoning", None, "deepseek-reasoner", NOW),
            IngressDisposition::LaneConfigured
        );
        assert_eq!(
            gate.observe(
                "reasoning",
                Some("deepseek-reasoner"),
                "deepseek-reasoner",
                NOW
            ),
            IngressDisposition::LaneConfigured
        );
        assert!(
            gate.snapshot().is_empty(),
            "counting the lane's own model would bury the signal"
        );
    }

    #[test]
    fn an_override_is_counted_per_lane_and_reference_with_both_timestamps() {
        let gate = IngressReferenceGate::new();
        assert_eq!(
            gate.observe("reasoning", Some("chat.premium"), "deepseek-reasoner", NOW),
            IngressDisposition::UnresolvedOverride
        );
        gate.observe(
            "reasoning",
            Some("chat.premium"),
            "deepseek-reasoner",
            LATER,
        );
        gate.observe("summary", Some("chat.premium"), "qwen-7b", LATER);

        let snapshot = gate.snapshot();
        assert_eq!(snapshot.len(), 2, "the same name on two lanes is two facts");
        assert_eq!(
            snapshot[0],
            UnresolvedReferenceReport {
                lane: "reasoning",
                reference: "chat.premium".to_string(),
                count: 2,
                first_seen_at: NOW.to_string(),
                last_seen_at: LATER.to_string(),
            }
        );
        assert_eq!(snapshot[1].lane, "summary");
        assert_eq!(snapshot[1].count, 1);
        assert_eq!(gate.overflow(), 0);
    }

    #[test]
    fn a_caller_supplied_string_cannot_grow_the_map_without_bound() {
        let gate = IngressReferenceGate::new();
        for index in 0..DISTINCT_REFERENCE_CAP + 25 {
            gate.observe("reasoning", Some(&format!("model-{index}")), "base", NOW);
        }
        assert_eq!(gate.snapshot().len(), DISTINCT_REFERENCE_CAP);
        assert_eq!(
            gate.overflow(),
            25,
            "past the cap the total stays truthful even though the list is a sample"
        );
    }
}
