use super::*;

pub(in crate::gh_ops) async fn handle_gh_repo_view(
    server: &MemoryServer,
    params: GhRepoViewParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let mut call = GhCall::read();
    call.args(["repo", "view", &params.repo]).args([
        "--json",
        "name,owner,description,url,defaultBranchRef,stargazerCount,forkCount,isPrivate,languages,createdAt,updatedAt",
    ]);

    let output = call.run(server).await?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_repo_view",
        "repo": params.repo,
        "result": serde_json::from_str::<serde_json::Value>(&output).unwrap_or(json!(output)),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
