use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use super::types::GitChangedFile;

pub(super) struct GitDiffPlan {
    pub(super) changed_files: Vec<GitChangedFile>,
    pub(super) patches: BTreeMap<String, String>,
}

pub(super) fn git_latest_sha(repo_url: &str, ref_name: &str) -> Result<String, String> {
    let candidates = if ref_name.starts_with("refs/") {
        vec![ref_name.to_string()]
    } else {
        vec![
            format!("refs/heads/{ref_name}"),
            format!("refs/tags/{ref_name}"),
            ref_name.to_string(),
        ]
    };

    for candidate in candidates {
        let output = run_git(None, &["ls-remote", repo_url, &candidate])?;
        if let Some(sha) = output
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .find(|value| !value.is_empty())
        {
            return Ok(sha.to_string());
        }
    }

    Err(format!(
        "git ls-remote found no ref '{ref_name}' in {repo_url}"
    ))
}

pub(super) fn git_changed_files_and_patches(
    repo_url: &str,
    pinned_sha: &str,
    latest_sha: &str,
    tracked_paths: &BTreeSet<String>,
) -> Result<GitDiffPlan, String> {
    let root = create_temp_root("tachi-skill-source-sync")?;
    let result = (|| {
        let repo_dir = root.join("repo");
        let repo_dir_arg = repo_dir.display().to_string();
        run_git(
            None,
            &[
                "clone",
                "--quiet",
                "--filter=blob:none",
                "--no-checkout",
                repo_url,
                &repo_dir_arg,
            ],
        )?;
        run_git(Some(&repo_dir), &["fetch", "--quiet", "origin", pinned_sha])?;
        run_git(Some(&repo_dir), &["fetch", "--quiet", "origin", latest_sha])?;

        let mut diff_args = vec![
            "diff".to_string(),
            "--name-status".to_string(),
            pinned_sha.to_string(),
            latest_sha.to_string(),
            "--".to_string(),
        ];
        diff_args.extend(tracked_paths.iter().cloned());
        let diff_arg_refs = diff_args.iter().map(String::as_str).collect::<Vec<_>>();
        let changed_files = parse_name_status(&run_git(Some(&repo_dir), &diff_arg_refs)?);

        let mut patches = BTreeMap::new();
        for file in &changed_files {
            let patch_args = [
                "diff",
                "--unified=0",
                pinned_sha,
                latest_sha,
                "--",
                file.path.as_str(),
            ];
            let patch = run_git(Some(&repo_dir), &patch_args)?;
            patches.insert(file.path.clone(), patch);
        }

        Ok(GitDiffPlan {
            changed_files,
            patches,
        })
    })();
    if let Err(error) = fs::remove_dir_all(&root) {
        tracing::warn!(error = %error, path = %root.display(), "failed to cleanup temp sync-plan git root");
    }
    result
}

fn parse_name_status(output: &str) -> Vec<GitChangedFile> {
    output
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            let status = parts.first()?.trim();
            if status.is_empty() {
                return None;
            }
            let path = if status.starts_with('R') || status.starts_with('C') {
                parts.last()?
            } else {
                parts.get(1)?
            };
            Some(GitChangedFile {
                path: (*path).to_string(),
                status: status.to_string(),
            })
        })
        .collect()
}

fn run_git(cwd: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = ProcessCommand::new("git");
    if let Some(cwd) = cwd {
        command.arg("-C").arg(cwd);
    }
    command.args(args);
    let output = command
        .output()
        .map_err(|e| format!("run git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "git {} failed{}",
            args.join(" "),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) fn upstream_repo_url(repo: &str) -> String {
    if repo.contains("://") || repo.starts_with("git@") {
        repo.to_string()
    } else {
        format!("https://github.com/{repo}.git")
    }
}

fn create_temp_root(prefix: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system time before UNIX_EPOCH: {e}"))?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).map_err(|e| format!("create temp dir {}: {e}", root.display()))?;
    Ok(root)
}
