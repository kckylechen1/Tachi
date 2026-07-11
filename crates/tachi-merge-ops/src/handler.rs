use super::cleaner::remove_worktree_with_cleaner;
use super::preview::preview_merge_without_touching_worktree;
use super::repo::{
    current_worktree_branch, ensure_repo_root_is_clean, resolve_repo_root_from_worktree,
    resolve_worktree_top_level,
};
use super::safety::{
    detect_branch_safety_signals, validate_static_merge_safety, worktree_equals_repo_root,
};
use serde_json::json;
use tachi_params::TachiApproveMergeParams;
use tokio::process::Command;

pub async fn handle_approve_merge(params: TachiApproveMergeParams) -> Result<String, String> {
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
    let worktree_head = current_worktree_branch(&worktree, "worktree HEAD branch").await?;
    if branch != worktree_head {
        return Err(format!(
            "Refusing to merge branch '{branch}' from worktree '{worktree}' because the worktree HEAD is '{worktree_head}'. Pass the worktree's current branch or check out the intended branch first."
        ));
    }
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
        let preview = preview_merge_without_touching_worktree(&repo_root, &branch).await?;

        if !preview.can_merge {
            return serde_json::to_string(&json!({
                "preview": true,
                "can_merge": false,
                "branch": branch,
                "error": preview.error.unwrap_or_else(|| "merge preview failed".to_string()),
                "repo_root": repo_root,
                "preview_engine": "git merge-tree --write-tree",
                "requested_strategy": strategy,
                "preview_strategy": "git merge-tree default",
                "safety_warnings": safety_warnings,
                "requires_human_review": requires_human_review,
            }))
            .map_err(|e| format!("serialize: {e}"));
        }

        return serde_json::to_string(&json!({
            "preview": true,
            "can_merge": true,
            "branch": branch,
            "repo_root": repo_root,
            "merge_output": preview.merge_output,
            "diff_stat": preview.diff_stat.unwrap_or_default(),
            "preview_engine": "git merge-tree --write-tree",
            "requested_strategy": strategy,
            "preview_strategy": "git merge-tree default",
            "safety_warnings": safety_warnings,
            "requires_human_review": requires_human_review,
            "delete_worktree_requested": params.delete_worktree,
            "delete_worktree_allowed": safety.allow_delete_worktree,
            "next_step": "Call approve_merge again with confirm=true to execute the merge.",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let merge_out = Command::new("git")
        .args([
            "-C",
            &repo_root,
            "merge",
            "--strategy",
            strategy,
            "--",
            &branch,
        ])
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
    let mut cleanup_error = None::<String>;
    let mut cleanup_warnings = Vec::<String>::new();
    if params.delete_worktree {
        if worktree_equals_repo_root(&worktree, &repo_root) {
            return Err(format!(
                "Refusing to delete worktree '{worktree}' because it resolves to repository root/main worktree '{repo_root}'. Clean up manually if needed."
            ));
        }
        if safety.allow_delete_worktree {
            match remove_worktree_with_cleaner(&worktree).await {
                Ok(report) => {
                    worktree_removed = report.removed;
                    cleanup_warnings = report.warnings;
                    if !report.errors.is_empty() {
                        cleanup_error = Some(report.errors.join("; "));
                    }
                }
                Err(err) => {
                    cleanup_error = Some(err);
                }
            }
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
        "cleanup_warnings": cleanup_warnings,
        "cleanup_error": cleanup_error,
        "delete_worktree_requested": params.delete_worktree,
        "delete_worktree_allowed": safety.allow_delete_worktree,
        "safety_warnings": safety_warnings,
        "requires_human_review": requires_human_review,
        "merge_output": merge_stdout.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
