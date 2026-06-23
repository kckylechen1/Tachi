use super::super::*;
use super::api::fetch_gh_pr_comments;
use super::artifacts::write_pr_review_digest_artifacts;
use super::build::build_pr_review_digest;

pub(in crate::gh_ops) async fn handle_gh_pr_comments(
    server: &MemoryServer,
    params: GhPrCommentsParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;
    let (reviews, inline_comments, comments) =
        fetch_gh_pr_comments(server, &params.repo, params.pr_number)?;

    serde_json::to_string(&json!({
        "tool": "tachi_gh_pr_comments",
        "repo": params.repo,
        "pr_number": params.pr_number,
        "result": {
            "reviews": reviews,
            "inline_comments": inline_comments,
            "comments": comments,
        },
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(in crate::gh_ops) async fn handle_gh_pr_review_digest(
    server: &MemoryServer,
    params: GhPrCommentsParams,
    author_filter: Option<String>,
    write_digest: bool,
) -> Result<String, String> {
    validate_repo(&params.repo)?;
    let (_, _, comments) = fetch_gh_pr_comments(server, &params.repo, params.pr_number)?;
    let author_filter = author_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_REVIEW_AUTHOR_FILTER);
    let digest = build_pr_review_digest(&params.repo, params.pr_number, author_filter, &comments);
    let artifacts = if write_digest {
        write_pr_review_digest_artifacts(&digest)?
    } else {
        Value::Null
    };

    serde_json::to_string(&json!({
        "tool": "tachi_gh_pr_review_digest",
        "repo": params.repo,
        "pr_number": params.pr_number,
        "write_digest": write_digest,
        "artifacts": artifacts,
        "result": digest,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
