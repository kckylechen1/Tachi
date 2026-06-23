use super::*;

pub(super) fn run_gh_api_paginated(server: &MemoryServer, endpoint: &str) -> Result<Value, String> {
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

pub(super) fn flatten_paginated_array(value: Value, label: &str) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) if items.iter().all(Value::is_array) => {
            let mut flattened = Vec::new();
            for page in items {
                if let Value::Array(page_items) = page {
                    flattened.extend(page_items);
                }
            }
            Ok(flattened)
        }
        Value::Array(items) => Ok(items),
        other => Err(format!("{label} response was not an array: {other}")),
    }
}

pub(super) fn normalize_review_entry(entry: Value) -> Value {
    json!({
        "kind": "review",
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "review_id": entry.get("id").cloned().unwrap_or(Value::Null),
        "author": entry
            .get("user")
            .and_then(|user| user.get("login"))
            .cloned()
            .unwrap_or(Value::Null),
        "body": entry.get("body").cloned().unwrap_or(Value::Null),
        "state": entry.get("state").cloned().unwrap_or(Value::Null),
        "submitted_at": entry.get("submitted_at").cloned().unwrap_or(Value::Null),
        "created_at": entry.get("submitted_at").cloned().unwrap_or(Value::Null),
        "url": entry.get("html_url").cloned().unwrap_or(Value::Null),
    })
}

pub(super) fn normalize_inline_comment_entry(entry: Value) -> Value {
    json!({
        "kind": "inline_comment",
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "comment_id": entry.get("id").cloned().unwrap_or(Value::Null),
        "review_id": entry
            .get("pull_request_review_id")
            .cloned()
            .unwrap_or(Value::Null),
        "in_reply_to_id": entry.get("in_reply_to_id").cloned().unwrap_or(Value::Null),
        "author": entry
            .get("user")
            .and_then(|user| user.get("login"))
            .cloned()
            .unwrap_or(Value::Null),
        "path": entry.get("path").cloned().unwrap_or(Value::Null),
        "line": entry.get("line").cloned().unwrap_or(Value::Null),
        "start_line": entry.get("start_line").cloned().unwrap_or(Value::Null),
        "side": entry.get("side").cloned().unwrap_or(Value::Null),
        "body": entry.get("body").cloned().unwrap_or(Value::Null),
        "created_at": entry.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at": entry.get("updated_at").cloned().unwrap_or(Value::Null),
        "url": entry.get("html_url").cloned().unwrap_or(Value::Null),
    })
}

pub(super) fn comment_entry_time(entry: &Value) -> Option<&str> {
    entry
        .get("created_at")
        .and_then(Value::as_str)
        .or_else(|| entry.get("submitted_at").and_then(Value::as_str))
}

pub(super) fn merge_pr_comment_entries(
    mut reviews: Vec<Value>,
    mut inline_comments: Vec<Value>,
) -> Vec<Value> {
    let mut comments = Vec::with_capacity(reviews.len() + inline_comments.len());
    comments.append(&mut reviews);
    comments.append(&mut inline_comments);
    comments.sort_by(|left, right| {
        comment_entry_time(left)
            .unwrap_or("")
            .cmp(comment_entry_time(right).unwrap_or(""))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
                    .cmp(&right.get("id").and_then(Value::as_i64).unwrap_or_default())
            })
    });
    comments
}

pub(super) fn review_digest_root() -> PathBuf {
    if let Ok(root) = std::env::var("TACHI_REVIEW_ROOT") {
        return PathBuf::from(root);
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.join(".tachi").join("reviews");
    }
    std::env::temp_dir().join("tachi").join("reviews")
}

pub(super) fn safe_path_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

pub(super) fn repo_review_segment(repo: &str) -> String {
    repo.split('/')
        .map(safe_path_segment)
        .collect::<Vec<_>>()
        .join("__")
}

pub(super) fn comment_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn first_meaningful_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("```")
                && !line.starts_with("---")
                && !line.starts_with("<!--")
                && !line.starts_with("![")
        })
        .unwrap_or(body.trim())
        .chars()
        .take(220)
        .collect()
}

pub(super) fn lower_contains_any(lower_haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| lower_haystack.contains(needle))
}

pub(super) fn classify_review_comment(body: &str, path: Option<&str>) -> &'static str {
    let combined = match path {
        Some(path) => format!("{body}\n{path}"),
        None => body.to_string(),
    };
    let lower = combined.to_ascii_lowercase();
    if lower_contains_any(
        &lower,
        &[
            "security",
            "secret",
            "token",
            "credential",
            "injection",
            "permission",
            "auth",
        ],
    ) {
        "security"
    } else if lower_contains_any(
        &lower,
        &[
            "panic",
            "bug",
            "incorrect",
            "wrong",
            "race",
            "deadlock",
            "lock",
            "fail",
            "regression",
            "root cause",
        ],
    ) {
        "correctness"
    } else if lower_contains_any(
        &lower,
        &["test", "coverage", "assert", "fixture", "mock", "case"],
    ) {
        "tests"
    } else if lower_contains_any(
        &lower,
        &[
            "api",
            "schema",
            "contract",
            "compat",
            "breaking",
            "parameter",
            "field",
        ],
    ) {
        "api-contract"
    } else if lower_contains_any(
        &lower,
        &[
            "maintain",
            "duplicate",
            "complex",
            "refactor",
            "simpl",
            "readability",
        ],
    ) {
        "maintainability"
    } else if lower_contains_any(&lower, &["nit", "style", "format", "typo", "naming"]) {
        "style"
    } else {
        "unclassified"
    }
}

pub(super) fn author_matches_filter(comment: &Value, lower_filter: &str) -> bool {
    if lower_filter.is_empty() {
        return true;
    }
    comment
        .get("author")
        .and_then(Value::as_str)
        .map(|author| author.to_ascii_lowercase().contains(lower_filter))
        .unwrap_or(false)
}

pub(super) fn infer_future_rule(category: &str, path: Option<&str>, body: &str) -> String {
    let scope = path.unwrap_or("similar code");
    let line = first_meaningful_line(body);
    match category {
        "security" => format!("When changing {scope}, verify trust boundaries and secret handling: {line}"),
        "correctness" => format!("When changing {scope}, check this failure mode before shipping: {line}"),
        "tests" => format!("When changing {scope}, add or update regression coverage for: {line}"),
        "api-contract" => format!("When changing {scope}, preserve or explicitly migrate the API/schema contract: {line}"),
        "maintainability" => format!("When changing {scope}, keep the simpler local pattern and avoid this maintainability trap: {line}"),
        "style" => format!("Style-only review signal for {scope}; do not promote unless it repeats: {line}"),
        _ => format!("Review signal for {scope}; leader must triage before promotion: {line}"),
    }
}

pub(super) fn review_project_base_path(layer: &str, repo: &str) -> String {
    let repo = repo.trim_matches('/');
    format!("/{layer}/projects/{repo}")
}

pub(super) fn review_route_for_item(
    repo: &str,
    pr_number: u64,
    category: &str,
    path: Option<&str>,
    summary: &str,
    future_rule: &str,
) -> Value {
    let actionable = matches!(
        category,
        "security" | "correctness" | "tests" | "api-contract"
    );
    let reusable = matches!(
        category,
        "security" | "correctness" | "tests" | "api-contract" | "maintainability"
    );
    let primary_destination = if actionable {
        "github_issue"
    } else if reusable {
        "project_wiki"
    } else {
        "pr_comment"
    };
    let mut destinations = vec![json!({
        "destination": "pr_comment",
        "layer": "github_ref",
        "authority": "project_work_record",
        "when": "reply, resolve, or mark false-positive on the PR after leader verdict",
        "target_ref": format!("{repo}#{pr_number}"),
    })];

    if actionable {
        destinations.push(json!({
            "destination": "github_issue",
            "layer": "github_ref",
            "authority": "project_work_record",
            "when": "valid actionable project bug/task remains after the PR review pass",
            "title_hint": summary,
            "source_ref": format!("{repo}#{pr_number}"),
            "path": path,
        }));
    }

    if reusable {
        destinations.push(json!({
            "destination": "feedback_rule",
            "layer": "feedback_rule",
            "authority": "behavior_patch",
            "when": "the finding is a reusable prompt/process correction for future workers",
            "path_hint": format!("{}/review/{}", review_project_base_path("feedback", repo), category),
            "rule": future_rule,
        }));
        destinations.push(json!({
            "destination": "guide",
            "layer": "guide",
            "authority": "playbook",
            "when": "the finding changes reusable AgentReview or workflow SOP",
            "path_hint": "/guide/global/workflows/agent-review",
        }));
    }

    if !matches!(category, "style" | "unclassified") {
        destinations.push(json!({
            "destination": "project_wiki",
            "layer": "wiki",
            "authority": "advisory",
            "when": "the finding is a project-specific durable lesson after close_loop",
            "path_hint": format!("{}/lessons/pr-{pr_number}", review_project_base_path("wiki", repo)),
            "source_ref": format!("{repo}#{pr_number}"),
        }));
    }

    if category == "api-contract"
        || path.is_some_and(|path| path.starts_with("docs/") || path.starts_with("spec"))
    {
        destinations.push(json!({
            "destination": "repo_doc_ref",
            "layer": "repo_doc_ref",
            "authority": "canonical",
            "when": "the accepted fix changes canonical design, API, or spec truth",
            "path": path,
        }));
    }

    destinations.push(json!({
        "destination": "eval",
        "layer": "eval",
        "authority": "evidence",
        "when": "after leader verdict, record reviewer usefulness/false-positive signal",
        "source_ref": format!("{repo}#{pr_number}"),
    }));

    json!({
        "primary_destination": primary_destination,
        "promotion_requires": "leader_verdict",
        "destinations": destinations,
    })
}

pub(super) fn review_routing_plan(items: &[Value]) -> Value {
    let mut destination_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut routed_items = Vec::new();
    for item in items {
        let Some(routing) = item.get("routing") else {
            continue;
        };
        let routes = routing
            .get("destinations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for route in &routes {
            if let Some(destination) = route.get("destination").and_then(Value::as_str) {
                *destination_counts
                    .entry(destination.to_string())
                    .or_insert(0) += 1;
            }
        }
        routed_items.push(json!({
            "category": item.get("category").cloned().unwrap_or(Value::Null),
            "summary": item.get("summary").cloned().unwrap_or(Value::Null),
            "primary_destination": routing.get("primary_destination").cloned().unwrap_or(Value::Null),
            "destinations": routes
                .iter()
                .filter_map(|route| route.get("destination").and_then(Value::as_str))
                .collect::<Vec<_>>(),
        }));
    }

    json!({
        "status": if routed_items.is_empty() { "empty" } else { "needs_leader_verdict" },
        "authority_order": [
            "github_ref",
            "repo_doc_ref",
            "wiki",
            "guide",
            "feedback_rule",
            "eval"
        ],
        "destination_counts": destination_counts,
        "items": routed_items,
    })
}

pub(super) fn build_pr_review_digest(
    repo: &str,
    pr_number: u64,
    author_filter: &str,
    comments: &[Value],
) -> Value {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut items = Vec::new();
    let mut memory_candidates = Vec::new();
    let mut handbook_candidates = Vec::new();
    let lower_filter = author_filter.trim().to_ascii_lowercase();

    for comment in comments
        .iter()
        .filter(|comment| author_matches_filter(comment, &lower_filter))
    {
        let body = comment_text(comment, "body").unwrap_or_default();
        if body.is_empty() {
            continue;
        }
        let path = comment_text(comment, "path");
        let category = classify_review_comment(&body, path.as_deref());
        *counts.entry(category.to_string()).or_insert(0) += 1;
        let summary = first_meaningful_line(&body);
        let future_rule = infer_future_rule(category, path.as_deref(), &body);
        let routing = review_route_for_item(
            repo,
            pr_number,
            category,
            path.as_deref(),
            &summary,
            &future_rule,
        );
        let source = json!({
            "kind": comment.get("kind").cloned().unwrap_or(Value::Null),
            "id": comment.get("id").cloned().unwrap_or(Value::Null),
            "review_id": comment.get("review_id").cloned().unwrap_or(Value::Null),
            "author": comment.get("author").cloned().unwrap_or(Value::Null),
            "path": path,
            "line": comment.get("line").cloned().unwrap_or(Value::Null),
            "url": comment.get("url").cloned().unwrap_or(Value::Null),
            "created_at": comment.get("created_at").cloned().unwrap_or(Value::Null),
        });
        let item = json!({
            "source": source,
            "category": category,
            "verdict": "needs_leader_verdict",
            "summary": summary,
            "body": body,
            "future_rule": future_rule,
            "routing": routing,
        });

        memory_candidates.push(json!({
            "source": "github_pr_review",
            "repo": repo,
            "pr_number": pr_number,
            "category": category,
            "verdict": "needs_leader_verdict",
            "comment_id": item["source"]["id"],
            "path": item["source"]["path"],
            "line": item["source"]["line"],
            "summary": item["summary"],
            "future_rule": item["future_rule"],
        }));
        if !matches!(category, "style" | "unclassified") {
            handbook_candidates.push(json!({
                "category": category,
                "requires_verdict": true,
                "rule": item["future_rule"],
                "source": {
                    "repo": repo,
                    "pr_number": pr_number,
                    "comment_id": item["source"]["id"],
                    "path": item["source"]["path"],
                    "line": item["source"]["line"],
                    "url": item["source"]["url"],
                },
            }));
        }
        items.push(item);
    }

    let routing_plan = review_routing_plan(&items);

    json!({
        "repo": repo,
        "pr_number": pr_number,
        "author_filter": author_filter,
        "comment_count": items.len(),
        "counts": counts,
        "items": items,
        "routing_plan": routing_plan,
        "memory_candidates": memory_candidates,
        "handbook_candidates": handbook_candidates,
        "promotion_policy": {
            "raw": "keep raw/digest artifacts as evidence",
            "memory": "promote only valid or useful false-positive cases after leader verdict",
            "wiki": "promote repeated valid patterns into handbook/checklist rules",
        },
    })
}

pub(super) fn render_pr_review_digest_markdown(digest: &Value) -> String {
    let repo = digest.get("repo").and_then(Value::as_str).unwrap_or("");
    let pr_number = digest
        .get("pr_number")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let author_filter = digest
        .get("author_filter")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_REVIEW_AUTHOR_FILTER);
    let mut out = format!(
        "# PR Review Digest\n\nRepo: `{repo}`\nPR: `#{pr_number}`\nAuthor filter: `{author_filter}`\n\n\
         ## Triage Contract\n\n\
         - Mark each item as `valid`, `partially_valid`, `false_positive`, or `unresolved` before promoting.\n\
         - Store raw review artifacts here; save only distilled conclusions to memory.\n\
         - Promote repeated valid patterns to the Gemini PR review handbook or worker checklist.\n\n"
    );

    out.push_str("## Counts\n\n");
    if let Some(counts) = digest.get("counts").and_then(Value::as_object) {
        for (category, count) in counts {
            out.push_str(&format!("- `{category}`: {count}\n"));
        }
    }

    out.push_str("\n## Review Output Routing\n\n");
    if let Some(counts) = digest
        .pointer("/routing_plan/destination_counts")
        .and_then(Value::as_object)
    {
        for (destination, count) in counts {
            out.push_str(&format!("- `{destination}`: {count}\n"));
        }
    } else {
        out.push_str("- No route candidates.\n");
    }

    out.push_str("\n## Items\n\n");
    if let Some(items) = digest.get("items").and_then(Value::as_array) {
        for (idx, item) in items.iter().enumerate() {
            let category = item
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("unclassified");
            let summary = item.get("summary").and_then(Value::as_str).unwrap_or("");
            let path = item
                .pointer("/source/path")
                .and_then(Value::as_str)
                .unwrap_or("");
            let line = item.pointer("/source/line").and_then(Value::as_u64);
            let url = item
                .pointer("/source/url")
                .and_then(Value::as_str)
                .unwrap_or("");
            let future_rule = item
                .get("future_rule")
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(&format!(
                "### {}. `{}`\n\nVerdict: `needs_leader_verdict`\n\n",
                idx + 1,
                category
            ));
            if !path.is_empty() {
                match line {
                    Some(line) => out.push_str(&format!("Location: `{path}:{line}`\n\n")),
                    None => out.push_str(&format!("Location: `{path}`\n\n")),
                }
            }
            if !url.is_empty() {
                out.push_str(&format!("Source: {url}\n\n"));
            }
            out.push_str(&format!("Summary: {summary}\n\n"));
            out.push_str(&format!("Future rule candidate: {future_rule}\n\n"));
            if let Some(primary) = item
                .pointer("/routing/primary_destination")
                .and_then(Value::as_str)
            {
                out.push_str(&format!("Primary route: `{primary}`\n\n"));
            }
        }
    }
    out
}

pub(super) fn write_pr_review_digest_artifacts(digest: &Value) -> Result<Value, String> {
    let repo = digest
        .get("repo")
        .and_then(Value::as_str)
        .ok_or("digest missing repo")?;
    let pr_number = digest
        .get("pr_number")
        .and_then(Value::as_u64)
        .ok_or("digest missing pr_number")?;
    let dir = review_digest_root()
        .join(repo_review_segment(repo))
        .join(format!("pr-{pr_number}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create review digest dir {}: {e}", dir.display()))?;
    let digest_json_path = dir.join("digest.json");
    let digest_md_path = dir.join("digest.md");
    let serialized =
        serde_json::to_string_pretty(digest).map_err(|e| format!("serialize digest: {e}"))?;
    crate::utils::write_owner_only_file_atomic(
        &digest_json_path,
        format!("{serialized}\n").as_bytes(),
    )
    .map_err(|e| format!("write {}: {e}", digest_json_path.display()))?;
    let markdown = render_pr_review_digest_markdown(digest);
    crate::utils::write_owner_only_file_atomic(&digest_md_path, markdown.as_bytes())
        .map_err(|e| format!("write {}: {e}", digest_md_path.display()))?;
    Ok(json!({
        "digest_dir": dir,
        "digest_json_path": digest_json_path,
        "digest_md_path": digest_md_path,
    }))
}

pub(super) fn fetch_gh_pr_comments(
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

pub(super) async fn handle_gh_pr_comments(
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

pub(super) async fn handle_gh_pr_review_digest(
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
