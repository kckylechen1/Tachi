use super::{
    ChecksState, MergeDecision, MergeGatePolicy, Mergeable, PrLifecycleState, PrState,
    ReviewDecision,
};

pub const PROTECTED_NO_CLOSE_LABELS: &[&str] = &[
    "agent:no-close",
    "type:umbrella",
    "type:roadmap",
    "type:rfc",
    "type:design",
    "status:needs-split",
    "status:partially-done",
    "status:reference",
];

/// Decide whether a PR may be merged based on a snapshot of its state.
///
/// Gate ordering (hard fails first, then soft waits):
///
/// 1. `state != Open`             → Blocked (already closed/merged)
/// 2. `is_draft`                  → Blocked
/// 3. `mergeable == Conflicting`  → Blocked
/// 4. `review_decision == ChangesRequested` → Blocked
/// 5. `checks == Failure`         → Blocked
/// 6. `mergeable == Unknown`      → Pending (GitHub still computing)
/// 7. `checks == Pending`         → Pending
/// 8. `review_decision == ReviewRequired` → Pending
/// 9. otherwise                   → Ready
///
/// A `Blocked` decision MAY include multiple reasons (e.g. a draft PR with
/// failing checks lists both, so the operator gets the full picture in one
/// `github_merge_blocked` event). A `Pending` decision likewise lists every
/// gate the caller is still waiting on, so the polling loop can surface
/// progress.
#[cfg(test)]
pub fn evaluate_merge_gate(pr: &PrState) -> MergeDecision {
    evaluate_merge_gate_with_policy(pr, MergeGatePolicy::standard())
}

pub fn evaluate_merge_gate_with_policy(pr: &PrState, policy: MergeGatePolicy) -> MergeDecision {
    let mut blocked: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();

    match pr.state {
        PrLifecycleState::Open => {}
        PrLifecycleState::Closed => blocked.push("state:closed".to_string()),
        PrLifecycleState::Merged => blocked.push("state:merged".to_string()),
    }
    if pr.is_draft {
        blocked.push("draft".to_string());
    }
    match pr.mergeable {
        Mergeable::Mergeable => {}
        Mergeable::Conflicting => blocked.push("mergeable:conflicting".to_string()),
        Mergeable::Unknown => pending.push("mergeable:unknown".to_string()),
    }
    match pr.review_decision {
        Some(ReviewDecision::ChangesRequested) => {
            blocked.push("review:changes_requested".to_string())
        }
        Some(ReviewDecision::ReviewRequired) => pending.push("review:required".to_string()),
        Some(ReviewDecision::Approved) => {}
        None => {
            if policy.require_review_approval && !policy.allow_missing_review_decision {
                pending.push("review:missing_decision".to_string());
            }
        }
    }
    match pr.checks {
        ChecksState::Failure => blocked.push("checks:failure".to_string()),
        ChecksState::Pending => pending.push("checks:pending".to_string()),
        ChecksState::Skipped => {
            if policy.require_checks && !policy.allow_missing_checks {
                pending.push("checks:skipped".to_string());
            }
        }
        ChecksState::Success => {}
        ChecksState::None => {
            if policy.require_checks && !policy.allow_missing_checks {
                pending.push("checks:none".to_string());
            }
        }
    }
    if policy.require_head_consistency {
        match pr.head_consistent {
            Some(true) => {}
            Some(false) => blocked.push("head:consistency_mismatch".to_string()),
            None => pending.push("head:consistency_unavailable".to_string()),
        }
    }
    if policy.block_protected_umbrella_close {
        for entry in &pr.closing_issue_labels {
            if let Some(matched_label) = PROTECTED_NO_CLOSE_LABELS
                .iter()
                .find(|protected| entry.labels.iter().any(|label| label == **protected))
            {
                blocked.push(format!(
                    "closes_protected:{}:{}",
                    entry.reference, matched_label
                ));
            }
        }
    }

    if !blocked.is_empty() {
        return MergeDecision::Blocked { reasons: blocked };
    }
    if !pending.is_empty() {
        return MergeDecision::Pending {
            waiting_on: pending,
        };
    }
    MergeDecision::Ready
}
