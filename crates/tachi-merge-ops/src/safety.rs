use tokio::process::Command;

const PROTECTED_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

#[derive(Debug, Clone, Default)]
pub struct DeleteWorktreeSafety {
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

pub fn worktree_equals_repo_root(worktree: &str, repo_root: &str) -> bool {
    normalize_path_for_compare(worktree) == normalize_path_for_compare(repo_root)
}

pub fn validate_merge_branch_name(branch: &str) -> Result<(), String> {
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return Err("Branch name must be non-empty.".to_string());
    }
    if trimmed.starts_with('-') {
        return Err(format!(
            "Refusing branch '{branch}' because refs beginning with '-' can be interpreted as git options."
        ));
    }
    if trimmed != branch {
        return Err("Branch name must not contain leading or trailing whitespace.".to_string());
    }
    Ok(())
}

pub fn validate_static_merge_safety(
    branch: &str,
    worktree: &str,
    repo_root: &str,
) -> Result<(), String> {
    validate_merge_branch_name(branch)?;
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

pub fn evaluate_delete_worktree_safety(
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

pub(crate) async fn detect_branch_safety_signals(worktree: &str) -> DeleteWorktreeSafety {
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
