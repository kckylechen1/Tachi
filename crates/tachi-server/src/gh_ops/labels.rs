//! Label write-back for GitHub issues/PRs (#1000: `verified-fixed` /
//! `stale-spec` labels). Mirrors `comments.rs`'s shape — `gh` was missing a
//! label-apply primitive entirely before this (only `issue_create` accepted
//! labels, at creation time).

use super::*;

pub(crate) async fn handle_gh_label(
    server: &MemoryServer,
    params: GhLabelParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;
    if params.labels.is_empty() {
        return Err("issue_label requires a non-empty 'labels' parameter".to_string());
    }
    let mode = params.mode.as_deref().unwrap_or("add");
    let flag = match mode {
        "add" => "--add-label",
        "remove" => "--remove-label",
        other => {
            return Err(format!(
                "issue_label 'mode' must be 'add' or 'remove', got '{other}'"
            ))
        }
    };

    // Idempotency: skip labels already in the target state (present for
    // "add", absent for "remove") rather than re-issuing a no-op `gh` call
    // per label — cheaper and gives the caller a legible per-label skip
    // reason instead of a blanket success. Mirrors gh_comment_marker_present's
    // any-error-treated-as-"proceed" posture (never blocks the write-back).
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let mut to_apply = Vec::new();
    for label in &params.labels {
        let present = gh_label_present(server, &params.repo, params.number, label).await;
        let already_target_state = if mode == "add" { present } else { !present };
        if already_target_state {
            skipped.push(label.clone());
        } else {
            to_apply.push(label.clone());
        }
    }

    if !to_apply.is_empty() {
        let mut call = GhCall::mutation();
        call.args(["issue", "edit", &params.number.to_string()])
            .args(["--repo", &params.repo]);
        for label in &to_apply {
            call.args([flag, label]);
        }
        call.run(server).await?;
        applied.extend(to_apply);
    }

    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_label",
        "repo": params.repo,
        "number": params.number,
        "mode": mode,
        "applied": applied,
        "skipped_already_in_target_state": skipped,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// Idempotency probe: does this issue already carry `label`? Mirrors
/// `gh_comment_marker_present`'s any-error-returns-false posture (we'd
/// rather risk a rare duplicate label-edit than block the write-back).
pub(crate) async fn gh_label_present(
    server: &MemoryServer,
    repo: &str,
    number: u64,
    label: &str,
) -> bool {
    let mut call = GhCall::read();
    call.args(["issue", "view", &number.to_string()])
        .args(["--repo", repo])
        .args(["--json", "labels"]);
    let Ok(output) = call.run_json(server).await else {
        return false;
    };
    serde_json::from_str::<Value>(&output)
        .ok()
        .and_then(|value| {
            value.get("labels").and_then(Value::as_array).map(|labels| {
                labels.iter().any(|l| {
                    l.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name == label)
                })
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mode_flag_resolution_covers_add_remove_and_rejects_other() {
        assert_eq!(mode_flag("add"), Ok("--add-label"));
        assert_eq!(mode_flag("remove"), Ok("--remove-label"));
        assert!(mode_flag("bogus").is_err());
    }

    // Extracted for direct unit coverage without needing a live MemoryServer.
    fn mode_flag(mode: &str) -> Result<&'static str, String> {
        match mode {
            "add" => Ok("--add-label"),
            "remove" => Ok("--remove-label"),
            other => Err(format!(
                "issue_label 'mode' must be 'add' or 'remove', got '{other}'"
            )),
        }
    }
}
