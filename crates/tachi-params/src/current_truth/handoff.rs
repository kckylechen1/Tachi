//! Stale handoff contract (#1696).
//!
//! A handoff binds the CurrentTruth **evidence-head revisions** it was
//! generated from. Later merge/revert/reopen/owner-close facts can mark its
//! action claims stale — per claim, never wholesale — while the historical
//! packet itself is preserved byte-for-byte. Consumers receive stale claim
//! refs and replacement evidence heads, never silently rewritten prose, and
//! no raw handoff narrative ever becomes authority.

use serde::{Deserialize, Serialize};

use super::reducer::{ReducedPredicateV1, ReductionV1};
use super::types::{EvidenceHeadV1, PredicateV1, ReductionStatusV1, SubjectRefV1};

/// One claim inside a handoff packet: a `(subject, predicate)` fact the
/// packet's action guidance relied on, bound to the evidence head it was
/// generated from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffClaimBindingV1 {
    pub subject: SubjectRefV1,
    pub predicate: PredicateV1,
    /// The evidence head (assertion id + source revision + observed_at) the
    /// claim was generated against.
    pub head: EvidenceHeadV1,
}

/// A handoff packet: immutable history plus its evidence-head bindings.
/// The packet is never rewritten after generation — staleness is *reported*
/// against it, not edited into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffPacketV1 {
    pub handoff_id: String,
    /// RFC 3339 generation timestamp (source-supplied; this module reads no
    /// clock).
    pub generated_at: String,
    pub repo: String,
    pub claim_bindings: Vec<HandoffClaimBindingV1>,
}

/// Per-claim staleness evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffClaimStalenessV1 {
    pub binding: HandoffClaimBindingV1,
    /// `true` when the predicate's current evidence head moved strictly
    /// beyond the bound head, or the predicate became conflicted.
    pub stale: bool,
    /// Why the claim is stale, when it is.
    pub reason: Option<HandoffStaleReasonV1>,
    /// The replacement evidence head, when one exists.
    pub replacement_head: Option<EvidenceHeadV1>,
}

/// Why a handoff claim went stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStaleReasonV1 {
    /// A newer current head replaced the bound one (merge/reopen/retract/
    /// correction arrived).
    SupersededByNewerHead,
    /// The predicate is now conflicted.
    NowConflicted,
    /// The predicate no longer has a current head (retracted world).
    NoCurrentHead,
}

/// The staleness report for one handoff packet against a reduction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffStalenessReportV1 {
    pub handoff_id: String,
    pub claims: Vec<HandoffClaimStalenessV1>,
}

impl HandoffStalenessReportV1 {
    /// Content-free stale-claim count (for projection health).
    pub fn stale_claim_count(&self) -> usize {
        self.claims.iter().filter(|claim| claim.stale).count()
    }
}

/// Evaluate a handoff packet against the current reduction. Pure: the packet
/// is consumed by reference and never modified; staleness is
/// predicate/claim-specific — unrelated evidence does not invalidate the
/// packet.
pub fn evaluate_handoff_staleness(
    packet: &HandoffPacketV1,
    reduction: &ReductionV1,
) -> HandoffStalenessReportV1 {
    let claims = packet
        .claim_bindings
        .iter()
        .map(|binding| {
            let (stale, reason, replacement_head) =
                if let Some(family) = lifecycle_family(binding.predicate) {
                    classify_lifecycle_claim(binding, reduction, family)
                } else {
                    let reduced: ReducedPredicateV1 =
                        reduction.get(&binding.subject, binding.predicate);
                    classify_claim(binding, &reduced)
                };
            HandoffClaimStalenessV1 {
                binding: binding.clone(),
                stale,
                reason,
                replacement_head,
            }
        })
        .collect();
    HandoffStalenessReportV1 {
        handoff_id: packet.handoff_id.clone(),
        claims,
    }
}

fn classify_claim(
    binding: &HandoffClaimBindingV1,
    reduced: &ReducedPredicateV1,
) -> (bool, Option<HandoffStaleReasonV1>, Option<EvidenceHeadV1>) {
    let bound_key = super::types::head_order_key(&binding.head);
    match reduced.status {
        ReductionStatusV1::Conflicted => (true, Some(HandoffStaleReasonV1::NowConflicted), None),
        ReductionStatusV1::Current => {
            // The newest current head decides freshness: a head identical to
            // the bound one keeps the claim current; a strictly newer one
            // makes it stale with a replacement head.
            let newest = reduced
                .current_heads
                .iter()
                .max_by_key(|head| super::types::head_order_key(head));
            match newest {
                None => (true, Some(HandoffStaleReasonV1::NoCurrentHead), None),
                Some(head) => {
                    let key = super::types::head_order_key(head);
                    if key == bound_key {
                        (false, None, None)
                    } else {
                        (
                            true,
                            Some(HandoffStaleReasonV1::SupersededByNewerHead),
                            Some(head.clone()),
                        )
                    }
                }
            }
        }
        // No admitted current fact for the claim's predicate any more
        // (e.g. everything retracted) — stale with nothing to replace it.
        ReductionStatusV1::Superseded | ReductionStatusV1::Unknown => {
            (true, Some(HandoffStaleReasonV1::NoCurrentHead), None)
        }
    }
}

/// The lifecycle families: a claim about one member predicate is also a
/// claim about the subject's current lifecycle (#1696 stale handoff
/// contract: "later merge/revert/reopen/owner-close facts can mark its
/// action claims stale"). A merge arriving after an "open PR" claim must
/// stale that claim even though `pr_open` and `pr_merged` are distinct
/// predicates.
fn lifecycle_family(predicate: PredicateV1) -> Option<&'static [PredicateV1]> {
    const ISSUE_FAMILY: &[PredicateV1] = &[
        PredicateV1::IssueOpen,
        PredicateV1::IssueClosed,
        PredicateV1::IssueReopened,
    ];
    const PR_FAMILY: &[PredicateV1] = &[
        PredicateV1::PrOpen,
        PredicateV1::PrMerged,
        PredicateV1::PrClosedUnmerged,
        PredicateV1::MergeReverted,
    ];
    if ISSUE_FAMILY.contains(&predicate) {
        Some(ISSUE_FAMILY)
    } else if PR_FAMILY.contains(&predicate) {
        Some(PR_FAMILY)
    } else {
        None
    }
}

/// Family-aware staleness: the claim is stale when any family member's
/// current head moved strictly beyond the bound head.
fn classify_lifecycle_claim(
    binding: &HandoffClaimBindingV1,
    reduction: &ReductionV1,
    family: &[PredicateV1],
) -> (bool, Option<HandoffStaleReasonV1>, Option<EvidenceHeadV1>) {
    let bound_key = super::types::head_order_key(&binding.head);
    type HeadKey = (chrono::DateTime<chrono::Utc>, String, String);
    let mut newest: Option<HeadKey> = None;
    let mut newest_head: Option<EvidenceHeadV1> = None;
    let mut conflicted = false;
    let mut any_current = false;
    for predicate in family {
        let reduced = reduction.get(&binding.subject, *predicate);
        match reduced.status {
            ReductionStatusV1::Conflicted => conflicted = true,
            ReductionStatusV1::Current => {
                any_current = true;
                for head in &reduced.current_heads {
                    let key = super::types::head_order_key(head);
                    if newest.as_ref().is_none_or(|best| key > *best) {
                        newest = Some(key);
                        newest_head = Some(head.clone());
                    }
                }
            }
            ReductionStatusV1::Superseded | ReductionStatusV1::Unknown => {}
        }
    }
    if conflicted {
        return (true, Some(HandoffStaleReasonV1::NowConflicted), None);
    }
    match (newest, newest_head) {
        (None, _) => (true, Some(HandoffStaleReasonV1::NoCurrentHead), None),
        (Some(key), head) => {
            if key == bound_key || !any_current {
                (false, None, None)
            } else {
                (
                    true,
                    Some(HandoffStaleReasonV1::SupersededByNewerHead),
                    head,
                )
            }
        }
    }
}
