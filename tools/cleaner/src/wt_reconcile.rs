use std::path::Path;
use std::process::Command;

use crate::registry::{self, ListedWorktree};
use crate::wt_clean::{self, OutputFormat};

#[derive(Debug, Clone, Copy)]
pub struct WtReconcileOptions {
    pub force: bool,
    pub output: OutputFormat,
}

#[derive(Debug, serde::Serialize)]
pub struct WtReconcileReport {
    pub action: &'static str,
    pub dry_run: bool,
    pub reconciled: Vec<ReconciledWorktree>,
    pub refused: Vec<RefusedWorktree>,
    pub skipped: Vec<SkippedWorktree>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ReconciledWorktree {
    pub path: String,
    pub branch: String,
    pub repo_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    pub removed: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct RefusedWorktree {
    pub path: String,
    pub branch: String,
    pub repo_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    pub reasons: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SkippedWorktree {
    pub path: String,
    pub branch: String,
    pub repo_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchPrState {
    TerminalMerged {
        pr_number: Option<String>,
        head_oid: Option<String>,
        head_name: Option<String>,
    },
    TerminalClosed {
        pr_number: Option<String>,
    },
    Open {
        pr_number: Option<String>,
    },
    NotFound,
    Error(String),
}

pub fn check_branch_pr_state(
    repo_root: &str,
    branch: &str,
    registered_pr: Option<&str>,
) -> BranchPrState {
    let clean_branch = branch.trim_start_matches("refs/heads/");
    if clean_branch.is_empty()
        || clean_branch == "HEAD"
        || clean_branch == "main"
        || clean_branch == "master"
        || clean_branch == "trunk"
    {
        return BranchPrState::NotFound;
    }

    if let Some(pr_str) = registered_pr.filter(|s| !s.trim().is_empty()) {
        let clean_num = pr_str.trim_start_matches('#');
        return match Command::new("gh")
            .args([
                "pr",
                "view",
                clean_num,
                "--json",
                "state,headRefOid,headRefName",
            ])
            .current_dir(repo_root)
            .output()
        {
            Ok(out) if out.status.success() => {
                match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    Ok(value) => parsed_pr_state(&value, Some(pr_str.to_string())),
                    Err(_) => BranchPrState::Error("pr view returned malformed JSON".to_string()),
                }
            }
            Ok(_) => BranchPrState::Error("pr view command failed".to_string()),
            Err(_) => BranchPrState::Error("pr view command unavailable".to_string()),
        };
    }

    match Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            clean_branch,
            "--state",
            "all",
            "--json",
            "number,state,headRefOid,headRefName",
            "--limit",
            "1",
        ])
        .current_dir(repo_root)
        .output()
    {
        Ok(out) if out.status.success() => {
            let value = match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                Ok(value) => value,
                Err(_) => {
                    return BranchPrState::Error("pr list returned malformed JSON".to_string())
                }
            };
            let Some(prs) = value.as_array() else {
                return BranchPrState::Error("pr list returned a non-array".to_string());
            };
            match prs.first() {
                Some(first) => parsed_pr_state(first, first.get("number").map(|n| n.to_string())),
                None => BranchPrState::NotFound,
            }
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            BranchPrState::Error(err)
        }
        Err(err) => BranchPrState::Error(err.to_string()),
    }
}

fn parsed_pr_state(value: &serde_json::Value, pr_number: Option<String>) -> BranchPrState {
    if !value.is_object() {
        return BranchPrState::Error("PR lookup returned a non-object entry".to_string());
    }
    match value.get("state").and_then(|state| state.as_str()) {
        Some("MERGED") => merged_pr_state(value, pr_number),
        Some("CLOSED") => BranchPrState::TerminalClosed { pr_number },
        Some("OPEN") => BranchPrState::Open { pr_number },
        _ => BranchPrState::Error("PR lookup returned a missing or invalid state".to_string()),
    }
}

fn merged_pr_state(value: &serde_json::Value, pr_number: Option<String>) -> BranchPrState {
    BranchPrState::TerminalMerged {
        pr_number,
        head_oid: value
            .get("headRefOid")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        head_name: value
            .get("headRefName")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    }
}

// Terminal state nominates a candidate; it cannot authorize discarding later commits.
fn verify_merged_worktree_head(
    worktree: &ListedWorktree,
    head_oid: Option<&str>,
    head_name: Option<&str>,
) -> Result<(), &'static str> {
    let head = head_oid
        .filter(|head| !head.is_empty())
        .ok_or("merged_pr_head_missing")?;
    if !matches!(head.len(), 40 | 64)
        || !head.bytes().all(|byte| byte.is_ascii_hexdigit())
        || head.bytes().all(|byte| byte == b'0')
    {
        return Err("merged_pr_head_invalid");
    }
    let branch = head_name
        .map(|branch| branch.trim_start_matches("refs/heads/"))
        .filter(|branch| !branch.is_empty())
        .ok_or("merged_pr_branch_missing")?;
    let read = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .arg("-C")
            .arg(&worktree.path)
            .args(args)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
    };
    if read(&["check-ref-format", &format!("refs/heads/{branch}")]).is_none() {
        return Err("merged_pr_branch_invalid");
    }
    if worktree.branch.trim_start_matches("refs/heads/") != branch {
        return Err("registered_branch_mismatch");
    }
    let actual_head = read(&["rev-parse", "--verify", "HEAD^{commit}"])
        .filter(|head| !head.is_empty())
        .ok_or("worktree_head_unavailable")?;
    let actual_branch = read(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .filter(|branch| !branch.is_empty())
        .ok_or("worktree_branch_unavailable")?;
    if actual_head != head {
        return Err("worktree_head_mismatch");
    }
    if actual_branch != branch {
        return Err("worktree_branch_mismatch");
    }
    Ok(())
}

pub fn run_wt_reconcile(options: WtReconcileOptions) -> Result<(), String> {
    let worktrees = registry::list_registered_worktrees()?;
    let report = reconcile_worktrees(&worktrees, options.force, &check_branch_pr_state);

    emit_reconcile_report(&report, options.output)?;
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("; "))
    }
}

pub(crate) fn reconcile_worktrees<F>(
    worktrees: &[ListedWorktree],
    force: bool,
    pr_checker: &F,
) -> WtReconcileReport
where
    F: Fn(&str, &str, Option<&str>) -> BranchPrState,
{
    let mut report = WtReconcileReport {
        action: "wt-reconcile",
        dry_run: !force,
        reconciled: Vec::new(),
        refused: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    for wt in worktrees {
        let pr_status = pr_checker(&wt.repo_root, &wt.branch, wt.pr.as_deref());
        match pr_status {
            BranchPrState::TerminalClosed { pr_number } => {
                report.refused.push(RefusedWorktree {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    repo_root: wt.repo_root.clone(),
                    pr: pr_number.or_else(|| wt.pr.clone()),
                    reasons: vec!["closed_pr_reclaimability_unproven".to_string()],
                });
            }
            BranchPrState::TerminalMerged {
                pr_number,
                head_oid,
                head_name,
            } => {
                let pr_id = pr_number.or_else(|| wt.pr.clone());
                if let Err(reason) =
                    verify_merged_worktree_head(wt, head_oid.as_deref(), head_name.as_deref())
                {
                    report.refused.push(RefusedWorktree {
                        path: wt.path.clone(),
                        branch: wt.branch.clone(),
                        repo_root: wt.repo_root.clone(),
                        pr: pr_id,
                        reasons: vec![reason.to_string()],
                    });
                    continue;
                }
                let plan = wt_clean::plan_wt_remove_default(Path::new(&wt.path), !force);
                if plan.allowed {
                    if force {
                        let exec_report = wt_clean::execute_wt_remove_planned(plan);
                        report.warnings.extend(exec_report.warnings);
                        if exec_report.removed {
                            report.reconciled.push(ReconciledWorktree {
                                path: wt.path.clone(),
                                branch: wt.branch.clone(),
                                repo_root: wt.repo_root.clone(),
                                pr: pr_id,
                                removed: true,
                            });
                        } else {
                            report.refused.push(RefusedWorktree {
                                path: wt.path.clone(),
                                branch: wt.branch.clone(),
                                repo_root: wt.repo_root.clone(),
                                pr: pr_id,
                                reasons: exec_report.errors,
                            });
                        }
                    } else {
                        report.reconciled.push(ReconciledWorktree {
                            path: wt.path.clone(),
                            branch: wt.branch.clone(),
                            repo_root: wt.repo_root.clone(),
                            pr: pr_id,
                            removed: false,
                        });
                    }
                } else {
                    report.refused.push(RefusedWorktree {
                        path: wt.path.clone(),
                        branch: wt.branch.clone(),
                        repo_root: wt.repo_root.clone(),
                        pr: pr_id,
                        reasons: plan.errors,
                    });
                }
            }
            BranchPrState::Open { pr_number } => {
                report.skipped.push(SkippedWorktree {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    repo_root: wt.repo_root.clone(),
                    pr: pr_number.or_else(|| wt.pr.clone()),
                    reason: "PR is still open".to_string(),
                });
            }
            BranchPrState::NotFound => {
                report.skipped.push(SkippedWorktree {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    repo_root: wt.repo_root.clone(),
                    pr: wt.pr.clone(),
                    reason: "no PR found for branch".to_string(),
                });
            }
            BranchPrState::Error(err) => {
                report.skipped.push(SkippedWorktree {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    repo_root: wt.repo_root.clone(),
                    pr: wt.pr.clone(),
                    reason: format!("could not inspect PR status: {err}"),
                });
            }
        }
    }

    report
}

fn emit_reconcile_report(report: &WtReconcileReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(report)
                    .map_err(|err| format!("serialize report: {err}"))?
            );
        }
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "force" };
            println!("tachi-clean wt-reconcile ({mode})");
            println!("  reconciled: {}", report.reconciled.len());
            for item in &report.reconciled {
                let status = if item.removed { "removed" } else { "candidate" };
                println!(
                    "    [{status}] {}  branch={}  repo={}",
                    item.path, item.branch, item.repo_root
                );
            }
            println!("  refused: {}", report.refused.len());
            for item in &report.refused {
                println!(
                    "    [refused] {}  branch={}  reasons={}",
                    item.path,
                    item.branch,
                    item.reasons.join("; ")
                );
            }
            println!("  skipped: {}", report.skipped.len());
            for item in &report.skipped {
                println!(
                    "    [skipped] {}  branch={}  reason={}",
                    item.path, item.branch, item.reason
                );
            }
            for warning in &report.warnings {
                println!("  warning: {warning}");
            }
            for error in &report.errors {
                println!("  error: {error}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_filters_open_and_processes_terminal() {
        let worktrees = vec![
            ListedWorktree {
                path: "/tmp/wt-open".to_string(),
                repo_root: "/tmp/repo".to_string(),
                branch: "feat/open".to_string(),
                dispatch_id: None,
                pr: Some("1".to_string()),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                path_exists: false,
            },
            ListedWorktree {
                path: "/tmp/wt-merged".to_string(),
                repo_root: "/tmp/repo".to_string(),
                branch: "feat/merged".to_string(),
                dispatch_id: None,
                pr: Some("2".to_string()),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                path_exists: false,
            },
            ListedWorktree {
                path: "/tmp/wt-notfound".to_string(),
                repo_root: "/tmp/repo".to_string(),
                branch: "feat/notfound".to_string(),
                dispatch_id: None,
                pr: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                path_exists: false,
            },
        ];

        let fake_checker = |_repo: &str, branch: &str, _pr: Option<&str>| match branch {
            "feat/open" => BranchPrState::Open {
                pr_number: Some("1".to_string()),
            },
            "feat/merged" => BranchPrState::TerminalMerged {
                pr_number: Some("2".to_string()),
                head_oid: None,
                head_name: None,
            },
            _ => BranchPrState::NotFound,
        };

        let report = reconcile_worktrees(&worktrees, false, &fake_checker);
        assert_eq!(report.skipped.len(), 2);
        assert!(report.skipped.iter().any(|s| s.branch == "feat/open"));
        assert!(report.skipped.iter().any(|s| s.branch == "feat/notfound"));

        assert_eq!(report.reconciled.len() + report.refused.len(), 1);
    }
}
