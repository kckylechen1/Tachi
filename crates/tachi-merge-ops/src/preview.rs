use tokio::process::Command;

pub(crate) struct MergePreviewResult {
    pub can_merge: bool,
    pub merge_output: String,
    pub error: Option<String>,
    pub diff_stat: Option<String>,
}

pub(crate) async fn preview_merge_without_touching_worktree(
    repo_root: &str,
    branch: &str,
) -> Result<MergePreviewResult, String> {
    let merge_out = Command::new("git")
        .args([
            "-C",
            repo_root,
            "merge-tree",
            "--write-tree",
            "HEAD",
            branch,
        ])
        .output()
        .await
        .map_err(|e| format!("Merge preview failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&merge_out.stdout)
        .trim()
        .to_string();
    let stderr = String::from_utf8_lossy(&merge_out.stderr)
        .trim()
        .to_string();

    if !merge_out.status.success() {
        let error = if stderr.is_empty() {
            stdout.clone()
        } else {
            stderr
        };
        return Ok(MergePreviewResult {
            can_merge: false,
            merge_output: stdout,
            error: Some(error),
            diff_stat: None,
        });
    }

    let tree = stdout.lines().next().unwrap_or("").trim();
    let diff_stat = if tree.is_empty() {
        None
    } else {
        let diff_out = Command::new("git")
            .args(["-C", repo_root, "diff", "--stat", "HEAD", tree])
            .output()
            .await
            .map_err(|e| format!("Merge preview diff failed: {e}"))?;
        Some(String::from_utf8_lossy(&diff_out.stdout).trim().to_string())
    };

    Ok(MergePreviewResult {
        can_merge: true,
        merge_output: stdout,
        error: None,
        diff_stat,
    })
}
