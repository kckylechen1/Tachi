use super::super::*;
use super::normalize::{
    flatten_paginated_array, merge_pr_comment_entries, normalize_inline_comment_entry,
    normalize_review_entry,
};

pub(in crate::gh_ops) fn run_gh_api_paginated(
    server: &MemoryServer,
    endpoint: &str,
) -> Result<Value, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["api", "--paginate", "--slurp"]).arg(endpoint);

    let output = run_gh_json(cmd, &token)?;
    serde_json::from_str::<Value>(&output).map_err(|e| {
        format!(
            "parse gh api response from '{}': {e}; raw={}",
            endpoint,
            output.chars().take(500).collect::<String>()
        )
    })
}

pub(in crate::gh_ops) fn fetch_gh_pr_comments(
    server: &MemoryServer,
    repo: &str,
    pr_number: u64,
) -> Result<GhPrCommentsBundle, String> {
    let reviews_endpoint = format!("repos/{}/pulls/{}/reviews?per_page=100", repo, pr_number);
    let inline_comments_endpoint =
        format!("repos/{}/pulls/{}/comments?per_page=100", repo, pr_number);
    let reviews =
        flatten_paginated_array(run_gh_api_paginated(server, &reviews_endpoint)?, "reviews")?
            .into_iter()
            .map(normalize_review_entry)
            .collect::<Vec<_>>();
    let inline_comments = flatten_paginated_array(
        run_gh_api_paginated(server, &inline_comments_endpoint)?,
        "inline_comments",
    )?
    .into_iter()
    .map(normalize_inline_comment_entry)
    .collect::<Vec<_>>();
    let comments = merge_pr_comment_entries(reviews.clone(), inline_comments.clone());
    Ok((reviews, inline_comments, comments))
}
