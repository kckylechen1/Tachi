//! Task lifecycle bindings for GitHub issue/PR backed Tachi flows.
//!
//! This keeps GitHub collaboration state inside existing flow artifacts:
//! `.tachi/runs/<flow_id>/status.json`, `instruction.md`, and `events.jsonl`.

use crate::tool_params::{TachiGhParams, TachiOrchestratorParams, TachiTaskParams};
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const UX_CLOSURE_STATES: &[&str] = &["closed_loop", "closed", "shipped"];
static FLOW_MARKER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubTarget {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct IssueSnapshot {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub state: Option<String>,
    pub url: String,
    pub doc_paths: Vec<String>,
    pub spec_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PrSnapshot {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: Option<String>,
    pub url: String,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub review_decision: Option<String>,
    pub mergeable: Option<String>,
}

pub(crate) fn parse_issue_ref(raw: &str, default_repo: Option<&str>) -> Option<GithubTarget> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        let parts = rest.split('/').collect::<Vec<_>>();
        if parts.len() == 4 && parts[2] == "issues" {
            return Some(GithubTarget {
                repo: format!("{}/{}", parts[0], parts[1]),
                number: parts[3].parse::<u64>().ok()?,
            });
        }
        return None;
    }
    if let Some(number) = trimmed.strip_prefix('#') {
        let repo = default_repo?.trim();
        if repo.matches('/').count() != 1 {
            return None;
        }
        return Some(GithubTarget {
            repo: repo.to_string(),
            number: number.parse::<u64>().ok()?,
        });
    }
    let (repo, number) = trimmed.rsplit_once('#')?;
    if repo.matches('/').count() != 1 {
        return None;
    }
    Some(GithubTarget {
        repo: repo.to_string(),
        number: number.parse::<u64>().ok()?,
    })
}

pub(crate) fn parse_pr_ref(raw: &str) -> Option<GithubTarget> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        let parts = rest.split('/').collect::<Vec<_>>();
        if parts.len() == 4 && parts[2] == "pull" {
            return Some(GithubTarget {
                repo: format!("{}/{}", parts[0], parts[1]),
                number: parts[3].parse::<u64>().ok()?,
            });
        }
        return None;
    }
    let (repo, number) = trimmed.rsplit_once('#')?;
    if repo.matches('/').count() != 1 {
        return None;
    }
    Some(GithubTarget {
        repo: repo.to_string(),
        number: number.parse::<u64>().ok()?,
    })
}

pub(crate) fn resolve_task_issue_target(params: &TachiTaskParams) -> Result<GithubTarget, String> {
    if let (Some(repo), Some(number)) = (
        params
            .repo
            .as_deref()
            .filter(|repo| !repo.trim().is_empty()),
        params.number,
    ) {
        return Ok(GithubTarget {
            repo: repo.trim().to_string(),
            number,
        });
    }
    if let Some(issue_ref) = params.issue_ref.as_deref() {
        if let Some(target) = parse_issue_ref(issue_ref, params.repo.as_deref()) {
            return Ok(target);
        }
    }
    Err(
        "intake requires either repo+number or issue_ref='owner/repo#123' / GitHub issue URL"
            .to_string(),
    )
}

pub(crate) fn resolve_task_pr_target(params: &TachiTaskParams) -> Result<GithubTarget, String> {
    if let (Some(repo), Some(number)) = (
        params
            .repo
            .as_deref()
            .filter(|repo| !repo.trim().is_empty()),
        params.number,
    ) {
        return Ok(GithubTarget {
            repo: repo.trim().to_string(),
            number,
        });
    }
    if let Some(pr_ref) = params.pr_ref.as_deref() {
        if let Some(target) = parse_pr_ref(pr_ref) {
            return Ok(target);
        }
    }
    Err(
        "link_pr/pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
            .to_string(),
    )
}

mod cycle_plan;
mod cycle_status;
mod flow_artifacts;
mod github_flow_state;
mod github_io;
mod issue_flow;
pub(crate) mod release_ux;
pub(crate) mod utils;

#[cfg(test)]
mod tests;

use self::flow_artifacts::*;
pub(crate) use self::github_flow_state::*;
use self::github_io::*;
use self::issue_flow::*;
use self::release_ux::*;
use self::utils::*;

pub(crate) use self::cycle_plan::handle_task_cycle_plan;
pub(crate) use self::cycle_status::handle_task_cycle_status;
pub(crate) use self::flow_artifacts::{
    flow_status_doc_refs, mark_task_close_loop, mark_task_dispatch, mark_task_dispatch_completion,
    resolve_link_pr_issue_ref, run_dir_for_flow_id, scan_open_loops, shell_runs_root,
    validate_flow_id, write_intake_flow_artifacts, write_link_pr_artifacts,
};
#[cfg(test)]
pub(crate) use self::issue_flow::build_issue_automation_plan;
pub(crate) use self::issue_flow::{
    handle_task_intake, handle_task_link_pr, handle_task_pr_handoff,
};
pub(crate) use self::release_ux::{handle_task_release_note, handle_task_ux_matrix};
pub(crate) use self::utils::read_json_file;
