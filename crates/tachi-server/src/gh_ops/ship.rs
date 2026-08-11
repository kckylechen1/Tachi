use super::*;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct ShipFilePlan {
    path: String,
    state: &'static str,
}

#[derive(Debug, Clone)]
struct CreatedPr {
    number: u64,
    url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::gh_ops) struct ResolvedBase {
    pub base: String,
    pub baseref: String,
    pub base_pushed_needed: bool,
}

pub(in crate::gh_ops) async fn handle_github_ship(
    server: &MemoryServer,
    params: &TachiGhParams,
) -> Result<String, String> {
    handle_github_ship_inner(Some(server), params).await
}

pub(in crate::gh_ops) async fn handle_github_ship_inner(
    server: Option<&MemoryServer>,
    params: &TachiGhParams,
) -> Result<String, String> {
    let repo_root = resolve_repo_root(params.cwd.as_deref())?;
    let branch = current_branch(&repo_root)?;
    if matches!(branch.as_str(), "main" | "master") {
        return Err(format!(
            "protected_branch: ship refuses to commit directly on branch '{branch}'"
        ));
    }
    if branch == "HEAD" {
        return Err("detached_head: ship requires a named feature branch".to_string());
    }
    if let Some(expected) = params
        .expect_branch
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if expected != branch {
            return Err(format!(
                "expect_branch_mismatch: expected '{expected}', current branch is '{branch}'"
            ));
        }
    }
    if params.files.is_empty() && params.commit_message.is_none() {
        return handle_contract_ship(server, params, &repo_root, &branch).await;
    }
    validate_files(&params.files)?;
    let commit_message = params
        .commit_message
        .as_deref()
        .ok_or_else(|| "commit_message is required; the caller must author it".to_string())?;
    if commit_message.is_empty() {
        return Err(
            "commit_message is required and must be non-empty; the caller must author it"
                .to_string(),
        );
    }

    let plan = build_file_plan(&repo_root, &params.files)?;
    reject_unshippable_files(&plan)?;
    let pr_requested = params.pr_title.is_some() && params.pr_body.is_some();

    if !params.confirm {
        return serde_json::to_string(&json!({
            "status": "dry_run",
            "branch": branch,
            "plan": {
                "branch": branch,
                "files": plan_to_json(&plan),
                "commit_message_len": commit_message.len(),
                "pr_requested": pr_requested,
            },
            "steps": {
                "gates": "passed",
                "staged": "dry_run",
                "committed": null,
                "pushed": "dry_run",
                "pr": null,
                "linked": null,
                "event_appended": false,
            },
            "warnings": [],
        }))
        .map_err(|err| format!("serialize ship dry-run response: {err}"));
    }

    git_add_exact(&repo_root, &params.files)?;
    verify_cached_set(&repo_root, &params.files)?;
    let sha = git_commit_verbatim(&repo_root, commit_message)?;

    let mut status = "completed";
    let mut warnings: Vec<String> = Vec::new();
    let mut steps = json!({
        "gates": "passed",
        "staged": "passed",
        "committed": { "sha": sha },
        "pushed": null,
        "pr": null,
        "linked": null,
        "event_appended": false,
    });

    let mut created_pr: Option<CreatedPr> = None;
    if origin_remote_exists(&repo_root) {
        match git_push_origin(&repo_root, &branch) {
            Ok(()) => {
                steps["pushed"] = json!("pushed");
                if pr_requested {
                    if let Some(server) = server {
                        let base = params
                            .pr_base
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .unwrap_or("main");
                        let title = params.pr_title.as_deref().ok_or_else(|| {
                            "pr_title missing after pr_requested gate".to_string()
                        })?;
                        let body = params
                            .pr_body
                            .as_deref()
                            .ok_or_else(|| "pr_body missing after pr_requested gate".to_string())?;
                        match create_pull_request(
                            server,
                            &repo_root,
                            &branch,
                            base,
                            title,
                            body,
                            params.repo.as_deref(),
                        )
                        .await
                        {
                            Ok(pr) => {
                                steps["pr"] = json!({
                                    "number": pr.number,
                                    "url": pr.url,
                                });
                                if let Some(flow_id) = params
                                    .flow_id
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|v| !v.is_empty())
                                {
                                    match link_created_pr(server, flow_id, &pr.url).await {
                                        Ok(linked) => {
                                            steps["linked"] = linked;
                                        }
                                        Err(err) => {
                                            status = "partial";
                                            warnings.push(format!(
                                                "commit {sha} pushed and PR created but link_pr failed: {err}"
                                            ));
                                            steps["linked"] = json!({ "error": err });
                                        }
                                    }
                                } else {
                                    steps["linked"] = json!("skipped");
                                }
                                created_pr = Some(pr);
                            }
                            Err(err) => {
                                status = "partial";
                                warnings.push(format!(
                                    "commit {sha} pushed but PR creation failed; open the PR manually: {err}"
                                ));
                                steps["pr"] = json!({ "error": err });
                                steps["linked"] = json!("skipped");
                            }
                        }
                    } else {
                        let err = "ship pr creation requires a MemoryServer for GitHub transport"
                            .to_string();
                        status = "partial";
                        warnings.push(format!(
                            "commit {sha} pushed but PR creation failed; open the PR manually: {err}"
                        ));
                        steps["pr"] = json!({ "error": err });
                        steps["linked"] = json!("skipped");
                    }
                } else {
                    steps["pr"] = json!("skipped");
                    steps["linked"] = json!("skipped");
                }
            }
            Err(err) => {
                status = "partial";
                warnings.push(format!(
                    "commit {sha} created locally but push failed; push manually and open the PR yourself"
                ));
                steps["pushed"] = json!({ "error": err });
                steps["pr"] = json!("skipped");
                steps["linked"] = json!("skipped");
            }
        }
    } else {
        status = "partial";
        warnings
            .push("origin remote not found; committed locally, skipped push and PR".to_string());
        steps["pushed"] = json!("no_remote");
        steps["pr"] = json!("skipped");
        steps["linked"] = json!("skipped");
    }

    if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if let Err(err) = append_ship_flow_event(
            flow_id,
            status,
            &branch,
            &params.files,
            &sha,
            &mut steps,
            &warnings,
            created_pr.as_ref(),
        ) {
            status = "partial";
            warnings.push(format!("append github ship event failed: {err}"));
        }
    }

    serde_json::to_string(&json!({
        "status": status,
        "branch": branch,
        "steps": steps,
        "warnings": warnings,
    }))
    .map_err(|err| format!("serialize ship response: {err}"))
}

fn resolve_repo_root(cwd: Option<&str>) -> Result<PathBuf, String> {
    let start = cwd
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|err| format!("resolve cwd: {err}"))?;
    let root = run_git(&start, &["rev-parse", "--show-toplevel"])
        .map_err(|err| format!("not_git_repo: {err}"))?;
    Ok(PathBuf::from(root.trim()))
}

fn current_branch(repo: &Path) -> Result<String, String> {
    let branch = run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(branch.trim().to_string())
}

fn validate_files(files: &[String]) -> Result<(), String> {
    if files.is_empty() {
        return Err("files must be a non-empty exact staging list".to_string());
    }
    let mut seen = BTreeSet::new();
    for file in files {
        if file.is_empty() {
            return Err("files contains an empty path".to_string());
        }
        let path = Path::new(file);
        if path.is_absolute() {
            return Err(format!("files contains absolute path '{file}'"));
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("files contains path traversal '{file}'"));
        }
        if !seen.insert(file) {
            return Err(format!("files contains duplicate path '{file}'"));
        }
    }
    Ok(())
}

fn build_file_plan(repo: &Path, files: &[String]) -> Result<Vec<ShipFilePlan>, String> {
    files
        .iter()
        .map(|path| {
            let state = file_state(repo, path)?;
            Ok(ShipFilePlan {
                path: path.clone(),
                state,
            })
        })
        .collect()
}

fn file_state(repo: &Path, path: &str) -> Result<&'static str, String> {
    if repo.join(path).symlink_metadata().is_err() {
        return Ok("missing");
    }
    let status = run_git(repo, &["status", "--porcelain=v1", "--", path])?;
    if status.trim().is_empty() {
        return Ok("unchanged");
    }
    if status.lines().any(|line| line.starts_with("?? ")) {
        return Ok("untracked");
    }
    Ok("modified")
}

fn reject_unshippable_files(plan: &[ShipFilePlan]) -> Result<(), String> {
    let unchanged: Vec<&str> = plan
        .iter()
        .filter(|item| item.state == "unchanged")
        .map(|item| item.path.as_str())
        .collect();
    let missing: Vec<&str> = plan
        .iter()
        .filter(|item| item.state == "missing")
        .map(|item| item.path.as_str())
        .collect();
    if unchanged.is_empty() && missing.is_empty() {
        return Ok(());
    }
    let mut parts = Vec::new();
    if !unchanged.is_empty() {
        parts.push(format!("unchanged files: {}", unchanged.join(", ")));
    }
    if !missing.is_empty() {
        parts.push(format!("missing files: {}", missing.join(", ")));
    }
    Err(format!("ship plan failed: {}", parts.join("; ")))
}

fn plan_to_json(plan: &[ShipFilePlan]) -> Vec<Value> {
    plan.iter()
        .map(|item| {
            json!({
                "path": item.path,
                "state": item.state,
            })
        })
        .collect()
}

fn git_add_exact(repo: &Path, files: &[String]) -> Result<(), String> {
    let mut args = vec![OsString::from("add"), OsString::from("--")];
    args.extend(files.iter().map(|file| OsString::from(file.as_str())));
    run_git_os(repo, args).map(|_| ())
}

fn verify_cached_set(repo: &Path, files: &[String]) -> Result<(), String> {
    let expected: BTreeSet<String> = files.iter().cloned().collect();
    let staged_raw = run_git(
        repo,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "--name-only",
        ],
    )?;
    let staged: BTreeSet<String> = staged_raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if staged == expected {
        return Ok(());
    }
    let mut args = vec![
        OsString::from("reset"),
        OsString::from("-q"),
        OsString::from("--"),
    ];
    args.extend(files.iter().map(|file| OsString::from(file.as_str())));
    let _ = run_git_os(repo, args);
    Err(format!(
        "staged_mismatch: expected exactly {:?}, got {:?}",
        expected, staged
    ))
}

pub(in crate::gh_ops) struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Creates `path` exclusively and writes `message` into it, returning the
/// delete-on-drop guard for that file.
///
/// The guard is armed **only after `create_new` wins the race**. Arming it
/// before the open (the previous shape) made a caller that lost an
/// `AlreadyExists` race delete the *winner's* in-flight file, so the winner's
/// `git commit -F` then failed with `could not read log file ... No such file
/// or directory`. A loser must never delete a file it did not create.
pub(in crate::gh_ops) fn write_commit_message_file(
    path: &Path,
    message: &str,
) -> Result<TempFileGuard, String> {
    let mut temp = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| format!("create commit message tempfile: {err}"))?;
    // Ours now: from here on every failure path must clean the file up.
    let guard = TempFileGuard(path.to_path_buf());
    temp.write_all(message.as_bytes())
        .map_err(|err| format!("write commit message tempfile: {err}"))?;
    temp.flush()
        .map_err(|err| format!("flush commit message tempfile: {err}"))?;
    Ok(guard)
}

fn git_commit_verbatim(repo: &Path, message: &str) -> Result<String, String> {
    let temp_path = commit_message_temp_path()?;
    let _guard = write_commit_message_file(&temp_path, message)?;
    run_git_os(
        repo,
        vec![
            OsString::from("commit"),
            OsString::from("--cleanup=verbatim"),
            OsString::from("-F"),
            temp_path.as_os_str().to_os_string(),
        ],
    )?;
    let sha = run_git(repo, &["rev-parse", "HEAD"])?;
    Ok(sha.trim().to_string())
}

/// A per-call unique path for the commit-message tempfile.
///
/// `pid` separates processes; the process-wide counter separates *threads*,
/// which the timestamp alone does not: `SystemTime::now()` is only
/// microsecond-granular on macOS, so two concurrent ships could mint the exact
/// same `pid-nanos` name and collide on `create_new`.
pub(in crate::gh_ops) fn commit_message_temp_path() -> Result<PathBuf, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock before UNIX_EPOCH: {err}"))?
        .as_nanos();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(std::env::temp_dir().join(format!(
        "tachi-ship-commit-message-{}-{nanos}-{seq}.txt",
        std::process::id()
    )))
}

fn origin_remote_exists(repo: &Path) -> bool {
    run_git(repo, &["remote", "get-url", "origin"]).is_ok()
}

fn git_push_origin(repo: &Path, branch: &str) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["push", "-u", "origin", branch])
        .output()
        .map_err(|err| format!("execute git push: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return Err(stderr);
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return Err(stdout);
    }
    Err(format!(
        "git push failed with exit {}",
        output.status.code().unwrap_or(-1)
    ))
}

pub(in crate::gh_ops) fn resolve_base_ref(repo: &Path, base: &str) -> Result<ResolvedBase, String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("base_not_found: base name is empty".to_string());
    }
    let origin_ref = format!("origin/{base}");
    if git_ref_exists(repo, &origin_ref) {
        return Ok(ResolvedBase {
            base: base.to_string(),
            baseref: origin_ref,
            base_pushed_needed: false,
        });
    }
    if git_ref_exists(repo, base) {
        return Ok(ResolvedBase {
            base: base.to_string(),
            baseref: base.to_string(),
            base_pushed_needed: true,
        });
    }
    Err(format!(
        "base_not_found: no local or origin ref for base '{base}'"
    ))
}

fn git_ref_exists(repo: &Path, ref_name: &str) -> bool {
    run_git(repo, &["rev-parse", "--verify", ref_name]).is_ok()
}

fn collect_commits_oneline(repo: &Path, baseref: &str) -> Result<Vec<String>, String> {
    let range = format!("{baseref}..HEAD");
    let output = run_git(repo, &["log", "--oneline", "--no-merges", range.as_str()])?;
    let lines: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if lines.is_empty() {
        return Err(format!(
            "nothing_to_ship: no commits between {baseref} and HEAD"
        ));
    }
    Ok(lines)
}

fn last_commit_subject(log_lines: &[String]) -> String {
    log_lines
        .first()
        .and_then(|line| line.split_once(' ').map(|x| x.1))
        .unwrap_or_default()
        .to_string()
}

pub(in crate::gh_ops) fn build_contract_pr_body(
    issue_ref: Option<&str>,
    log_lines: &[String],
    tests_run: &[String],
) -> String {
    let mut body = String::new();
    if let Some(issue_ref) = issue_ref {
        body.push_str("Refs ");
        body.push_str(issue_ref);
        body.push('\n');
        body.push('\n');
    }
    body.push_str("## Commits (one bounded contract, batched)\n");
    for line in log_lines {
        body.push_str("- ");
        body.push_str(line);
        body.push('\n');
    }
    body.push('\n');
    body.push_str("## Tested (full suite, once)\n");
    if tests_run.is_empty() {
        body.push_str("_(not run — record the full-suite command via tests_run)_\n");
    } else {
        for test in tests_run {
            body.push_str("- ");
            body.push_str(test);
            body.push('\n');
        }
    }
    body.push('\n');
    body.push_str("## Not-tested\n");
    body.push_str("_fill honest gaps_\n");
    body
}

fn contract_coaching_line(base_name: &str) -> String {
    if base_name == "main" {
        "This contract's ceremony is DONE — owner merges. If your mandate has more contracts, start the next one NOW.".to_string()
    } else {
        format!(
            "Self-merge into '{base_name}' is allowed after review; ONE reviewed PR {base_name} -> main at campaign end. Start the next slice NOW."
        )
    }
}

async fn find_open_pr_for_head(
    server: &MemoryServer,
    repo_root: &Path,
    branch: &str,
    repo: Option<&str>,
) -> Result<Option<CreatedPr>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.current_dir(repo_root)
        .args(["pr", "list", "--state", "open", "--head", branch])
        .args(["--json", "number,url"]);
    if let Some(repo) = repo.map(str::trim).filter(|value| !value.is_empty()) {
        validate_repo(repo)?;
        cmd.args(["--repo", repo]);
    }
    let raw = run_gh(cmd, &token).map_err(|err| format!("pr_exists_check_failed: {err}"))?;
    let parsed: Vec<Value> = serde_json::from_str(raw.trim())
        .map_err(|err| format!("pr_exists_check_failed: parse gh pr list JSON: {err}"))?;
    Ok(parsed.first().and_then(|entry| {
        let number = entry.get("number")?.as_u64()?;
        let url = entry.get("url")?.as_str()?.to_string();
        Some(CreatedPr { number, url })
    }))
}

async fn handle_contract_ship(
    server: Option<&MemoryServer>,
    params: &TachiGhParams,
    repo_root: &Path,
    branch: &str,
) -> Result<String, String> {
    let base_name = params
        .pr_base
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("main");
    if branch == base_name {
        return Err(format!(
            "self_pr: branch '{branch}' equals pr_base; pick a different pr_base or branch"
        ));
    }
    let resolved = resolve_base_ref(repo_root, base_name)?;
    let log_lines = collect_commits_oneline(repo_root, &resolved.baseref)?;
    let commit_count = log_lines.len();
    let pr_title = params
        .pr_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| last_commit_subject(&log_lines));
    let issue_ref = params
        .issue_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let pr_body = build_contract_pr_body(issue_ref, &log_lines, &params.tests_run);

    if !params.confirm {
        let mut response = json!({
            "status": "dry_run",
            "mode": "contract",
            "branch": branch,
            "base": resolved.base,
            "baseref": resolved.baseref,
            "base_pushed_needed": resolved.base_pushed_needed,
            "commit_count": commit_count,
            "pr_title": pr_title,
            "pr_body_preview": pr_body,
            "steps": {
                "gates": "passed",
                "pushed": "dry_run",
                "pr": "dry_run",
                "linked": null,
                "event_appended": false,
            },
            "warnings": [],
        });
        if server.is_none() {
            response["pr_exists_check"] = json!("deferred");
        }
        return serde_json::to_string(&response)
            .map_err(|err| format!("serialize contract ship dry-run response: {err}"));
    }

    if let Some(server) = server {
        if let Some(existing) =
            find_open_pr_for_head(server, repo_root, branch, params.repo.as_deref()).await?
        {
            return serde_json::to_string(&json!({
                "status": "existing_pr",
                "number": existing.number,
                "url": existing.url,
            }))
            .map_err(|err| format!("serialize existing_pr response: {err}"));
        }
    }

    let sha = run_git(repo_root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let mut status = "completed";
    let mut warnings: Vec<String> = Vec::new();
    let mut steps = json!({
        "gates": "passed",
        "staged": "skipped",
        "committed": "skipped",
        "pushed": null,
        "pr": null,
        "linked": null,
        "event_appended": false,
    });
    let mut created_pr: Option<CreatedPr> = None;
    let coaching = server.map(|_| contract_coaching_line(base_name));

    if resolved.base_pushed_needed {
        if !origin_remote_exists(repo_root) {
            return Err(
                "base_push_failed: origin remote not found for local-only base".to_string(),
            );
        }
        git_push_origin(repo_root, &resolved.base)
            .map_err(|err| format!("base_push_failed: {err}"))?;
    }

    if origin_remote_exists(repo_root) {
        match git_push_origin(repo_root, branch) {
            Ok(()) => {
                steps["pushed"] = json!("pushed");
                if let Some(server) = server {
                    match create_pull_request(
                        server,
                        repo_root,
                        branch,
                        base_name,
                        &pr_title,
                        &pr_body,
                        params.repo.as_deref(),
                    )
                    .await
                    {
                        Ok(pr) => {
                            steps["pr"] = json!({
                                "number": pr.number,
                                "url": pr.url,
                            });
                            if let Some(flow_id) = params
                                .flow_id
                                .as_deref()
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                            {
                                match link_created_pr(server, flow_id, &pr.url).await {
                                    Ok(linked) => {
                                        steps["linked"] = linked;
                                    }
                                    Err(err) => {
                                        status = "partial";
                                        warnings.push(format!(
                                            "branch {branch} pushed and PR created but link_pr failed: {err}"
                                        ));
                                        steps["linked"] = json!({ "error": err });
                                    }
                                }
                            } else {
                                steps["linked"] = json!("skipped");
                            }
                            created_pr = Some(pr);
                        }
                        Err(err) => {
                            status = "partial";
                            warnings.push(format!(
                                "branch {branch} pushed but PR creation failed; open the PR manually: {err}"
                            ));
                            steps["pr"] = json!({ "error": err });
                            steps["linked"] = json!("skipped");
                        }
                    }
                } else {
                    let err =
                        "ship pr creation requires a MemoryServer for GitHub transport".to_string();
                    status = "partial";
                    warnings.push(format!(
                        "branch {branch} pushed but PR creation failed; open the PR manually: {err}"
                    ));
                    steps["pr"] = json!({ "error": err });
                    steps["linked"] = json!("skipped");
                }
            }
            Err(err) => {
                status = "partial";
                warnings.push(
                    "branch ready locally but push failed; push manually and open the PR yourself"
                        .to_string(),
                );
                steps["pushed"] = json!({ "error": err });
                steps["pr"] = json!("skipped");
                steps["linked"] = json!("skipped");
            }
        }
    } else {
        status = "partial";
        warnings.push("origin remote not found; skipped push and PR".to_string());
        steps["pushed"] = json!("no_remote");
        steps["pr"] = json!("skipped");
        steps["linked"] = json!("skipped");
    }

    if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if let Err(err) = append_ship_flow_event(
            flow_id,
            status,
            branch,
            &[],
            &sha,
            &mut steps,
            &warnings,
            created_pr.as_ref(),
        ) {
            status = "partial";
            warnings.push(format!("append github ship event failed: {err}"));
        }
    }

    let mut response = json!({
        "status": status,
        "mode": "contract",
        "branch": branch,
        "steps": steps,
        "warnings": warnings,
    });
    if let Some(coaching) = coaching {
        response["coaching"] = json!(coaching);
    }
    if server.is_none() {
        response["pr_exists_check"] = json!("deferred");
    }
    serde_json::to_string(&response)
        .map_err(|err| format!("serialize contract ship response: {err}"))
}

async fn create_pull_request(
    server: &MemoryServer,
    repo_root: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
    repo: Option<&str>,
) -> Result<CreatedPr, String> {
    // TODO: migrate `pr create --body` to `--body-file` for gh shim compatibility.
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.current_dir(repo_root)
        .args(["pr", "create"])
        .args(["--base", base])
        .args(["--head", branch])
        .args(["--title", title])
        .args(["--body", body]);
    if let Some(repo) = repo.map(str::trim).filter(|value| !value.is_empty()) {
        validate_repo(repo)?;
        cmd.args(["--repo", repo]);
    }
    let raw = run_gh(cmd, &token)?;
    let (number, url) = parse_created_pr_url(&raw)?;
    Ok(CreatedPr { number, url })
}

pub(in crate::gh_ops) fn parse_created_pr_url(raw: &str) -> Result<(u64, String), String> {
    let url = raw
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("https://") && line.contains("/pull/"))
        .unwrap_or_else(|| raw.trim())
        .to_string();
    let number = url
        .rsplit('/')
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| format!("gh pr create returned an unparseable PR URL: {url}"))?;
    Ok((number, url))
}

async fn link_created_pr(
    server: &MemoryServer,
    flow_id: &str,
    pr_ref: &str,
) -> Result<Value, String> {
    // action is ignored by handle_task_link_pr; use a valid tachi_task primary
    // after #757 removed link_pr from TachiTaskAction.
    let task_params: crate::tool_params::TachiTaskParams = serde_json::from_value(json!({
        "action": "status",
        "flow_id": flow_id,
        "pr_ref": pr_ref,
    }))
    .map_err(|err| format!("build link_pr params: {err}"))?;
    let raw = Box::pin(crate::task_lifecycle::handle_task_link_pr(
        server,
        &task_params,
    ))
    .await?;
    Ok(serde_json::from_str(&raw).unwrap_or(json!(raw)))
}

fn append_ship_flow_event(
    flow_id: &str,
    status: &str,
    branch: &str,
    files: &[String],
    sha: &str,
    steps: &mut Value,
    warnings: &[String],
    pr: Option<&CreatedPr>,
) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    fs::create_dir_all(&run_dir).map_err(|err| format!("create ship run dir: {err}"))?;
    let mut persisted_steps = steps.clone();
    persisted_steps["event_appended"] = json!(true);
    let pr_patch = pr
        .map(|pr| {
            json!({
                "pr_number": pr.number,
                "pr_url": pr.url,
            })
        })
        .unwrap_or_else(|| json!({}));
    let mut status_patch = json!({
        "ship": {
            "status": status,
            "branch": branch,
            "commit_sha": sha,
            "files": files,
            "steps": persisted_steps.clone(),
            "warnings": warnings,
        },
    });
    if let (Some(obj), Some(pr)) = (status_patch.as_object_mut(), pr) {
        obj.insert("pr_number".to_string(), json!(pr.number));
        obj.insert("pr_url".to_string(), json!(pr.url));
    }
    merge_github_status(&run_dir, status_patch)?;
    append_github_event(
        &run_dir,
        flow_id,
        "github_ship_completed",
        json!({
            "status": status,
            "branch": branch,
            "commit_sha": sha,
            "files": files,
            "steps": persisted_steps,
            "warnings": warnings,
            "pr": pr_patch,
        }),
    )?;
    steps["event_appended"] = json!(true);
    Ok(())
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run_git_os(repo, args.iter().map(OsString::from).collect())
}

fn run_git_os(repo: &Path, args: Vec<OsString>) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(&args)
        .output()
        .map_err(|err| format!("execute git: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        return Ok(stdout);
    }
    let rendered_args = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    Err(format!(
        "git {rendered_args} failed (exit {}): {}",
        output.status.code().unwrap_or(-1),
        stderr.trim()
    ))
}
