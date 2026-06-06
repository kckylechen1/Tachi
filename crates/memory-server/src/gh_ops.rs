use crate::gh_safe_merge::{
    evaluate_merge_gate, evaluate_merge_gate_with_policy, ChecksState, GhClient, GhError,
    MergeDecision, MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable,
    PrLifecycleState, PrState, ReviewDecision,
};
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::{
    GhIssueCreateParams, GhIssueListParams, GhIssueReadParams, GhPrListParams, GhPrReadParams,
    GhRepoViewParams, TachiGhParams,
};
use crate::vault_ops::read_unlocked_vault_secret;
use crate::MemoryServer;
use async_trait::async_trait;
use serde_json::json;
use std::process::Command;

const GH_AGENT_ID: &str = "tachi_gh_ops";
const MAX_GH_OUTPUT_CHARS: usize = 50_000;
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
    "GIT_SSH_COMMAND",
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
            "Unknown action '{}'. Expected: repo_view, issue_list, issue_read, issue_create, pr_list, pr_read, safe_merge",
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
                "number,state,mergeable,reviewDecision,isDraft,headRefOid",
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
            .unwrap_or(0);
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

/// Parse the `gh pr view --json number,state,mergeable,reviewDecision,isDraft,headRefOid`
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
    Ok(PrState {
        number,
        state,
        mergeable,
        review_decision,
        checks: ChecksState::aggregate(&checks),
        is_draft,
        head_sha,
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
    let pr = client
        .pr_view(repo, pr_number)
        .await
        .map_err(|e| format!("pr_view failed: {e}"))?;
    let mut decision = if policy.mode == MergeGatePolicyMode::Standard {
        evaluate_merge_gate(&pr)
    } else {
        evaluate_merge_gate_with_policy(&pr, policy)
    };
    if policy.require_linked_issue_or_flow && flow_id.is_none() {
        decision = match decision {
            MergeDecision::Ready => MergeDecision::Pending {
                waiting_on: vec!["flow:missing".to_string()],
            },
            MergeDecision::Pending { mut waiting_on } => {
                waiting_on.push("flow:missing".to_string());
                MergeDecision::Pending { waiting_on }
            }
            blocked => blocked,
        };
    }
    let merge_state = decision.merge_state_label();
    let will_merge = matches!(decision, MergeDecision::Ready) && !dry_run;

    let (event_kind, event_payload, merged_sha) = match &decision {
        MergeDecision::Ready => {
            if dry_run {
                (
                    "github_review_gate_passed",
                    json!({
                        "repo": repo,
                        "pr_number": pr_number,
                        "head_sha": pr.head_sha,
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
        "head_consistency": {
            "head_sha": pr.head_sha,
            "checks_head_sha": null,
            "review_decision_head_sha": null,
            "head_consistent": !policy.require_head_consistency,
            "source": "single_pr_snapshot",
        },
        "checks": {
            "state": match pr.checks {
                ChecksState::None => "none",
                ChecksState::Pending => "pending",
                ChecksState::Success => "success",
                ChecksState::Failure => "failure",
            },
            "required": policy.require_checks,
            "allow_missing": policy.allow_missing_checks,
            "source": "gh_pr_checks",
            "head_consistent": !policy.require_head_consistency,
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
            "required": policy.require_linked_issue_or_flow,
        },
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
        assert!(v["merged_sha"].is_null());
        assert_eq!(v["status_patch"]["checks"]["required"], true);
        assert_eq!(v["status_patch"]["review"]["required"], true);
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
        assert!(v["merged_sha"].is_string());
        assert_eq!(v["event"]["kind"], "github_pr_merged");
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
        let run_dir = tmp.path().join(flow);
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
                .unwrap();
        assert_eq!(status["github"]["merge_state"], "ready");
        assert_eq!(status["github"]["policy"], "standard");
        assert_eq!(status["github"]["will_merge"], false);
        assert_eq!(status["github"]["pr_number"], 42);
        let events = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
        assert!(events.contains("\"github_review_gate_passed\""));
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
        });
        let pr = parse_pr_view_json(&v, vec![]).unwrap();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.state, PrLifecycleState::Open);
        assert_eq!(pr.mergeable, Mergeable::Mergeable);
        assert_eq!(pr.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(pr.checks, ChecksState::None);
        assert!(!pr.is_draft);
        assert_eq!(pr.head_sha, "abc123");
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
        let old_gh = std::env::var_os("GH_TOKEN");
        let old_github = std::env::var_os("GITHUB_TOKEN");

        std::env::set_var("HTTPS_PROXY", "http://proxy.local:8080");
        std::env::set_var("SSL_CERT_FILE", "/tmp/test-ca.pem");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg");
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
