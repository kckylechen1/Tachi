use super::*;

// ─── Worktree merge handler ──────────────────────────────────────────────────

const PROTECTED_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

#[derive(Debug, Clone, Default)]
pub(crate) struct DeleteWorktreeSafety {
    pub allow_delete_worktree: bool,
    pub requires_human_review: bool,
    pub safety_warnings: Vec<String>,
}

pub(crate) fn is_protected_branch(branch: &str) -> bool {
    let normalized = branch.trim();
    PROTECTED_BRANCHES
        .iter()
        .any(|protected| protected.eq_ignore_ascii_case(normalized))
}

fn normalize_path_for_compare(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| std::path::PathBuf::from(path).to_string_lossy().to_string())
}

pub(crate) fn worktree_equals_repo_root(worktree: &str, repo_root: &str) -> bool {
    normalize_path_for_compare(worktree) == normalize_path_for_compare(repo_root)
}

pub(crate) fn validate_static_merge_safety(
    branch: &str,
    worktree: &str,
    repo_root: &str,
) -> Result<(), String> {
    if is_protected_branch(branch) {
        return Err(format!(
            "Refusing to merge protected branch '{branch}'. Use a feature branch instead."
        ));
    }
    if worktree_equals_repo_root(worktree, repo_root) {
        return Err(format!(
            "Refusing to use worktree '{worktree}' because it resolves to repository root/main worktree '{repo_root}'. Pass a linked feature worktree path."
        ));
    }
    Ok(())
}

pub(crate) fn evaluate_delete_worktree_safety(
    delete_worktree_requested: bool,
    has_upstream: bool,
    has_local_only_commits: bool,
) -> DeleteWorktreeSafety {
    let mut safety = DeleteWorktreeSafety::default();

    if !has_upstream {
        safety.requires_human_review = true;
        safety.safety_warnings.push(
            "Branch has no upstream tracking branch. Automatic worktree deletion is disabled; clean up manually after review."
                .to_string(),
        );
    }

    if has_local_only_commits {
        safety.requires_human_review = true;
        safety.safety_warnings.push(
            "Branch contains commits not confirmed on upstream. Automatic worktree deletion is disabled; clean up manually after review."
                .to_string(),
        );
    }

    safety.allow_delete_worktree = delete_worktree_requested && !safety.requires_human_review;
    safety
}

async fn resolve_worktree_top_level(worktree: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args([
            "-C",
            worktree,
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to resolve worktree path: {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "Not a valid git worktree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn resolve_repo_root_from_worktree(worktree: &str) -> Result<String, String> {
    let repo_root_out = Command::new("git")
        .args([
            "-C",
            worktree,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to find repo root: {e}"))?;

    if !repo_root_out.status.success() {
        return Err(format!(
            "Failed to locate repository root/main worktree from '{worktree}': {}",
            String::from_utf8_lossy(&repo_root_out.stderr).trim()
        ));
    }

    let git_common_dir = String::from_utf8_lossy(&repo_root_out.stdout)
        .trim()
        .to_string();
    Ok(std::path::Path::new(&git_common_dir)
        .parent()
        .unwrap_or(std::path::Path::new(&git_common_dir))
        .to_string_lossy()
        .to_string())
}

async fn ensure_repo_root_is_clean(repo_root: &str) -> Result<(), String> {
    let repo_check = Command::new("git")
        .args(["-C", repo_root, "rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .map_err(|e| format!("Failed to validate repository root worktree: {e}"))?;
    if !repo_check.status.success() {
        return Err(format!(
            "Repository root/main worktree '{repo_root}' is missing or invalid. Refusing to merge."
        ));
    }

    let status_out = Command::new("git")
        .args(["-C", repo_root, "status", "--porcelain"])
        .output()
        .await
        .map_err(|e| format!("Failed to inspect repository root status: {e}"))?;
    if !status_out.status.success() {
        return Err(format!(
            "Could not verify cleanliness of repository root/main worktree '{repo_root}'. Refusing to merge."
        ));
    }
    if !String::from_utf8_lossy(&status_out.stdout)
        .trim()
        .is_empty()
    {
        return Err(format!(
            "Repository root/main worktree '{repo_root}' is not clean. Commit or stash changes before approve_merge."
        ));
    }

    Ok(())
}

async fn detect_branch_safety_signals(worktree: &str) -> DeleteWorktreeSafety {
    let upstream_out = Command::new("git")
        .args([
            "-C",
            worktree,
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .output()
        .await;

    let mut has_upstream = false;
    let mut has_local_only_commits = false;
    let mut extra_warnings = Vec::new();

    if let Ok(out) = upstream_out {
        if out.status.success() {
            has_upstream = true;
            let upstream = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !upstream.is_empty() {
                let ahead_out = Command::new("git")
                    .args([
                        "-C",
                        worktree,
                        "rev-list",
                        "--count",
                        &format!("{upstream}..HEAD"),
                    ])
                    .output()
                    .await;
                match ahead_out {
                    Ok(count_out) if count_out.status.success() => {
                        let count = String::from_utf8_lossy(&count_out.stdout)
                            .trim()
                            .parse::<u64>()
                            .unwrap_or(0);
                        has_local_only_commits = count > 0;
                    }
                    _ => {
                        has_local_only_commits = true;
                        extra_warnings.push("Could not verify whether commits are fully pushed to upstream; requiring human review before deleting worktree.".to_string());
                    }
                }
            }
        }
    }

    let mut safety = evaluate_delete_worktree_safety(true, has_upstream, has_local_only_commits);
    if !extra_warnings.is_empty() {
        safety.requires_human_review = true;
        safety.allow_delete_worktree = false;
        safety.safety_warnings.extend(extra_warnings);
    }
    safety
}

pub(crate) async fn handle_approve_merge(
    params: crate::TachiApproveMergeParams,
) -> Result<String, String> {
    let worktree = resolve_worktree_top_level(&params.worktree).await?;

    let branch = if let Some(ref b) = params.branch {
        b.clone()
    } else {
        let out = Command::new("git")
            .args(["-C", &worktree, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .await
            .map_err(|e| format!("Failed to get branch from worktree: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Not a valid git worktree: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    let repo_root = resolve_repo_root_from_worktree(&worktree).await?;
    validate_static_merge_safety(&branch, &worktree, &repo_root)?;
    ensure_repo_root_is_clean(&repo_root).await?;

    let strategy = params.strategy.as_deref().unwrap_or("recursive");
    let mut safety = detect_branch_safety_signals(&worktree).await;
    if !params.delete_worktree {
        safety.allow_delete_worktree = false;
    } else if safety.requires_human_review {
        safety.allow_delete_worktree = false;
    }
    let safety_warnings = safety.safety_warnings.clone();
    let requires_human_review = safety.requires_human_review;

    if !params.confirm {
        let merge_out = Command::new("git")
            .args([
                "-C",
                &repo_root,
                "merge",
                "--strategy",
                strategy,
                "--no-commit",
                "--no-ff",
                &branch,
            ])
            .output()
            .await
            .map_err(|e| format!("Merge preview failed: {e}"))?;

        let merge_stdout = String::from_utf8_lossy(&merge_out.stdout).to_string();
        let merge_stderr = String::from_utf8_lossy(&merge_out.stderr).to_string();

        if !merge_out.status.success() {
            let _ = Command::new("git")
                .args(["-C", &repo_root, "merge", "--abort"])
                .output()
                .await;
            return serde_json::to_string(&json!({
                "preview": true,
                "can_merge": false,
                "branch": branch,
                "error": merge_stderr,
                "repo_root": repo_root,
                "safety_warnings": safety_warnings,
                "requires_human_review": requires_human_review,
            }))
            .map_err(|e| format!("serialize: {e}"));
        }

        let diff_out = Command::new("git")
            .args(["-C", &repo_root, "diff", "--stat", "HEAD"])
            .output()
            .await;
        let diff_stat = diff_out
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let _ = Command::new("git")
            .args(["-C", &repo_root, "merge", "--abort"])
            .output()
            .await;

        return serde_json::to_string(&json!({
            "preview": true,
            "can_merge": true,
            "branch": branch,
            "repo_root": repo_root,
            "merge_output": merge_stdout.trim(),
            "diff_stat": diff_stat.trim(),
            "safety_warnings": safety_warnings,
            "requires_human_review": requires_human_review,
            "delete_worktree_requested": params.delete_worktree,
            "delete_worktree_allowed": safety.allow_delete_worktree,
            "next_step": "Call approve_merge again with confirm=true to execute the merge.",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let merge_out = Command::new("git")
        .args(["-C", &repo_root, "merge", "--strategy", strategy, &branch])
        .output()
        .await
        .map_err(|e| format!("Merge command failed: {e}"))?;

    let merge_stdout = String::from_utf8_lossy(&merge_out.stdout).to_string();
    let merge_stderr = String::from_utf8_lossy(&merge_out.stderr).to_string();

    if !merge_out.status.success() {
        return serde_json::to_string(&json!({
            "merged": false,
            "branch": branch,
            "error": merge_stderr,
            "stdout": merge_stdout,
            "repo_root": repo_root,
            "safety_warnings": safety_warnings,
            "requires_human_review": requires_human_review,
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let mut worktree_removed = false;
    let mut auto_delete_skipped = false;
    if params.delete_worktree {
        if worktree_equals_repo_root(&worktree, &repo_root) {
            return Err(format!(
                "Refusing to delete worktree '{worktree}' because it resolves to repository root/main worktree '{repo_root}'. Clean up manually if needed."
            ));
        }
        if safety.allow_delete_worktree {
            let rm_out = Command::new("git")
                .args(["-C", &repo_root, "worktree", "remove", &worktree])
                .output()
                .await;
            worktree_removed = rm_out.map(|o| o.status.success()).unwrap_or(false);
        } else {
            auto_delete_skipped = true;
        }
    }

    serde_json::to_string(&json!({
        "merged": true,
        "branch": branch,
        "repo_root": repo_root,
        "worktree_removed": worktree_removed,
        "auto_delete_skipped": auto_delete_skipped,
        "delete_worktree_requested": params.delete_worktree,
        "delete_worktree_allowed": safety.allow_delete_worktree,
        "safety_warnings": safety_warnings,
        "requires_human_review": requires_human_review,
        "merge_output": merge_stdout.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
