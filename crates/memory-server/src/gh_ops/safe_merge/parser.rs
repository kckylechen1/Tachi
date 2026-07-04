use super::*;

/// Parse the `gh pr view --json number,state,mergeable,reviewDecision,isDraft,headRefOid,closingIssuesReferences`
/// payload into a `PrState`. Pulled out as a free function so unit tests can
/// exercise the JSON shape without spawning `gh`.
pub(in crate::gh_ops) fn parse_pr_view_json(
    v: &serde_json::Value,
    checks: Vec<crate::gh_safe_merge::CheckRun>,
) -> Result<PrState, String> {
    let number = v
        .get("number")
        .and_then(|n| n.as_u64())
        .ok_or("pr_view: missing number")?;
    let state_raw = v
        .get("state")
        .and_then(|s| s.as_str())
        .ok_or("pr_view: missing state")?;
    let state = match state_raw {
        "OPEN" => PrLifecycleState::Open,
        "CLOSED" => PrLifecycleState::Closed,
        "MERGED" => PrLifecycleState::Merged,
        other => return Err(format!("pr_view: unknown state '{}'", other)),
    };
    let mergeable = match v.get("mergeable").and_then(|m| m.as_str()).unwrap_or("") {
        "MERGEABLE" => Mergeable::Mergeable,
        "CONFLICTING" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    let review_decision = v
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .and_then(|s| match s {
            "APPROVED" => Some(ReviewDecision::Approved),
            "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
            "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
            "" => None,
            _ => Some(ReviewDecision::ReviewRequired),
        });
    let is_draft = v.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
    let head_sha = v
        .get("headRefOid")
        .and_then(|h| h.as_str())
        .unwrap_or_default()
        .to_string();
    let linked_issue_refs = v
        .get("closingIssuesReferences")
        .and_then(|refs| refs.as_array())
        .map(|refs| {
            refs.iter()
                .filter_map(|issue| {
                    if let Some(url) = issue.get("url").and_then(|u| u.as_str()) {
                        Some(url.to_string())
                    } else {
                        issue
                            .get("number")
                            .and_then(|n| n.as_u64())
                            .map(|n| format!("#{n}"))
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(PrState {
        number,
        state,
        mergeable,
        review_decision,
        checks: ChecksState::aggregate(&checks),
        is_draft,
        head_sha,
        linked_issue_refs,
        closing_issue_labels: Vec::new(),
        head_consistent: None,
    })
}
