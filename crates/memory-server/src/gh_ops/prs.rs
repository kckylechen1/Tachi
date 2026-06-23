use super::*;

pub(in crate::gh_ops) async fn handle_gh_pr_read(
    server: &MemoryServer,
    params: GhPrReadParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["pr", "view", &params.pr_number.to_string()])
        .args(["--repo", &params.repo])
        .args([
            "--json",
            "number,title,state,body,author,labels,reviewDecision,mergeable,additions,deletions,changedFiles,headRefName,baseRefName,createdAt,updatedAt",
        ]);

    let output = run_gh(cmd, &token)?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_pr_read",
        "repo": params.repo,
        "pr_number": params.pr_number,
        "result": serde_json::from_str::<serde_json::Value>(&output).unwrap_or(json!(output)),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(in crate::gh_ops) async fn handle_gh_pr_list(
    server: &MemoryServer,
    params: GhPrListParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["pr", "list"])
        .args(["--repo", &params.repo])
        .args(["--state", &params.state])
        .args(["--limit", &params.limit.to_string()])
        .args([
            "--json",
            "number,title,state,author,labels,headRefName,createdAt",
        ]);

    let output = run_gh(cmd, &token)?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_pr_list",
        "repo": params.repo,
        "result": serde_json::from_str::<serde_json::Value>(&output).unwrap_or(json!(output)),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
