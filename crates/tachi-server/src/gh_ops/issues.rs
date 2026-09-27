use super::*;
use std::time::Duration;

/// #1071: bounded, non-mutating single-issue read for callers OUTSIDE the
/// `tachi_gh` mutation surface (currently `tachi_memory(action='ask')`'s
/// exact current-work anchor grounding) that must never risk hanging their
/// caller's hot path.
///
/// Every `gh` call now runs on the shared bounded executor ([`GhCall`],
/// audit E1), whose default read bound (`GH_READ_TIMEOUT`) is sized for an
/// explicit `tachi_gh` action. A read-only recall/briefing hot path needs a
/// much tighter bound: a `gh` invocation that stalls on network (observed
/// directly during this leaf's own development: `gh auth status` hung past
/// 5s against this machine's network) must not hold the caller for long.
/// This function uses the same executor and credential/env hardening with
/// the explicit [`ANCHOR_GH_TIMEOUT`]. On timeout the child process is killed
/// and reaped (not orphaned) and the caller gets a plain `Err`, never a hang.
pub(crate) const ANCHOR_GH_TIMEOUT: Duration = Duration::from_secs(6);

pub(crate) async fn read_issue_snapshot_bounded(
    server: &MemoryServer,
    repo: &str,
    issue_number: u64,
) -> Result<Value, String> {
    validate_repo(repo)?;
    // #1071 fix-round checkpoint 7: `build_gh_command` (which resolves `gh`
    // and reads the vault token; the `gh` lookup is in-process since audit
    // E1, it used to be a synchronous `which gh` shell-out) used to run BEFORE `ANCHOR_GH_TIMEOUT` started, so a stall in either of
    // those steps was completely unbounded — exactly the class of hang this
    // function exists to prevent (see module doc's `gh auth status` anecdote).
    // `run_gh_json_bounded` keeps that whole sequence under one bound while
    // sharing the exact same hardened path with CurrentTruth refresh reads.
    run_gh_json_bounded(
        server,
        vec![
            "issue".to_string(),
            "view".to_string(),
            issue_number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "number,title,state,body,labels,milestone,updatedAt,comments".to_string(),
        ],
        ANCHOR_GH_TIMEOUT,
        "gh issue view",
    )
    .await
}

pub(in crate::gh_ops) async fn handle_gh_issue_read(
    server: &MemoryServer,
    params: GhIssueReadParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let mut call = GhCall::read();
    call.args(["issue", "view", &params.issue_number.to_string()])
        .args(["--repo", &params.repo])
        .args([
            "--json",
            "number,title,state,body,author,labels,assignees,createdAt,updatedAt,comments,milestone",
        ]);

    let output = call.run(server).await?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_read",
        "repo": params.repo,
        "issue_number": params.issue_number,
        "result": serde_json::from_str::<serde_json::Value>(&output).unwrap_or(json!(output)),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(in crate::gh_ops) async fn handle_gh_issue_list(
    server: &MemoryServer,
    params: GhIssueListParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let mut call = GhCall::bulk_read();
    call.args(["issue", "list"])
        .args(["--repo", &params.repo])
        .args(["--state", &params.state])
        .args(["--limit", &params.limit.to_string()])
        .args(["--json", "number,title,state,author,labels,createdAt"]);

    if let Some(ref labels) = params.labels {
        call.args(["--label", labels]);
    }

    let output = call.run(server).await?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_list",
        "repo": params.repo,
        "result": serde_json::from_str::<serde_json::Value>(&output).unwrap_or(json!(output)),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(in crate::gh_ops) async fn handle_gh_issue_create(
    server: &MemoryServer,
    params: GhIssueCreateParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let mut call = GhCall::mutation();
    call.args(["issue", "create"])
        .args(["--repo", &params.repo])
        .args(["--title", &params.title]);

    if let Some(ref body) = params.body {
        call.attach_body_file(body)?;
    }

    for label in &params.labels {
        call.args(["--label", label]);
    }

    let output = call.run(server).await?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_create",
        "repo": params.repo,
        "result": output.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
