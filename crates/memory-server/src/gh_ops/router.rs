use super::*;

pub(crate) async fn handle_tachi_gh(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    let action = params.action.clone();
    let raw = match action.as_str() {
        "repo_view" => {
            handle_gh_repo_view(
                server,
                GhRepoViewParams {
                    repo: required_repo(&params, "repo_view")?,
                },
            )
            .await
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
                    repo: required_repo(&params, "issue_list")?,
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
                    repo: required_repo(&params, "issue_read")?,
                    issue_number: number,
                },
            )
            .await
        }
        "issue_create" => {
            let repo = required_repo(&params, "issue_create")?;
            let title = params.title.ok_or("issue_create requires 'title' parameter")?;
            handle_gh_issue_create(
                server,
                GhIssueCreateParams {
                    repo,
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
                    repo: required_repo(&params, "issue_comment")?,
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
                    repo: required_repo(&params, "pr_comment")?,
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
                    repo: required_repo(&params, "pr_list")?,
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
                    repo: required_repo(&params, "pr_read")?,
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
                    repo: required_repo(&params, "pr_comments")?,
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
                    repo: required_repo(&params, "pr_review_digest")?,
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
            let repo = required_repo(&params, "safe_merge")?;
            handle_github_safe_merge(
                &client,
                &repo,
                number,
                strategy,
                effective_safe_merge_dry_run(params.confirm, params.dry_run),
                params.flow_id.as_deref(),
                policy,
            )
            .await
        }
        "ship" => handle_github_ship(server, &params).await,
        "link_pr" => {
            let task_params = lifecycle_task_params(&params)?;
            Box::pin(crate::task_lifecycle::handle_task_link_pr(
                server,
                &task_params,
            ))
            .await
        }
        "pr_status" => {
            let task_params = lifecycle_task_params(&params)?;
            let target = crate::task_lifecycle::resolve_task_pr_target(&task_params).map_err(|_| {
                "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
                    .to_string()
            })?;
            let policy = parse_merge_gate_policy(task_params.merge_policy.as_deref())?;
            let client = CliGhClient { server };
            handle_github_safe_merge(
                &client,
                &target.repo,
                target.number,
                MergeStrategy::Squash,
                true,
                task_params.flow_id.as_deref(),
                policy,
            )
            .await
        }
        "pr_handoff" => {
            let task_params = lifecycle_task_params(&params)?;
            crate::task_lifecycle::handle_task_pr_handoff(&task_params)
        }
        "release_note" => {
            let task_params = lifecycle_task_params(&params)?;
            Box::pin(crate::task_lifecycle::handle_task_release_note(
                server,
                &task_params,
            ))
            .await
        }
        other => Err(format!(
            "Unknown action '{}'. Expected: repo_view, issue_list, issue_read, issue_create, issue_comment, pr_list, pr_read, pr_comments, pr_comment, pr_review_digest, safe_merge, ship, link_pr, pr_status, pr_handoff, release_note",
            other
        )),
    }?;
    normalize_gh_response(&action, &raw)
}

fn required_repo(params: &TachiGhParams, action: &str) -> Result<String, String> {
    params
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{action} requires 'repo' in owner/repo format"))
}

fn lifecycle_task_params(
    params: &TachiGhParams,
) -> Result<crate::tool_params::TachiTaskParams, String> {
    let value = serde_json::to_value(params)
        .map_err(|err| format!("serialize tachi_gh lifecycle params: {err}"))?;
    serde_json::from_value(value)
        .map_err(|err| format!("convert tachi_gh lifecycle params to tachi_task params: {err}"))
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
