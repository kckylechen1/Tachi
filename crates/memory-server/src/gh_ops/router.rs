use super::*;

pub(crate) async fn handle_tachi_gh(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    let action = params.action.clone();
    let raw = match action.as_str() {
        "repo_view" => {
            handle_gh_repo_view(server, GhRepoViewParams { repo: params.repo }).await
        }
        "issue_list" => {
            // gh CLI accepts `--label` multiple times or a single comma-separated
            // value. We normalize to comma-separated to preserve all labels the
            // caller passed; taking `.first()` silently dropped extras.
            let labels_csv = if params.labels.is_empty() {
                None
            } else {
                Some(params.labels.join(","))
            };
            handle_gh_issue_list(
                server,
                GhIssueListParams {
                    repo: params.repo,
                    state: params.state.unwrap_or_else(|| "open".to_string()),
                    labels: labels_csv,
                    limit: params.limit.unwrap_or(30),
                },
            )
            .await
        }
        "issue_read" => {
            let number = params.number.ok_or("issue_read requires 'number' parameter")?;
            handle_gh_issue_read(
                server,
                GhIssueReadParams {
                    repo: params.repo,
                    issue_number: number,
                },
            )
            .await
        }
        "issue_create" => {
            let title = params.title.ok_or("issue_create requires 'title' parameter")?;
            handle_gh_issue_create(
                server,
                GhIssueCreateParams {
                    repo: params.repo,
                    title,
                    body: params.body,
                    labels: params.labels,
                },
            )
            .await
        }
        "issue_comment" => {
            let number = params
                .number
                .ok_or("issue_comment requires 'number' parameter")?;
            handle_gh_comment(
                server,
                "issue",
                GhCommentParams {
                    repo: params.repo,
                    number,
                    body: params.body,
                    dry_run: params.dry_run.unwrap_or(false),
                },
            )
            .await
        }
        "pr_comment" => {
            let number = params
                .number
                .ok_or("pr_comment requires 'number' parameter (PR number)")?;
            handle_gh_comment(
                server,
                "pr",
                GhCommentParams {
                    repo: params.repo,
                    number,
                    body: params.body,
                    dry_run: params.dry_run.unwrap_or(false),
                },
            )
            .await
        }
        "pr_list" => {
            handle_gh_pr_list(
                server,
                GhPrListParams {
                    repo: params.repo,
                    state: params.state.unwrap_or_else(|| "open".to_string()),
                    limit: params.limit.unwrap_or(30),
                },
            )
            .await
        }
        "pr_read" => {
            let number = params.number.ok_or("pr_read requires 'number' parameter")?;
            handle_gh_pr_read(
                server,
                GhPrReadParams {
                    repo: params.repo,
                    pr_number: number,
                },
            )
            .await
        }
        "pr_comments" => {
            let number = params
                .number
                .ok_or("pr_comments requires 'number' parameter (PR number)")?;
            handle_gh_pr_comments(
                server,
                GhPrCommentsParams {
                    repo: params.repo,
                    pr_number: number,
                },
            )
            .await
        }
        "pr_review_digest" => {
            let number = params
                .number
                .ok_or("pr_review_digest requires 'number' parameter (PR number)")?;
            handle_gh_pr_review_digest(
                server,
                GhPrCommentsParams {
                    repo: params.repo,
                    pr_number: number,
                },
                params.author_filter,
                params.write_digest.unwrap_or(true),
            )
            .await
        }
        "safe_merge" => {
            let number = params
                .number
                .ok_or("safe_merge requires 'number' parameter (PR number)")?;
            let strategy = parse_merge_strategy(params.merge_strategy.as_deref())?;
            let policy = parse_merge_gate_policy(params.merge_policy.as_deref())?;
            let client = CliGhClient { server };
            handle_github_safe_merge(
                &client,
                &params.repo,
                number,
                strategy,
                effective_safe_merge_dry_run(params.confirm, params.dry_run),
                params.flow_id.as_deref(),
                policy,
            )
            .await
        }
        other => Err(format!(
            "Unknown action '{}'. Expected: repo_view, issue_list, issue_read, issue_create, issue_comment, pr_list, pr_read, pr_comments, pr_comment, pr_review_digest, safe_merge",
            other
        )),
    }?;
    normalize_gh_response(&action, &raw)
}

fn normalize_gh_response(action: &str, raw: &str) -> Result<String, String> {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return Ok(raw.to_string());
    };
    if let Some(obj) = value.as_object_mut() {
        obj.entry("status".to_string())
            .or_insert_with(|| Value::String("completed".to_string()));
        obj.entry("action".to_string())
            .or_insert_with(|| Value::String(action.to_string()));
    }
    serde_json::to_string(&value).map_err(|err| format!("serialize normalized gh response: {err}"))
}
