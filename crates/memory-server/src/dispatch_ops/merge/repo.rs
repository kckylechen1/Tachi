use tokio::process::Command;

pub(in crate::dispatch_ops::merge) async fn resolve_worktree_top_level(
    worktree: &str,
) -> Result<String, String> {
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

pub(in crate::dispatch_ops::merge) async fn resolve_repo_root_from_worktree(
    worktree: &str,
) -> Result<String, String> {
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

pub(in crate::dispatch_ops::merge) async fn ensure_repo_root_is_clean(
    repo_root: &str,
) -> Result<(), String> {
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

pub(in crate::dispatch_ops::merge) async fn current_worktree_branch(
    worktree: &str,
    label: &str,
) -> Result<String, String> {
    let out = Command::new("git")
        .args(["-C", worktree, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
        .map_err(|e| format!("Failed to get {label}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Failed to resolve {label}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
