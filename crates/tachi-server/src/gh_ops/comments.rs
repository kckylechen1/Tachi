use super::*;

/// Post a comment to a GitHub issue or PR. `kind` is "issue" or "pr".
/// This is the write-back arc of the closure loop: closure results, review
/// verdicts, and reap notices land back on the source issue/PR. `dry_run`
/// returns a preview without posting.
/// Best-effort idempotency probe: does this issue/PR already carry a comment
/// containing `marker`? Lets the closure write-back avoid double-posting on
/// re-run (which would spam the issue and erode trust in the loop). Any error
/// (gh down, parse failure) returns false so the caller proceeds — we'd rather
/// risk a rare duplicate than silently swallow the write-back.
pub(crate) fn gh_comment_marker_present(
    server: &MemoryServer,
    kind: &str,
    repo: &str,
    number: u64,
    marker: &str,
) -> bool {
    let Ok((mut cmd, token)) = build_gh_command(server) else {
        return false;
    };
    cmd.args([kind, "view", &number.to_string()])
        .args(["--repo", repo])
        .args(["--json", "comments"]);
    let Ok(output) = run_gh_json(cmd, &token) else {
        return false;
    };
    serde_json::from_str::<Value>(&output)
        .ok()
        .and_then(|value| {
            value
                .get("comments")
                .and_then(Value::as_array)
                .map(|comments| {
                    comments.iter().any(|c| {
                        c.get("body")
                            .and_then(Value::as_str)
                            .is_some_and(|body| body.contains(marker))
                    })
                })
        })
        .unwrap_or(false)
}

pub(crate) async fn handle_gh_comment(
    server: &MemoryServer,
    kind: &str,
    params: GhCommentParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;
    let body = params.body.unwrap_or_default();
    if body.trim().is_empty() {
        return Err(format!(
            "{kind}_comment requires a non-empty 'body' parameter"
        ));
    }

    if params.dry_run {
        return serde_json::to_string(&json!({
            "tool": format!("tachi_gh_{kind}_comment"),
            "repo": params.repo,
            "number": params.number,
            "dry_run": true,
            "preview_body": body,
            "note": "dry_run=true: comment was NOT posted",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args([kind, "comment", &params.number.to_string()])
        .args(["--repo", &params.repo]);
    let _body_file = attach_gh_body_file(&mut cmd, &body)?;

    let output = run_gh(cmd, &token)?;
    serde_json::to_string(&json!({
        "tool": format!("tachi_gh_{kind}_comment"),
        "repo": params.repo,
        "number": params.number,
        "result": output.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
