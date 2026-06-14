use crate::gh_safe_merge::{
    evaluate_merge_gate_with_policy, ChecksState, GhClient, GhError, MergeDecision,
    MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable, PrLifecycleState,
    PrState, ReviewDecision,
};
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::{
    GhIssueCreateParams, GhIssueListParams, GhIssueReadParams, GhPrCommentsParams, GhPrListParams,
    GhPrReadParams, GhRepoViewParams, TachiGhParams,
};
use crate::vault_ops::read_unlocked_vault_secret;
use crate::verify_ops::evaluate_verification_gate;
use crate::MemoryServer;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const GH_AGENT_ID: &str = "tachi_gh_ops";
const MAX_GH_OUTPUT_CHARS: usize = 50_000;
const DEFAULT_REVIEW_AUTHOR_FILTER: &str = "gemini";
type GhPrCommentsBundle = (Vec<Value>, Vec<Value>, Vec<Value>);
const GH_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "SSH_AUTH_SOCK",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
];

/// Validate that a repo string contains exactly one "/"
fn validate_repo(repo: &str) -> Result<(), String> {
    let count = repo.matches('/').count();
    if count != 1 {
        return Err(format!(
            "Invalid repo format '{}'. Expected 'owner/repo' with exactly one '/'.",
            repo
        ));
    }
    let parts: Vec<&str> = repo.splitn(2, '/').collect();
    if parts[0].is_empty() || parts[1].is_empty() {
        return Err(format!(
            "Invalid repo format '{}'. Both owner and repo name must be non-empty.",
            repo
        ));
    }
    Ok(())
}

/// Resolve absolute path of `gh` binary. Returns error if not found.
fn resolve_gh_path() -> Result<String, String> {
    let output = Command::new("which")
        .arg("gh")
        .output()
        .map_err(|e| format!("Failed to locate `gh` CLI: {e}"))?;
    if !output.status.success() {
        return Err("GitHub CLI (`gh`) not found. Install it: https://cli.github.com".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err("`gh` CLI path resolved to empty string".into());
    }
    Ok(path)
}

/// Strip sensitive tokens from output text
fn sanitize_output(text: &str, token: &str) -> String {
    let mut sanitized = text.to_string();
    if !token.is_empty() {
        sanitized = sanitized.replace(token, "[REDACTED]");
    }
    // Also strip common auth header patterns
    let patterns = [
        "x-github-token",
        "Bearer gho_",
        "Bearer ghp_",
        "Bearer github_pat_",
    ];
    for pat in patterns {
        if let Some(pos) = sanitized.to_lowercase().find(&pat.to_lowercase()) {
            // Redact from the pattern to end of line or next whitespace
            if let Some(end) = sanitized[pos..].find(|c: char| c == '\n' || c == '\r') {
                sanitized.replace_range(pos..pos + end, "[REDACTED]");
            }
        }
    }
    sanitized
}

fn vault_secret_unavailable(err: &str) -> bool {
    err.starts_with("Secret not found: ")
        || err.starts_with("Vault is locked")
        || err.starts_with("Vault auto-locked")
        || err.starts_with("Vault not initialized")
}

fn env_gh_token() -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn resolve_gh_token(server: &MemoryServer) -> Result<Option<String>, String> {
    match read_unlocked_vault_secret(server, "GH_TOKEN", Some(GH_AGENT_ID), false) {
        Ok(token) => Ok(Some(token)),
        Err(err) if vault_secret_unavailable(&err) => Ok(env_gh_token()),
        Err(err) => Err(err),
    }
}

fn preserve_gh_env(cmd: &mut Command) {
    for var in GH_ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }
    if std::env::var_os("GITHUB_TOKEN").is_some() && std::env::var_os("GH_TOKEN").is_none() {
        if let Ok(val) = std::env::var("GITHUB_TOKEN") {
            cmd.env("GITHUB_TOKEN", val);
        }
    }
}

/// Build a sanitized Command for `gh` with env_clear + vault token injection
fn build_gh_command(server: &MemoryServer) -> Result<(Command, String), String> {
    let gh_path = resolve_gh_path()?;
    let token = resolve_gh_token(server)?;

    let mut cmd = Command::new(&gh_path);
    cmd.env_clear();

    preserve_gh_env(&mut cmd);
    if let Some(token) = token.as_deref() {
        cmd.env("GH_TOKEN", token);
    }
    cmd.env("GH_PROMPT_DISABLED", "1");
    cmd.env("NO_COLOR", "1");

    Ok((cmd, token.unwrap_or_default()))
}

/// Execute a gh command and return sanitized output, truncated to MAX_GH_OUTPUT_CHARS
fn run_gh(mut cmd: Command, token: &str) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute `gh`: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let sanitized_stdout = sanitize_output(&stdout, token);
    let sanitized_stderr = sanitize_output(&stderr, token);

    if !output.status.success() {
        return Err(format!(
            "gh failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            sanitized_stderr.chars().take(1000).collect::<String>()
        ));
    }

    // Truncate large output
    let result = if sanitized_stdout.len() > MAX_GH_OUTPUT_CHARS {
        let truncated: String = sanitized_stdout.chars().take(MAX_GH_OUTPUT_CHARS).collect();
        format!(
            "{}\n\n[truncated: {} total chars]",
            truncated,
            sanitized_stdout.len()
        )
    } else {
        sanitized_stdout
    };

    Ok(result)
}

fn run_gh_json(mut cmd: Command, token: &str) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute `gh`: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let sanitized_stdout = sanitize_output(&stdout, token);
    let sanitized_stderr = sanitize_output(&stderr, token);

    if !output.status.success() {
        return Err(format!(
            "gh failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            sanitized_stderr.chars().take(1000).collect::<String>()
        ));
    }

    Ok(sanitized_stdout)
}

// ─── Handlers ────────────────────────────────────────────────────────────────

pub(crate) async fn handle_gh_issue_read(
    server: &MemoryServer,
    params: GhIssueReadParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "view", &params.issue_number.to_string()])
        .args(["--repo", &params.repo])
        .args([
            "--json",
            "number,title,state,body,author,labels,assignees,createdAt,updatedAt,comments",
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

pub(crate) async fn handle_gh_issue_list(
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

pub(crate) async fn handle_gh_issue_create(
    server: &MemoryServer,
    params: GhIssueCreateParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "create"])
        .args(["--repo", &params.repo])
        .args(["--title", &params.title]);

    if let Some(ref body) = params.body {
        cmd.args(["--body", body]);
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

pub(crate) async fn handle_gh_pr_read(
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

fn run_gh_api_paginated(server: &MemoryServer, endpoint: &str) -> Result<Value, String> {
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

fn flatten_paginated_array(value: Value, label: &str) -> Result<Vec<Value>, String> {
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

fn normalize_review_entry(entry: Value) -> Value {
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

fn normalize_inline_comment_entry(entry: Value) -> Value {
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

fn comment_entry_time(entry: &Value) -> Option<&str> {
    entry
        .get("created_at")
        .and_then(Value::as_str)
        .or_else(|| entry.get("submitted_at").and_then(Value::as_str))
}

fn merge_pr_comment_entries(
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

fn review_digest_root() -> PathBuf {
    if let Ok(root) = std::env::var("TACHI_REVIEW_ROOT") {
        return PathBuf::from(root);
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.join(".tachi").join("reviews");
    }
    std::env::temp_dir().join("tachi").join("reviews")
}

fn safe_path_segment(raw: &str) -> String {
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

fn repo_review_segment(repo: &str) -> String {
    repo.split('/')
        .map(safe_path_segment)
        .collect::<Vec<_>>()
        .join("__")
}

fn comment_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn first_meaningful_line(body: &str) -> String {
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

fn lower_contains_any(lower_haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| lower_haystack.contains(needle))
}

fn classify_review_comment(body: &str, path: Option<&str>) -> &'static str {
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

fn author_matches_filter(comment: &Value, lower_filter: &str) -> bool {
    if lower_filter.is_empty() {
        return true;
    }
    comment
        .get("author")
        .and_then(Value::as_str)
        .map(|author| author.to_ascii_lowercase().contains(lower_filter))
        .unwrap_or(false)
}

fn infer_future_rule(category: &str, path: Option<&str>, body: &str) -> String {
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

fn review_project_base_path(layer: &str, repo: &str) -> String {
    let repo = repo.trim_matches('/');
    format!("/{layer}/projects/{repo}")
}

fn review_route_for_item(
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

fn review_routing_plan(items: &[Value]) -> Value {
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

fn build_pr_review_digest(
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

fn render_pr_review_digest_markdown(digest: &Value) -> String {
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

fn write_pr_review_digest_artifacts(digest: &Value) -> Result<Value, String> {
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

fn fetch_gh_pr_comments(
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

pub(crate) async fn handle_gh_pr_comments(
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

pub(crate) async fn handle_gh_pr_review_digest(
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

pub(crate) async fn handle_gh_pr_list(
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

pub(crate) async fn handle_gh_repo_view(
    server: &MemoryServer,
    params: GhRepoViewParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["repo", "view", &params.repo]).args([
        "--json",
        "name,owner,description,url,defaultBranchRef,stargazerCount,forkCount,isPrivate,languages,createdAt,updatedAt",
    ]);

    let output = run_gh(cmd, &token)?;
    serde_json::to_string(&json!({
        "tool": "tachi_gh_repo_view",
        "repo": params.repo,
        "result": serde_json::from_str::<serde_json::Value>(&output).unwrap_or(json!(output)),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_tachi_gh(
    server: &MemoryServer,
    params: TachiGhParams,
) -> Result<String, String> {
    match params.action.as_str() {
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
            "Unknown action '{}'. Expected: repo_view, issue_list, issue_read, issue_create, pr_list, pr_read, pr_comments, pr_review_digest, safe_merge",
            other
        )),
    }
}

// ─── safe_merge: CliGhClient + orchestrator ─────────────────────────────────

fn parse_merge_strategy(raw: Option<&str>) -> Result<MergeStrategy, String> {
    match raw.unwrap_or("squash").to_ascii_lowercase().as_str() {
        "squash" => Ok(MergeStrategy::Squash),
        "merge" => Ok(MergeStrategy::Merge),
        "rebase" => Ok(MergeStrategy::Rebase),
        other => Err(format!(
            "invalid merge_strategy '{}' (allowed: squash, merge, rebase)",
            other
        )),
    }
}

fn effective_safe_merge_dry_run(confirm: bool, requested_dry_run: Option<bool>) -> bool {
    !confirm || requested_dry_run.unwrap_or(false)
}

fn verification_satisfies_head_consistency(gate: Option<&Value>, policy: MergeGatePolicy) -> bool {
    policy.require_head_consistency
        && gate.and_then(|v| v.get("overall")).and_then(Value::as_str) == Some("passed")
}

fn apply_verification_gate_to_decision(
    decision: MergeDecision,
    gate: Option<&Value>,
) -> MergeDecision {
    let Some(gate) = gate else {
        return decision;
    };
    let reasons: Vec<String> = gate
        .get("reasons")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !reasons.is_empty() {
        return match decision {
            MergeDecision::Blocked {
                reasons: mut existing,
            } => {
                existing.extend(reasons);
                existing.sort();
                existing.dedup();
                MergeDecision::Blocked { reasons: existing }
            }
            _ => MergeDecision::Blocked { reasons },
        };
    }

    let waiting_on: Vec<String> = gate
        .get("waiting_on")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if waiting_on.is_empty() {
        return decision;
    }
    match decision {
        MergeDecision::Ready => MergeDecision::Pending { waiting_on },
        MergeDecision::Pending {
            waiting_on: mut existing,
        } => {
            existing.extend(waiting_on);
            existing.sort();
            existing.dedup();
            MergeDecision::Pending {
                waiting_on: existing,
            }
        }
        blocked => blocked,
    }
}

fn parse_merge_gate_policy(raw: Option<&str>) -> Result<MergeGatePolicy, String> {
    let mode = match raw
        .unwrap_or("standard")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "standard" => MergeGatePolicyMode::Standard,
        "permissive" => MergeGatePolicyMode::Permissive,
        "strict" => MergeGatePolicyMode::Strict,
        other => {
            return Err(format!(
                "invalid merge_policy '{}' (allowed: permissive, standard, strict)",
                other
            ))
        }
    };
    Ok(MergeGatePolicy::from_mode(mode))
}

fn merge_strategy_flag(s: MergeStrategy) -> &'static str {
    match s {
        MergeStrategy::Squash => "--squash",
        MergeStrategy::Merge => "--merge",
        MergeStrategy::Rebase => "--rebase",
    }
}

/// Map a `gh` CLI failure string into a typed `GhError`. The input is already
/// sanitized by `run_gh`. We classify by substring so callers can distinguish
/// "PR doesn't exist" (NotFound, terminal) from "API rate limit" (transient).
fn classify_gh_error(raw: &str) -> GhError {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("could not resolve") || lower.contains("not found") || lower.contains("404") {
        GhError::NotFound(raw.to_string())
    } else if lower.contains("rate limit") || lower.contains("403") && lower.contains("rate") {
        GhError::RateLimited(raw.to_string())
    } else {
        GhError::Sanitized(raw.to_string())
    }
}

fn is_no_checks_reported(raw: &str) -> bool {
    raw.to_ascii_lowercase().contains("no checks reported")
}

/// Production `GhClient` that wraps the sanitized `gh` subprocess pipeline
/// already used by the rest of `tachi_gh`. Holds a borrowed `&MemoryServer`
/// so the vault token is read fresh per call.
pub(crate) struct CliGhClient<'a> {
    pub(crate) server: &'a MemoryServer,
}

impl<'a> CliGhClient<'a> {
    fn build(&self) -> Result<(Command, String), GhError> {
        build_gh_command(self.server).map_err(|e| GhError::Sanitized(e))
    }
}

#[async_trait]
impl<'a> GhClient for CliGhClient<'a> {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args([
                "--json",
                "number,state,mergeable,reviewDecision,isDraft,headRefOid,closingIssuesReferences",
            ]);
        let raw = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| GhError::Sanitized(format!("pr_view parse: {e}")))?;
        let checks = self.checks_list(repo, number).await?;
        Ok(parse_pr_view_json(&v, checks)
            .map_err(|e| GhError::Sanitized(format!("pr_view shape: {e}")))?)
    }

    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["pr", "merge", &number.to_string()])
            .args(["--repo", repo])
            .arg(merge_strategy_flag(strategy));
        if !expected_head_sha.trim().is_empty() {
            cmd.args(["--match-head-commit", expected_head_sha]);
        }
        let _out = run_gh(cmd, &token).map_err(|e| classify_gh_error(&e))?;
        // gh pr merge prints a status line, not JSON. Re-fetch the merged SHA.
        let (mut cmd2, token2) = self.build()?;
        cmd2.args(["pr", "view", &number.to_string()])
            .args(["--repo", repo])
            .args(["--json", "mergeCommit"]);
        let sha_raw = run_gh(cmd2, &token2).map_err(|e| classify_gh_error(&e))?;
        let merge_sha = serde_json::from_str::<serde_json::Value>(&sha_raw)
            .ok()
            .and_then(|v| {
                v.get("mergeCommit")
                    .and_then(|mc| mc.get("oid"))
                    .and_then(|o| o.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        Ok(MergeResult {
            pr_number: number,
            merge_sha,
            strategy,
        })
    }

    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<crate::gh_safe_merge::IssueState, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        let (mut cmd, token) = self.build()?;
        cmd.args(["issue", "create"])
            .args(["--repo", repo])
            .args(["--title", title]);
        if let Some(b) = body {
            cmd.args(["--body", b]);
        }
        for l in labels {
            cmd.args(["--label", l]);
        }
        let url = run_gh(cmd, &token)
            .map_err(|e| classify_gh_error(&e))?
            .trim()
            .to_string();
        // `gh issue create` prints the issue URL; derive the number from the trailing path segment.
        let number = url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                GhError::Sanitized(format!(
                    "gh issue create returned an unparseable issue URL: {url}"
                ))
            })?;
        Ok(crate::gh_safe_merge::IssueState {
            number,
            title: title.to_string(),
            state: "OPEN".to_string(),
            url,
        })
    }

    async fn checks_list(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<crate::gh_safe_merge::CheckRun>, GhError> {
        validate_repo(repo).map_err(GhError::Sanitized)?;
        // `gh pr checks` may exit non-zero when checks have failed; we still
        // want to parse the JSON. Run it directly and tolerate non-zero exit
        // when stdout looks like a JSON array.
        let (mut raw_cmd, token) = self.build()?;
        raw_cmd
            .args(["pr", "checks", &pr_number.to_string()])
            .args(["--repo", repo])
            .arg("--json")
            .arg("name,state,bucket");
        let output = raw_cmd
            .output()
            .map_err(|e| GhError::Sanitized(format!("gh exec: {e}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let sanitized = sanitize_output(&stdout, &token);
        let sanitized_stderr = sanitize_output(&stderr, &token);
        let trimmed = sanitized.trim();
        if trimmed.is_empty() || trimmed == "null" {
            if output.status.success() {
                return Ok(Vec::new());
            }
            if is_no_checks_reported(&sanitized_stderr) {
                return Ok(Vec::new());
            }
            return Err(classify_gh_error(&format!(
                "gh pr checks failed: {}",
                sanitized_stderr.trim()
            )));
        }
        if !trimmed.starts_with('[') {
            return Err(classify_gh_error(&format!(
                "gh pr checks returned non-json output: {} {}",
                trimmed,
                sanitized_stderr.trim()
            )));
        }
        let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed)
            .map_err(|e| GhError::Sanitized(format!("checks_list parse: {e}")))?;
        Ok(arr
            .into_iter()
            .map(|v| {
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                // `gh pr checks --json` exposes `bucket` ∈ pass/fail/pending/skipping/cancel
                // and `state` for the raw check status. We map bucket → conclusion
                // and synthesize a `completed`/`in_progress` status.
                let bucket = v
                    .get("bucket")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string();
                let (status, conclusion) = match bucket.as_str() {
                    "pass" => ("completed".to_string(), Some("success".to_string())),
                    "fail" => ("completed".to_string(), Some("failure".to_string())),
                    "cancel" => ("completed".to_string(), Some("cancelled".to_string())),
                    "skipping" => ("completed".to_string(), Some("skipped".to_string())),
                    "pending" | "" => ("in_progress".to_string(), None),
                    _ => ("completed".to_string(), Some(bucket.clone())),
                };
                crate::gh_safe_merge::CheckRun {
                    name,
                    conclusion,
                    status,
                }
            })
            .collect())
    }
}

/// Parse the `gh pr view --json number,state,mergeable,reviewDecision,isDraft,headRefOid,closingIssuesReferences`
/// payload into a `PrState`. Pulled out as a free function so unit tests can
/// exercise the JSON shape without spawning `gh`.
fn parse_pr_view_json(
    v: &serde_json::Value,
    checks: Vec<crate::gh_safe_merge::CheckRun>,
) -> Result<PrState, String> {
    let number = v
        .get("number")
        .and_then(|n| n.as_u64())
        .ok_or("pr_view: missing number")?;
    let state_raw = v
        .get("state")
        .and_then(|s| s.as_str())
        .ok_or("pr_view: missing state")?;
    let state = match state_raw {
        "OPEN" => PrLifecycleState::Open,
        "CLOSED" => PrLifecycleState::Closed,
        "MERGED" => PrLifecycleState::Merged,
        other => return Err(format!("pr_view: unknown state '{}'", other)),
    };
    let mergeable = match v.get("mergeable").and_then(|m| m.as_str()).unwrap_or("") {
        "MERGEABLE" => Mergeable::Mergeable,
        "CONFLICTING" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    let review_decision = v
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .and_then(|s| match s {
            "APPROVED" => Some(ReviewDecision::Approved),
            "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
            "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
            "" => None,
            _ => Some(ReviewDecision::ReviewRequired),
        });
    let is_draft = v.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
    let head_sha = v
        .get("headRefOid")
        .and_then(|h| h.as_str())
        .unwrap_or_default()
        .to_string();
    let linked_issue_refs = v
        .get("closingIssuesReferences")
        .and_then(|refs| refs.as_array())
        .map(|refs| {
            refs.iter()
                .filter_map(|issue| {
                    if let Some(url) = issue.get("url").and_then(|u| u.as_str()) {
                        Some(url.to_string())
                    } else {
                        issue
                            .get("number")
                            .and_then(|n| n.as_u64())
                            .map(|n| format!("#{n}"))
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(PrState {
        number,
        state,
        mergeable,
        review_decision,
        checks: ChecksState::aggregate(&checks),
        is_draft,
        head_sha,
        linked_issue_refs,
        head_consistent: None,
    })
}

/// Orchestrate one `tachi_gh safe_merge` invocation:
/// 1. Pull PR + checks via the `GhClient`.
/// 2. Run the pure `evaluate_merge_gate`.
/// 3. If `Ready` and `!dry_run`, call `pr_merge` (squash by default).
/// 4. When `flow_id` is present, persist `merge_state` + reasons into
///    `status.json::github` and append the matching event to `events.jsonl`.
/// 5. Return a JSON envelope the agent can render directly.
pub(crate) async fn handle_github_safe_merge<C: GhClient + ?Sized>(
    client: &C,
    repo: &str,
    pr_number: u64,
    strategy: MergeStrategy,
    dry_run: bool,
    flow_id: Option<&str>,
    policy: MergeGatePolicy,
) -> Result<String, String> {
    let flow_run_dir = match flow_id {
        Some(fid) => Some(run_dir_for_flow_id(fid)?),
        None => None,
    };
    let mut pr = client
        .pr_view(repo, pr_number)
        .await
        .map_err(|e| format!("pr_view failed: {e}"))?;
    let mut verification_gate = match evaluate_verification_gate(flow_id, &pr.head_sha) {
        Ok(gate) => gate,
        Err(err) if flow_id.is_some() => Some(json!({
            "flow_id": flow_id,
            "overall": "failed",
            "required_total": 0,
            "current_head_sha": pr.head_sha,
            "passed": [],
            "failed": [],
            "pending": [],
            "stale": [],
            "waiting_on": [],
            "reasons": ["verification:invalid"],
            "error": err,
        })),
        Err(err) => return Err(err),
    };
    if verification_gate.is_none()
        && flow_id.is_some()
        && !matches!(policy.mode, MergeGatePolicyMode::Permissive)
    {
        verification_gate = Some(json!({
            "flow_id": flow_id,
            "overall": "pending",
            "required_total": 0,
            "current_head_sha": pr.head_sha,
            "passed": [],
            "failed": [],
            "pending": [],
            "stale": [],
            "waiting_on": ["verification:missing"],
            "reasons": [],
        }));
    }
    if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
        pr.head_consistent = Some(true);
    }
    let mut decision = evaluate_merge_gate_with_policy(&pr, policy);
    decision = apply_verification_gate_to_decision(decision, verification_gate.as_ref());
    let has_linked_issue = !pr.linked_issue_refs.is_empty();
    if policy.require_linked_issue_or_flow && flow_id.is_none() && !has_linked_issue {
        decision = match decision {
            MergeDecision::Ready => MergeDecision::Pending {
                waiting_on: vec!["flow_or_issue:missing".to_string()],
            },
            MergeDecision::Pending { mut waiting_on } => {
                waiting_on.push("flow_or_issue:missing".to_string());
                MergeDecision::Pending { waiting_on }
            }
            blocked => blocked,
        };
    }
    let merge_state = decision.merge_state_label();
    let will_merge = matches!(decision, MergeDecision::Ready) && !dry_run;
    let requested_mode = if dry_run {
        "preview"
    } else {
        "merge_requested"
    };

    let (event_kind, event_payload, merged_sha) = match &decision {
        MergeDecision::Ready => {
            if dry_run {
                (
                    "github_review_gate_passed",
                    json!({
                        "repo": repo,
                        "pr_number": pr_number,
                        "head_sha": pr.head_sha,
                        "requested_mode": requested_mode,
                        "merge_attempted": false,
                        "merge_executed": false,
                        "dry_run": true,
                    }),
                    None,
                )
            } else {
                let merge_res = client
                    .pr_merge(repo, pr_number, strategy, &pr.head_sha)
                    .await
                    .map_err(|e| format!("pr_merge failed: {e}"))?;
                (
                    "github_pr_merged",
                    json!({
                        "repo": repo,
                        "pr_number": pr_number,
                        "head_sha": pr.head_sha,
                        "merge_sha": merge_res.merge_sha,
                        "strategy": format!("{:?}", merge_res.strategy).to_lowercase(),
                        "requested_mode": requested_mode,
                        "merge_attempted": true,
                        "merge_executed": true,
                        "dry_run": false,
                    }),
                    Some(merge_res.merge_sha),
                )
            }
        }
        MergeDecision::Blocked { reasons } => (
            "github_merge_blocked",
            json!({
                "repo": repo,
                "pr_number": pr_number,
                "head_sha": pr.head_sha,
                "requested_mode": requested_mode,
                "merge_attempted": false,
                "merge_executed": false,
                "reasons": reasons,
            }),
            None,
        ),
        MergeDecision::Pending { waiting_on } => (
            "github_checks_polled",
            json!({
                "repo": repo,
                "pr_number": pr_number,
                "head_sha": pr.head_sha,
                "requested_mode": requested_mode,
                "merge_attempted": false,
                "merge_executed": false,
                "waiting_on": waiting_on,
            }),
            None,
        ),
    };

    // Effective merge_state: if we actually merged, surface "merged" rather
    // than "ready" so downstream consumers don't need to re-check.
    let effective_state = if merged_sha.is_some() {
        "merged"
    } else {
        merge_state
    };

    let status_patch = json!({
        "repo": repo,
        "pr_number": pr_number,
        "merge_state": effective_state,
        "head_sha": pr.head_sha,
        "policy": policy.mode.as_str(),
        "dry_run": dry_run,
        "will_merge": will_merge,
        "requested_mode": requested_mode,
        "merge_attempted": merged_sha.is_some(),
        "merge_executed": merged_sha.is_some(),
        "head_consistency": {
            "head_sha": pr.head_sha,
            "checks_head_sha": null,
            "review_decision_head_sha": null,
            "head_consistent": verification_satisfies_head_consistency(verification_gate.as_ref(), policy),
            "state": if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
                "verified_by_tachi_verification"
            } else {
                "unknown"
            },
            "requirement": policy.require_head_consistency,
            "source": if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
                "verification_ledger"
            } else {
                "single_pr_snapshot"
            },
            "note": if verification_satisfies_head_consistency(verification_gate.as_ref(), policy) {
                "required verification ledger passed for the same PR head SHA; match-head-commit still pins the final merge command"
            } else {
                "gh_pr_checks does not expose independent head SHA data; match-head-commit still pins the final merge command"
            },
        },
        "checks": {
            "state": match pr.checks {
                ChecksState::None => "none",
                ChecksState::Pending => "pending",
                ChecksState::Skipped => "skipped",
                ChecksState::Success => "success",
                ChecksState::Failure => "failure",
            },
            "required": policy.require_checks,
            "allow_missing": policy.allow_missing_checks,
            "source": "gh_pr_checks",
            "head_consistent": false,
            "head_consistency_state": "unknown",
        },
        "review": {
            "state": match pr.review_decision {
                Some(ReviewDecision::Approved) => "approved",
                Some(ReviewDecision::ChangesRequested) => "changes_requested",
                Some(ReviewDecision::ReviewRequired) => "review_required",
                None if policy.require_review_approval => "unknown",
                None => "not_required",
            },
            "required": policy.require_review_approval,
            "allow_missing_decision": policy.allow_missing_review_decision,
        },
        "flow": {
            "flow_id": flow_id,
            "linked_issue_refs": pr.linked_issue_refs,
            "has_linked_issue": has_linked_issue,
            "required": policy.require_linked_issue_or_flow,
        },
        "verification": verification_gate,
    });

    let mut persisted = false;
    if let (Some(fid), Some(run_dir)) = (flow_id, flow_run_dir.as_ref()) {
        std::fs::create_dir_all(run_dir).map_err(|e| format!("create run dir: {e}"))?;
        merge_github_status(run_dir, status_patch.clone())?;
        append_github_event(run_dir, fid, event_kind, event_payload.clone())?;
        persisted = true;
    }

    serde_json::to_string(&json!({
        "tool": "tachi_gh_safe_merge",
        "repo": repo,
        "pr_number": pr_number,
        "decision": decision,
        "merge_state": effective_state,
        "mode": policy.mode.as_str(),
        "merged_sha": merged_sha,
        "dry_run": dry_run,
        "will_merge": will_merge,
        "requested_mode": requested_mode,
        "merge_attempted": merged_sha.is_some(),
        "merge_executed": merged_sha.is_some(),
        "flow_id": flow_id,
        "persisted": persisted,
        "status_patch": status_patch,
        "event": {
            "kind": event_kind,
            "payload": event_payload,
        },
    }))
    .map_err(|e| format!("serialize: {e}"))
}

#[cfg(test)]
mod safe_merge_tests {
    use super::*;
    use crate::gh_safe_merge::{CheckRun, MockGhClient};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn ready_pr() -> PrState {
        PrState {
            number: 42,
            state: PrLifecycleState::Open,
            mergeable: Mergeable::Mergeable,
            review_decision: Some(ReviewDecision::Approved),
            checks: ChecksState::Success,
            is_draft: false,
            head_sha: "deadbeef".to_string(),
            linked_issue_refs: Vec::new(),
            head_consistent: None,
        }
    }

    fn blocked_draft_pr() -> PrState {
        PrState {
            is_draft: true,
            ..ready_pr()
        }
    }

    fn pending_pr() -> PrState {
        PrState {
            mergeable: Mergeable::Unknown,
            ..ready_pr()
        }
    }

    fn skipped_checks_pr() -> PrState {
        PrState {
            checks: ChecksState::Skipped,
            ..ready_pr()
        }
    }

    fn write_verification(root: &std::path::Path, flow: &str, status: &str, head_sha: &str) {
        let run_dir = root.join(flow);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("verification.json"),
            serde_json::to_string_pretty(&json!({
                "flow_id": flow,
                "head_sha": head_sha,
                "overall": status,
                "updated_at": "2026-06-08T00:00:00Z",
                "items": [
                    {
                        "id": "gitleaks",
                        "kind": "gitleaks",
                        "status": status,
                        "head_sha": head_sha,
                        "required": true,
                        "summary": "verification fixture"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pr_comments_merge_preserves_chronological_order() {
        let reviews = vec![json!({
            "kind": "review",
            "id": 2,
            "created_at": "2026-06-06T10:10:00Z",
            "body": "summary",
        })];
        let inline_comments = vec![json!({
            "kind": "inline_comment",
            "id": 1,
            "created_at": "2026-06-06T10:05:00Z",
            "body": "line comment",
        })];

        let merged = merge_pr_comment_entries(reviews, inline_comments);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["kind"], "inline_comment");
        assert_eq!(merged[1]["kind"], "review");
    }

    #[test]
    fn pr_comments_flatten_paginated_arrays() {
        let pages = json!([
            [{"id": 1}],
            [{"id": 2}, {"id": 3}]
        ]);

        let flattened = flatten_paginated_array(pages, "comments").expect("flat");

        assert_eq!(flattened.len(), 3);
        assert_eq!(flattened[0]["id"], 1);
        assert_eq!(flattened[2]["id"], 3);
    }

    #[test]
    fn pr_review_digest_filters_gemini_and_builds_candidates() {
        let comments = vec![
            json!({
                "kind": "inline_comment",
                "id": 11,
                "author": "gemini-code-assist",
                "path": "crates/memory-server/src/gh_ops.rs",
                "line": 42,
                "body": "![medium](https://www.gstatic.com/codereviewagent/medium-priority.svg)\nPlease add coverage for paginated comments.",
                "created_at": "2026-06-06T10:05:00Z",
                "url": "https://example.test/comment/11",
            }),
            json!({
                "kind": "inline_comment",
                "id": 12,
                "author": "human-reviewer",
                "path": "README.md",
                "line": 1,
                "body": "Looks good.",
            }),
        ];

        let digest = build_pr_review_digest("o/r", 202, "gemini", &comments);

        assert_eq!(digest["comment_count"], 1);
        assert_eq!(digest["counts"]["tests"], 1);
        assert_eq!(digest["items"][0]["verdict"], "needs_leader_verdict");
        assert_eq!(
            digest["items"][0]["summary"],
            "Please add coverage for paginated comments."
        );
        assert_eq!(digest["memory_candidates"].as_array().unwrap().len(), 1);
        assert_eq!(digest["handbook_candidates"].as_array().unwrap().len(), 1);
        assert!(digest["handbook_candidates"][0]["rule"]
            .as_str()
            .unwrap()
            .contains("regression coverage"));
        let destinations = digest["routing_plan"]["items"][0]["destinations"]
            .as_array()
            .unwrap();
        for expected in [
            "pr_comment",
            "github_issue",
            "feedback_rule",
            "guide",
            "project_wiki",
            "eval",
        ] {
            assert!(
                destinations.iter().any(|value| value == expected),
                "missing {expected} in {destinations:#?}"
            );
        }
        assert_eq!(
            digest["items"][0]["routing"]["primary_destination"],
            json!("github_issue")
        );
        assert_eq!(
            digest["items"][0]["routing"]["promotion_requires"],
            json!("leader_verdict")
        );
        assert_eq!(
            digest["routing_plan"]["status"],
            json!("needs_leader_verdict")
        );
        assert_eq!(
            digest["routing_plan"]["destination_counts"]["feedback_rule"],
            1
        );
    }

    #[test]
    fn pr_review_digest_keeps_style_out_of_handbook_candidates() {
        let comments = vec![json!({
            "kind": "inline_comment",
            "id": 21,
            "author": "gemini-code-assist",
            "path": "src/lib.rs",
            "line": 7,
            "body": "Nit: this naming is a little unclear.",
        })];

        let digest = build_pr_review_digest("o/r", 7, "gemini", &comments);

        assert_eq!(digest["counts"]["style"], 1);
        assert_eq!(digest["memory_candidates"].as_array().unwrap().len(), 1);
        assert_eq!(digest["handbook_candidates"].as_array().unwrap().len(), 0);
        assert_eq!(
            digest["items"][0]["routing"]["primary_destination"],
            json!("pr_comment")
        );
        let destinations = digest["routing_plan"]["items"][0]["destinations"]
            .as_array()
            .unwrap();
        assert!(destinations.iter().any(|value| value == "pr_comment"));
        assert!(destinations.iter().any(|value| value == "eval"));
        assert!(!destinations.iter().any(|value| value == "feedback_rule"));
        assert!(digest["routing_plan"]["destination_counts"]
            .get("feedback_rule")
            .is_none());
    }

    #[test]
    fn pr_review_digest_artifacts_write_json_and_markdown() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_REVIEW_ROOT");
        std::env::set_var("TACHI_REVIEW_ROOT", tmp.path());
        let comments = vec![json!({
            "kind": "inline_comment",
            "id": 31,
            "author": "gemini-code-assist",
            "path": "src/lib.rs",
            "line": 9,
            "body": "Incorrect state handling can cause a regression.",
        })];
        let digest = build_pr_review_digest("owner/repo", 31, "gemini", &comments);

        let artifacts = write_pr_review_digest_artifacts(&digest).unwrap();

        let md_path = PathBuf::from(artifacts["digest_md_path"].as_str().unwrap());
        let json_path = PathBuf::from(artifacts["digest_json_path"].as_str().unwrap());
        assert!(md_path.exists());
        assert!(json_path.exists());
        let markdown = std::fs::read_to_string(md_path).unwrap();
        assert!(markdown.contains("Triage Contract"));
        assert!(markdown.contains("Review Output Routing"));
        assert!(markdown.contains("Primary route:"));
        assert!(markdown.contains("needs_leader_verdict"));
        let leftovers: Vec<_> = std::fs::read_dir(json_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                name.starts_with("digest.json.tmp.") || name.starts_with("digest.md.tmp.")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "digest artifact writes should not leave temp files: {leftovers:?}"
        );
        if let Some(v) = original {
            std::env::set_var("TACHI_REVIEW_ROOT", v);
        } else {
            std::env::remove_var("TACHI_REVIEW_ROOT");
        }
    }

    #[tokio::test]
    async fn safe_merge_dry_run_ready_does_not_call_pr_merge() {
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "ready");
        assert_eq!(v["mode"], "standard");
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["will_merge"], false);
        assert_eq!(v["requested_mode"], "preview");
        assert_eq!(v["merge_attempted"], false);
        assert_eq!(v["merge_executed"], false);
        assert!(v["merged_sha"].is_null());
        assert_eq!(v["status_patch"]["checks"]["required"], true);
        assert_eq!(v["status_patch"]["review"]["required"], true);
        assert_eq!(
            v["status_patch"]["head_consistency"]["head_sha"],
            "deadbeef"
        );
        assert_eq!(v["status_patch"]["head_consistency"]["state"], "unknown");
        assert_eq!(
            v["status_patch"]["head_consistency"]["head_consistent"],
            false
        );
        assert_eq!(
            v["status_patch"]["head_consistency"]["source"],
            "single_pr_snapshot"
        );
        assert_eq!(v["event"]["payload"]["requested_mode"], "preview");
        assert_eq!(v["event"]["kind"], "github_review_gate_passed");
        assert!(client.merge_calls().is_empty());
    }

    #[tokio::test]
    async fn safe_merge_ready_executes_merge_when_not_dry_run() {
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "merged");
        assert_eq!(v["will_merge"], true);
        assert_eq!(v["requested_mode"], "merge_requested");
        assert_eq!(v["merge_attempted"], true);
        assert_eq!(v["merge_executed"], true);
        assert!(v["merged_sha"].is_string());
        assert_eq!(v["event"]["kind"], "github_pr_merged");
        assert_eq!(v["event"]["payload"]["requested_mode"], "merge_requested");
        assert_eq!(v["event"]["payload"]["merge_attempted"], true);
        assert_eq!(v["event"]["payload"]["merge_executed"], true);
        let calls = client.merge_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "o/r");
        assert_eq!(calls[0].1, 42);
        assert_eq!(calls[0].3, "deadbeef");
    }

    #[tokio::test]
    async fn safe_merge_reports_head_sha_mismatch_from_merge_client() {
        let client = MockGhClient::new().with_pr("o/r", ready_pr());
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["event"]["payload"]["head_sha"], "deadbeef");
        assert_eq!(client.merge_calls()[0].3, "deadbeef");
    }

    #[tokio::test]
    async fn safe_merge_blocked_does_not_call_pr_merge_even_when_not_dry_run() {
        let client = MockGhClient::new()
            .with_pr("o/r", blocked_draft_pr())
            .with_checks("o/r", 42, vec![]);
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "blocked");
        assert_eq!(v["event"]["kind"], "github_merge_blocked");
        let reasons = v["event"]["payload"]["reasons"].as_array().unwrap();
        assert!(reasons.iter().any(|r| r == "draft"));
        assert!(client.merge_calls().is_empty());
    }

    #[tokio::test]
    async fn safe_merge_pending_emits_checks_polled() {
        let client = MockGhClient::new()
            .with_pr("o/r", pending_pr())
            .with_checks("o/r", 42, vec![]);
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert_eq!(v["event"]["kind"], "github_checks_polled");
        assert!(client.merge_calls().is_empty());
    }

    #[tokio::test]
    async fn safe_merge_skipped_checks_waits_and_labels_check_state() {
        let client = MockGhClient::new()
            .with_pr("o/r", skipped_checks_pr())
            .with_checks(
                "o/r",
                42,
                vec![CheckRun {
                    name: "conditional-ci".to_string(),
                    status: "completed".to_string(),
                    conclusion: Some("skipped".to_string()),
                }],
            );
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert_eq!(v["status_patch"]["checks"]["state"], "skipped");
        assert!(v["decision"]["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "checks:skipped"));
        assert!(client.merge_calls().is_empty());
    }

    #[tokio::test]
    async fn safe_merge_strict_requires_flow_id_before_merge() {
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
        let mut policy = MergeGatePolicy::strict();
        policy.require_head_consistency = false;
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            policy,
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert_eq!(v["will_merge"], false);
        assert_eq!(v["merge_attempted"], false);
        assert!(v["decision"]["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "flow_or_issue:missing"));
        assert!(client.merge_calls().is_empty());
    }

    #[tokio::test]
    async fn safe_merge_strict_accepts_linked_issue_without_flow_id() {
        let mut pr = ready_pr();
        pr.linked_issue_refs = vec!["https://github.com/o/r/issues/99".to_string()];
        let client = MockGhClient::new()
            .with_pr("o/r", pr)
            .with_checks("o/r", 42, vec![]);
        let mut policy = MergeGatePolicy::strict();
        policy.require_head_consistency = false;
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            None,
            policy,
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "ready");
        assert_eq!(v["status_patch"]["flow"]["has_linked_issue"], true);
        assert_eq!(
            v["status_patch"]["flow"]["linked_issue_refs"][0],
            "https://github.com/o/r/issues/99"
        );
    }

    #[tokio::test]
    async fn safe_merge_head_consistency_required_blocks_merge() {
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
        let mut policy = MergeGatePolicy::standard();
        policy.require_head_consistency = true;
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            None,
            policy,
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert_eq!(v["will_merge"], false);
        assert_eq!(v["status_patch"]["head_consistency"]["state"], "unknown");
        assert_eq!(
            v["status_patch"]["head_consistency"]["head_consistent"],
            false
        );
        assert!(v["decision"]["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "head:consistency_unavailable"));
        assert!(client.merge_calls().is_empty());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_with_flow_id_missing_verification_waits() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);

        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            Some("flow_missing-verification"),
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert_eq!(v["will_merge"], false);
        assert!(v["decision"]["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:missing"));
        assert!(client.merge_calls().is_empty());
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_failed_verification_blocks_even_permissive() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());
        let flow = "flow_failed-verification";
        write_verification(tmp.path(), flow, "failed", "deadbeef");
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);

        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            Some(flow),
            MergeGatePolicy::permissive(),
        )
        .await
        .expect("ok");

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "blocked");
        assert!(v["decision"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:gitleaks:failed"));
        assert!(client.merge_calls().is_empty());
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_stale_verification_waits_on_head_mismatch() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());
        let flow = "flow_stale-verification";
        write_verification(tmp.path(), flow, "passed", "oldsha");
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);

        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            Some(flow),
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert!(v["decision"]["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:gitleaks:stale"));
        assert!(client.merge_calls().is_empty());
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_strict_uses_passed_verification_for_head_consistency() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());
        let flow = "flow_strict-verification";
        write_verification(tmp.path(), flow, "passed", "deadbeef");
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);

        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            Some(flow),
            MergeGatePolicy::strict(),
        )
        .await
        .expect("ok");

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "ready");
        assert_eq!(
            v["status_patch"]["head_consistency"]["state"],
            "verified_by_tachi_verification"
        );
        assert_eq!(
            v["status_patch"]["head_consistency"]["head_consistent"],
            true
        );
        assert!(client.merge_calls().is_empty());
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_strict_does_not_treat_not_required_verification_as_head_proof() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());
        let flow = "flow_strict-not-required";
        let run_dir = tmp.path().join(flow);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("verification.json"),
            serde_json::to_string_pretty(&json!({
                "flow_id": flow,
                "overall": "passed",
                "items": [
                    {"id":"optional-check","status":"passed","head_sha":"deadbeef","required":false}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);

        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            Some(flow),
            MergeGatePolicy::strict(),
        )
        .await
        .expect("ok");

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["merge_state"], "pending");
        assert_eq!(
            v["status_patch"]["head_consistency"]["head_consistent"],
            false
        );
        assert!(v["decision"]["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "head:consistency_unavailable"));
        assert!(client.merge_calls().is_empty());
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_persists_status_and_event_when_flow_id_supplied() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        // Force shell_runs_root() to the tempdir via env override.
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
        let flow = "flow_test-safe-merge";
        write_verification(tmp.path(), flow, "passed", "deadbeef");
        let out = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            Some(flow),
            MergeGatePolicy::standard(),
        )
        .await
        .expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["persisted"], true);
        assert_eq!(v["requested_mode"], "preview");
        let run_dir = tmp.path().join(flow);
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
                .unwrap();
        assert_eq!(status["github"]["merge_state"], "ready");
        assert_eq!(status["github"]["policy"], "standard");
        assert_eq!(status["github"]["will_merge"], false);
        assert_eq!(status["github"]["requested_mode"], "preview");
        assert_eq!(status["github"]["merge_attempted"], false);
        assert_eq!(status["github"]["merge_executed"], false);
        assert_eq!(
            status["github"]["head_consistency"]["source"],
            "single_pr_snapshot"
        );
        assert_eq!(status["github"]["pr_number"], 42);
        let events = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
        assert!(events.contains("\"github_review_gate_passed\""));
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn safe_merge_persists_pending_blocked_and_merged_flow_events() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());

        let pending_client = MockGhClient::new()
            .with_pr("o/r", pending_pr())
            .with_checks("o/r", 42, vec![]);
        handle_github_safe_merge(
            &pending_client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            Some("flow_pending-safe-merge"),
            MergeGatePolicy::standard(),
        )
        .await
        .expect("pending ok");
        let pending_dir = tmp.path().join("flow_pending-safe-merge");
        let pending_status: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(pending_dir.join("status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(pending_status["github"]["merge_state"], "pending");
        assert!(std::fs::read_to_string(pending_dir.join("events.jsonl"))
            .unwrap()
            .contains("\"github_checks_polled\""));

        let blocked_client = MockGhClient::new()
            .with_pr("o/r", blocked_draft_pr())
            .with_checks("o/r", 42, vec![]);
        handle_github_safe_merge(
            &blocked_client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            Some("flow_blocked-safe-merge"),
            MergeGatePolicy::standard(),
        )
        .await
        .expect("blocked ok");
        let blocked_dir = tmp.path().join("flow_blocked-safe-merge");
        let blocked_status: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(blocked_dir.join("status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(blocked_status["github"]["merge_state"], "blocked");
        assert_eq!(
            blocked_status["github"]["requested_mode"],
            "merge_requested"
        );
        assert_eq!(blocked_status["github"]["merge_attempted"], false);
        assert_eq!(blocked_status["github"]["merge_executed"], false);
        assert!(std::fs::read_to_string(blocked_dir.join("events.jsonl"))
            .unwrap()
            .contains("\"github_merge_blocked\""));
        assert!(blocked_client.merge_calls().is_empty());

        let merged_client =
            MockGhClient::new()
                .with_pr("o/r", ready_pr())
                .with_checks("o/r", 42, vec![]);
        write_verification(tmp.path(), "flow_merged-safe-merge", "passed", "deadbeef");
        handle_github_safe_merge(
            &merged_client,
            "o/r",
            42,
            MergeStrategy::Squash,
            false,
            Some("flow_merged-safe-merge"),
            MergeGatePolicy::standard(),
        )
        .await
        .expect("merged ok");
        let merged_dir = tmp.path().join("flow_merged-safe-merge");
        let merged_status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(merged_dir.join("status.json")).unwrap())
                .unwrap();
        assert_eq!(merged_status["github"]["merge_state"], "merged");
        assert_eq!(merged_status["github"]["will_merge"], true);
        assert_eq!(merged_status["github"]["requested_mode"], "merge_requested");
        assert_eq!(merged_status["github"]["merge_attempted"], true);
        assert_eq!(merged_status["github"]["merge_executed"], true);
        assert!(std::fs::read_to_string(merged_dir.join("events.jsonl"))
            .unwrap()
            .contains("\"github_pr_merged\""));
        assert_eq!(merged_client.merge_calls().len(), 1);

        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[tokio::test]
    async fn safe_merge_rejects_invalid_flow_id() {
        let client = MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
        let err = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            Some("../escape"),
            MergeGatePolicy::standard(),
        )
        .await
        .expect_err("invalid flow id should fail");
        assert!(err.contains("Invalid flow_id"));
    }

    #[tokio::test]
    async fn safe_merge_propagates_pr_view_not_found() {
        let client = MockGhClient::new(); // no PRs registered
        let err = handle_github_safe_merge(
            &client,
            "o/r",
            42,
            MergeStrategy::Squash,
            true,
            None,
            MergeGatePolicy::standard(),
        )
        .await
        .expect_err("should fail");
        assert!(err.contains("pr_view failed"));
        assert!(err.contains("not found") || err.contains("NotFound"));
    }

    #[test]
    fn parse_pr_view_json_happy_path() {
        let v = json!({
            "number": 7,
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "isDraft": false,
            "headRefOid": "abc123",
            "closingIssuesReferences": [
                {"number": 99, "url": "https://github.com/o/r/issues/99"}
            ],
        });
        let pr = parse_pr_view_json(&v, vec![]).unwrap();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.state, PrLifecycleState::Open);
        assert_eq!(pr.mergeable, Mergeable::Mergeable);
        assert_eq!(pr.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(pr.checks, ChecksState::None);
        assert!(!pr.is_draft);
        assert_eq!(pr.head_sha, "abc123");
        assert_eq!(
            pr.linked_issue_refs,
            vec!["https://github.com/o/r/issues/99".to_string()]
        );
    }

    #[test]
    fn parse_pr_view_json_aggregates_checks() {
        let v = json!({
            "number": 7,
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "reviewDecision": null,
            "isDraft": false,
            "headRefOid": "abc",
        });
        let runs = vec![
            CheckRun {
                name: "ci".into(),
                conclusion: Some("success".into()),
                status: "completed".into(),
            },
            CheckRun {
                name: "lint".into(),
                conclusion: Some("failure".into()),
                status: "completed".into(),
            },
        ];
        let pr = parse_pr_view_json(&v, runs).unwrap();
        assert_eq!(pr.checks, ChecksState::Failure);
        assert_eq!(pr.review_decision, None);
    }

    #[test]
    fn parse_merge_strategy_defaults_to_squash() {
        assert_eq!(parse_merge_strategy(None).unwrap(), MergeStrategy::Squash);
        assert_eq!(
            parse_merge_strategy(Some("Squash")).unwrap(),
            MergeStrategy::Squash
        );
        assert_eq!(
            parse_merge_strategy(Some("rebase")).unwrap(),
            MergeStrategy::Rebase
        );
        assert!(parse_merge_strategy(Some("foo")).is_err());
    }

    #[test]
    fn parse_merge_gate_policy_defaults_to_standard() {
        assert_eq!(
            parse_merge_gate_policy(None).unwrap().mode,
            MergeGatePolicyMode::Standard
        );
        assert_eq!(
            parse_merge_gate_policy(Some("permissive")).unwrap().mode,
            MergeGatePolicyMode::Permissive
        );
        assert_eq!(
            parse_merge_gate_policy(Some("strict")).unwrap().mode,
            MergeGatePolicyMode::Strict
        );
        assert!(parse_merge_gate_policy(Some("loose")).is_err());
    }

    #[test]
    fn safe_merge_effective_dry_run_requires_confirm() {
        assert!(effective_safe_merge_dry_run(false, None));
        assert!(effective_safe_merge_dry_run(false, Some(false)));
        assert!(effective_safe_merge_dry_run(false, Some(true)));
        assert!(!effective_safe_merge_dry_run(true, None));
        assert!(!effective_safe_merge_dry_run(true, Some(false)));
        assert!(effective_safe_merge_dry_run(true, Some(true)));
    }

    #[test]
    fn safe_merge_classifies_no_checks_reported_as_empty_checks_surface() {
        assert!(is_no_checks_reported(
            "no checks reported on the 'feature' branch"
        ));
        assert!(is_no_checks_reported("No checks reported"));
        assert!(!is_no_checks_reported("API rate limit exceeded"));
    }

    #[test]
    fn classify_gh_error_buckets() {
        assert!(matches!(
            classify_gh_error("HTTP 404 not found"),
            GhError::NotFound(_)
        ));
        assert!(matches!(
            classify_gh_error("API rate limit exceeded"),
            GhError::RateLimited(_)
        ));
        assert!(matches!(
            classify_gh_error("network blip"),
            GhError::Sanitized(_)
        ));
    }

    #[test]
    fn vault_unavailable_errors_allow_fallback() {
        assert!(vault_secret_unavailable("Secret not found: GH_TOKEN"));
        assert!(vault_secret_unavailable("Vault is locked"));
        assert!(vault_secret_unavailable(
            "Vault auto-locked. Call vault_unlock first."
        ));
        assert!(vault_secret_unavailable("Vault not initialized"));
        assert!(!vault_secret_unavailable("Vault decrypt failed"));
    }

    #[test]
    fn env_gh_token_prefers_gh_token_and_falls_back() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old_gh = std::env::var_os("GH_TOKEN");
        let old_github = std::env::var_os("GITHUB_TOKEN");
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");

        std::env::set_var("GITHUB_TOKEN", "github-token");
        assert_eq!(env_gh_token().as_deref(), Some("github-token"));
        std::env::set_var("GH_TOKEN", "gh-token");
        assert_eq!(env_gh_token().as_deref(), Some("gh-token"));
        std::env::set_var("GH_TOKEN", "   ");
        assert_eq!(env_gh_token().as_deref(), Some("github-token"));

        if let Some(v) = old_gh {
            std::env::set_var("GH_TOKEN", v);
        } else {
            std::env::remove_var("GH_TOKEN");
        }
        if let Some(v) = old_github {
            std::env::set_var("GITHUB_TOKEN", v);
        } else {
            std::env::remove_var("GITHUB_TOKEN");
        }
    }

    #[test]
    fn preserve_gh_env_keeps_auth_proxy_and_platform_env() {
        use std::ffi::OsStr;

        let _guard = ENV_LOCK.lock().unwrap();
        let old_https = std::env::var_os("HTTPS_PROXY");
        let old_cert = std::env::var_os("SSL_CERT_FILE");
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let old_git_ssh_command = std::env::var_os("GIT_SSH_COMMAND");
        let old_ssh_auth_sock = std::env::var_os("SSH_AUTH_SOCK");
        let old_gh = std::env::var_os("GH_TOKEN");
        let old_github = std::env::var_os("GITHUB_TOKEN");

        std::env::set_var("HTTPS_PROXY", "http://proxy.local:8080");
        std::env::set_var("SSL_CERT_FILE", "/tmp/test-ca.pem");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg");
        std::env::set_var("GIT_SSH_COMMAND", "sh -c 'echo should-not-run'");
        std::env::set_var("SSH_AUTH_SOCK", "/tmp/test-ssh-agent.sock");
        std::env::remove_var("GH_TOKEN");
        std::env::set_var("GITHUB_TOKEN", "github-env-token");

        let mut cmd = Command::new("gh");
        cmd.env_clear();
        preserve_gh_env(&mut cmd);

        let envs: Vec<_> = cmd.get_envs().collect();
        let get = |name: &str| {
            envs.iter()
                .find(|(k, _)| *k == OsStr::new(name))
                .and_then(|(_, v)| *v)
                .map(|v| v.to_string_lossy().to_string())
        };

        assert_eq!(
            get("HTTPS_PROXY").as_deref(),
            Some("http://proxy.local:8080")
        );
        assert_eq!(get("SSL_CERT_FILE").as_deref(), Some("/tmp/test-ca.pem"));
        assert_eq!(get("XDG_CONFIG_HOME").as_deref(), Some("/tmp/test-xdg"));
        assert_eq!(
            get("SSH_AUTH_SOCK").as_deref(),
            Some("/tmp/test-ssh-agent.sock")
        );
        assert_eq!(get("GIT_SSH_COMMAND"), None);
        assert_eq!(get("GITHUB_TOKEN").as_deref(), Some("github-env-token"));

        if let Some(v) = old_https {
            std::env::set_var("HTTPS_PROXY", v);
        } else {
            std::env::remove_var("HTTPS_PROXY");
        }
        if let Some(v) = old_cert {
            std::env::set_var("SSL_CERT_FILE", v);
        } else {
            std::env::remove_var("SSL_CERT_FILE");
        }
        if let Some(v) = old_xdg {
            std::env::set_var("XDG_CONFIG_HOME", v);
        } else {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        if let Some(v) = old_git_ssh_command {
            std::env::set_var("GIT_SSH_COMMAND", v);
        } else {
            std::env::remove_var("GIT_SSH_COMMAND");
        }
        if let Some(v) = old_ssh_auth_sock {
            std::env::set_var("SSH_AUTH_SOCK", v);
        } else {
            std::env::remove_var("SSH_AUTH_SOCK");
        }
        if let Some(v) = old_gh {
            std::env::set_var("GH_TOKEN", v);
        } else {
            std::env::remove_var("GH_TOKEN");
        }
        if let Some(v) = old_github {
            std::env::set_var("GITHUB_TOKEN", v);
        } else {
            std::env::remove_var("GITHUB_TOKEN");
        }
    }
}
