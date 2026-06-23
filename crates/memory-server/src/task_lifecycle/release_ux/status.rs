use super::*;

pub(in crate::task_lifecycle) fn pr_snapshot_from_status(status: &Value) -> Option<PrSnapshot> {
    let github = status.get("github")?;
    let repo = github.get("repo").and_then(Value::as_str)?.to_string();
    let number = github.get("pr_number").and_then(Value::as_u64)?;
    Some(PrSnapshot {
        repo: repo.clone(),
        number,
        title: github
            .get("pr_title")
            .and_then(Value::as_str)
            .unwrap_or("GitHub PR")
            .to_string(),
        state: github
            .get("pr_state")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        url: github
            .get("pr_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}")),
        head_ref: github
            .get("head_ref")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        base_ref: github
            .get("base_ref")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        review_decision: review_state(status),
        mergeable: github
            .get("mergeable")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
    })
}

pub(in crate::task_lifecycle) fn release_note_issue_ref(status: &Value) -> Option<String> {
    status
        .get("issue_ref")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| github_string(status, "issue_ref"))
        .or_else(|| {
            let github = status.get("github")?;
            let repo = github.get("repo").and_then(Value::as_str)?;
            let number = github.get("issue_number").and_then(Value::as_u64)?;
            Some(format!("{repo}#{number}"))
        })
}

pub(in crate::task_lifecycle) fn release_note_pr_ref(status: &Value) -> Option<String> {
    status
        .get("pr_ref")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| github_string(status, "pr_ref"))
        .or_else(|| {
            let github = status.get("github")?;
            let repo = github.get("repo").and_then(Value::as_str)?;
            let number = github.get("pr_number").and_then(Value::as_u64)?;
            Some(format!("{repo}#{number}"))
        })
}

pub(in crate::task_lifecycle) fn github_string(status: &Value, key: &str) -> Option<String> {
    status
        .get("github")
        .and_then(|github| github.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

pub(in crate::task_lifecycle) fn review_state(status: &Value) -> Option<String> {
    status
        .get("github")
        .and_then(|github| github.get("review"))
        .and_then(|review| review.get("state"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}
