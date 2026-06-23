use crate::gh_safe_merge::{
    evaluate_merge_gate_with_policy, ChecksState, GhClient, GhError, MergeDecision,
    MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable, PrLifecycleState,
    PrState, ReviewDecision,
};
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::{
    GhCommentParams, GhIssueCreateParams, GhIssueListParams, GhIssueReadParams, GhPrCommentsParams,
    GhPrListParams, GhPrReadParams, GhRepoViewParams, TachiGhParams,
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

mod review_digest;
mod safe_merge;

#[cfg(test)]
mod safe_merge_tests;

use self::review_digest::*;
use self::safe_merge::*;

pub(crate) use self::safe_merge::{handle_github_safe_merge, CliGhClient};

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

async fn handle_gh_issue_read(
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

async fn handle_gh_issue_list(
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

async fn handle_gh_issue_create(
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

/// Post a comment to a GitHub issue or PR. `kind` is "issue" or "pr".
/// This is the write-back arc of the closure loop: closure results, review
/// verdicts, and reap notices land back on the source issue/PR. `dry_run`
/// returns a preview without posting.
/// Best-effort idempotency probe: does this issue/PR already carry a comment
/// containing `marker`? Lets the closure write-back avoid double-posting on
/// re-run (which would spam the issue and erode trust in the loop). Any error
/// (gh down, parse failure) returns false so the caller proceeds — we'd rather
/// risk a rare duplicate than silently swallow the write-back.
pub(crate) fn gh_comment_marker_present(
    server: &MemoryServer,
    kind: &str,
    repo: &str,
    number: u64,
    marker: &str,
) -> bool {
    let Ok((mut cmd, token)) = build_gh_command(server) else {
        return false;
    };
    cmd.args([kind, "view", &number.to_string()])
        .args(["--repo", repo])
        .args(["--json", "comments"]);
    let Ok(output) = run_gh_json(cmd, &token) else {
        return false;
    };
    serde_json::from_str::<Value>(&output)
        .ok()
        .and_then(|value| {
            value
                .get("comments")
                .and_then(Value::as_array)
                .map(|comments| {
                    comments.iter().any(|c| {
                        c.get("body")
                            .and_then(Value::as_str)
                            .is_some_and(|body| body.contains(marker))
                    })
                })
        })
        .unwrap_or(false)
}

pub(crate) async fn handle_gh_comment(
    server: &MemoryServer,
    kind: &str,
    params: GhCommentParams,
) -> Result<String, String> {
    validate_repo(&params.repo)?;
    let body = params.body.unwrap_or_default();
    if body.trim().is_empty() {
        return Err(format!(
            "{kind}_comment requires a non-empty 'body' parameter"
        ));
    }

    if params.dry_run {
        return serde_json::to_string(&json!({
            "tool": format!("tachi_gh_{kind}_comment"),
            "repo": params.repo,
            "number": params.number,
            "dry_run": true,
            "preview_body": body,
            "note": "dry_run=true: comment was NOT posted",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args([kind, "comment", &params.number.to_string()])
        .args(["--repo", &params.repo])
        .args(["--body", &body]);

    let output = run_gh(cmd, &token)?;
    serde_json::to_string(&json!({
        "tool": format!("tachi_gh_{kind}_comment"),
        "repo": params.repo,
        "number": params.number,
        "result": output.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

async fn handle_gh_pr_read(
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

async fn handle_gh_pr_list(
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

async fn handle_gh_repo_view(
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
    }
}
