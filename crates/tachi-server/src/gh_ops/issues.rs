use super::*;
use std::time::Duration;

/// #1071: bounded, non-mutating single-issue read for callers OUTSIDE the
/// `tachi_gh` mutation surface (currently `tachi_memory(action='ask')`'s
/// exact current-work anchor grounding) that must never risk hanging their
/// caller's hot path.
///
/// [`run_gh`] (used by [`handle_gh_issue_read`] above) shells a *synchronous*
/// `std::process::Command`, with no timeout and no kill-on-drop — safe for
/// the existing explicit, user-triggered `tachi_gh` mutation actions, but
/// NOT safe to call unconditionally from a read-only recall/briefing hot
/// path: a `gh` invocation that stalls on network (observed directly during
/// this leaf's own development: `gh auth status` hung past 5s against this
/// machine's network) would block the calling async task indefinitely with
/// no way to recover. This function reuses [`build_gh_command`]'s
/// security-vetted token/env setup verbatim (zero duplicated credential
/// logic) but runs it through `tokio::process::Command` with
/// `kill_on_drop(true)`, raced against an explicit [`ANCHOR_GH_TIMEOUT`] —
/// the same "external CLI, bounded and killable" shape
/// `chat_lanes::claude_cli::call_claude_cli` already uses for the same class
/// of risk. On timeout the child process is killed (not orphaned) and the
/// caller gets a plain `Err`, never a hang.
pub(crate) const ANCHOR_GH_TIMEOUT: Duration = Duration::from_secs(6);

pub(crate) async fn read_issue_snapshot_bounded(
    server: &MemoryServer,
    repo: &str,
    issue_number: u64,
) -> Result<Value, String> {
    validate_repo(repo)?;
    // #1071 fix-round checkpoint 7: `build_gh_command` (which resolves `gh`
    // via a synchronous `which gh` shell-out and reads the vault token) used
    // to run BEFORE `ANCHOR_GH_TIMEOUT` started, so a stall in either of
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

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "view", &params.issue_number.to_string()])
        .args(["--repo", &params.repo])
        .args([
            "--json",
            "number,title,state,body,author,labels,assignees,createdAt,updatedAt,comments,milestone",
        ]);

    let output = run_gh(cmd, &token)?;
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

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", &params.repo])
        .args(["--state", &params.state])
        .args(["--limit", &params.limit.to_string()])
        .args(["--json", "number,title,state,author,labels,createdAt"]);

    if let Some(ref labels) = params.labels {
        cmd.args(["--label", labels]);
    }

    let output = run_gh(cmd, &token)?;
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

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "create"])
        .args(["--repo", &params.repo])
        .args(["--title", &params.title]);

    let mut _body_file = None;
    if let Some(ref body) = params.body {
        _body_file = Some(attach_gh_body_file(&mut cmd, body)?);
    }

    for label in &params.labels {
        cmd.args(["--label", label]);
    }

    let output = run_gh(cmd, &token)?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_issue_create",
        "repo": params.repo,
        "result": output.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
