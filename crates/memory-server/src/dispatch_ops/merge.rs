use super::*;

// ─── Worktree merge handler ──────────────────────────────────────────────────

pub(crate) async fn handle_approve_merge(
    params: crate::TachiApproveMergeParams,
) -> Result<String, String> {
    let worktree = &params.worktree;

    let branch = if let Some(ref b) = params.branch {
        b.clone()
    } else {
        let out = Command::new("git")
            .args(["-C", worktree, "rev-parse", "--abbrev-ref", "HEAD"])
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
    let git_common_dir = String::from_utf8_lossy(&repo_root_out.stdout)
        .trim()
        .to_string();
    let repo_root = std::path::Path::new(&git_common_dir)
        .parent()
        .unwrap_or(std::path::Path::new(&git_common_dir))
        .to_string_lossy()
        .to_string();

    let strategy = params.strategy.as_deref().unwrap_or("recursive");

    if !params.confirm {
        // ── Preview mode: dry-run merge, return diff without committing ──
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
            return serde_json::to_string(&json!({
                "preview": true,
                "can_merge": false,
                "branch": branch,
                "error": merge_stderr,
            }))
            .map_err(|e| format!("serialize: {e}"));
        }

        // Get the diff of what would be merged
        let diff_out = Command::new("git")
            .args(["-C", &repo_root, "diff", "--stat", "HEAD"])
            .output()
            .await;
        let diff_stat = diff_out
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        // Abort the merge to restore clean state
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
            "next_step": "Call approve_merge again with confirm=true to execute the merge.",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    // ── Confirm mode: execute the real merge ──
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
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let mut worktree_removed = false;
    if params.delete_worktree {
        let rm_out = Command::new("git")
            .args(["-C", &repo_root, "worktree", "remove", worktree])
            .output()
            .await;
        worktree_removed = rm_out.map(|o| o.status.success()).unwrap_or(false);
    }

    serde_json::to_string(&json!({
        "merged": true,
        "branch": branch,
        "repo_root": repo_root,
        "worktree_removed": worktree_removed,
        "merge_output": merge_stdout.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
