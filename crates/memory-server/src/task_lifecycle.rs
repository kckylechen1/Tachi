//! Task lifecycle bindings for GitHub issue/PR backed Tachi flows.
//!
//! This keeps GitHub collaboration state inside existing flow artifacts:
//! `.tachi/runs/<flow_id>/status.json`, `instruction.md`, and `events.jsonl`.

use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::{TachiGhParams, TachiOrchestratorParams, TachiTaskParams};
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

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

pub(crate) async fn handle_task_intake(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let target = resolve_task_issue_target(params)?;
    let issue = read_issue_snapshot(server, &target, params).await?;
    let objective = params
        .task
        .clone()
        .filter(|task| !task.trim().is_empty())
        .unwrap_or_else(|| issue.title.clone());
    let flow_id = params
        .flow_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| new_task_flow_id("intake", &objective));
    write_intake_flow_artifacts(&flow_id, &objective, &issue)?;
    seed_intake_orchestrator(server, &flow_id, &objective, &issue).await?;
    let briefing_params = intake_briefing_params(params, &flow_id, &objective, &issue);
    let briefing =
        crate::copilot_ops::handle_tachi_feature_briefing(server, &briefing_params).await?;
    serde_json::to_string(&json!({
        "ok": true,
        "action": "intake",
        "flow_id": flow_id,
        "issue_ref": format!("{}#{}", issue.repo, issue.number),
        "issue": issue_to_json(&issue),
        "doc_paths": issue.doc_paths,
        "spec_paths": issue.spec_paths,
        "run_dir": run_dir_for_flow_id(&flow_id)?.to_string_lossy(),
        "briefing": serde_json::from_str::<Value>(&briefing).unwrap_or(json!(briefing)),
    }))
    .map_err(|e| format!("serialize intake: {e}"))
}

pub(crate) async fn handle_task_link_pr(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "flow_id is required for link_pr".to_string())?;
    let target = resolve_task_pr_target(params)?;
    let pr = read_pr_snapshot(server, &target).await?;
    let issue_ref = resolve_link_pr_issue_ref(flow_id, params.issue_ref.as_deref())?;
    write_link_pr_artifacts(flow_id, &pr, issue_ref.as_deref())?;
    serde_json::to_string(&json!({
        "ok": true,
        "action": "link_pr",
        "flow_id": flow_id,
        "pr_ref": format!("{}#{}", pr.repo, pr.number),
        "issue_ref": issue_ref,
        "pr": pr_to_json(&pr),
        "run_dir": run_dir_for_flow_id(flow_id)?.to_string_lossy(),
    }))
    .map_err(|e| format!("serialize link_pr: {e}"))
}

pub(crate) fn write_intake_flow_artifacts(
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create intake run dir: {e}"))?;
    let now = Utc::now().to_rfc3339();
    merge_flow_status(
        &run_dir,
        json!({
            "flow_id": flow_id,
            "stage": "intake",
            "state": "flow_bound",
            "task": objective,
            "issue_ref": format!("{}#{}", issue.repo, issue.number),
            "doc_paths": issue.doc_paths,
            "spec_paths": issue.spec_paths,
            "dispatch_ids": [],
            "updated_at": now,
        }),
    )?;
    merge_github_status(
        &run_dir,
        json!({
            "repo": issue.repo,
            "issue_number": issue.number,
            "issue_url": issue.url,
            "issue_ref": format!("{}#{}", issue.repo, issue.number),
            "issue_title": issue.title,
            "issue_state": issue.state,
            "doc_paths": issue.doc_paths,
            "spec_paths": issue.spec_paths,
        }),
    )?;
    write_intake_instruction(&run_dir, flow_id, objective, issue)?;
    append_github_event(
        &run_dir,
        flow_id,
        "github_issue_linked",
        json!({
            "repo": issue.repo,
            "issue_number": issue.number,
            "issue_url": issue.url,
            "title": issue.title,
            "state": issue.state,
            "doc_paths": issue.doc_paths,
            "spec_paths": issue.spec_paths,
        }),
    )?;
    Ok(())
}

pub(crate) fn write_link_pr_artifacts(
    flow_id: &str,
    pr: &PrSnapshot,
    issue_ref: Option<&str>,
) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create link_pr run dir: {e}"))?;
    let patch = json!({
        "repo": pr.repo,
        "pr_number": pr.number,
        "pr_url": pr.url,
        "pr_ref": format!("{}#{}", pr.repo, pr.number),
        "pr_title": pr.title,
        "pr_state": pr.state,
        "head_ref": pr.head_ref,
        "base_ref": pr.base_ref,
        "review": { "state": pr.review_decision, "updated_at": Utc::now().to_rfc3339() },
        "mergeable": pr.mergeable,
        "merge_state": initial_merge_state_for_pr(pr.state.as_deref()),
    });
    merge_github_status(&run_dir, patch.clone())?;
    let mut flow_patch = json!({
        "flow_id": flow_id,
        "stage": "review",
        "state": "pr_linked",
        "pr_ref": format!("{}#{}", pr.repo, pr.number),
        "updated_at": Utc::now().to_rfc3339(),
    });
    if let (Some(obj), Some(issue_ref)) = (flow_patch.as_object_mut(), issue_ref) {
        obj.insert("issue_ref".to_string(), json!(issue_ref));
    }
    merge_flow_status(&run_dir, flow_patch)?;
    append_github_event(
        &run_dir,
        flow_id,
        "github_pr_updated",
        json!({
            "repo": pr.repo,
            "pr_number": pr.number,
            "pr_url": pr.url,
            "issue_ref": issue_ref,
            "state": pr.state,
            "review_decision": pr.review_decision,
            "mergeable": pr.mergeable,
        }),
    )?;
    Ok(())
}

pub(crate) fn flow_status_doc_refs(
    flow_id: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let status = read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({}));
    let mut docs = string_array_field(&status, "doc_paths");
    let mut specs = string_array_field(&status, "spec_paths");
    if let Some(github) = status.get("github") {
        docs.extend(string_array_field(github, "doc_paths"));
        specs.extend(string_array_field(github, "spec_paths"));
    }
    dedupe_strings(&mut docs);
    dedupe_strings(&mut specs);
    Ok((docs, specs))
}

fn intake_briefing_params(
    params: &TachiTaskParams,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
) -> TachiTaskParams {
    let mut briefing = params.clone();
    briefing.action = "briefing".to_string();
    briefing.format = Some("json".to_string());
    briefing.flow_id = Some(flow_id.to_string());
    briefing.issue_ref = Some(format!("{}#{}", issue.repo, issue.number));
    briefing.task = Some(objective.to_string());
    briefing.doc_paths = issue.doc_paths.clone();
    briefing.spec_paths = issue.spec_paths.clone();
    briefing
}

pub(crate) fn resolve_link_pr_issue_ref(
    flow_id: &str,
    supplied: Option<&str>,
) -> Result<Option<String>, String> {
    let existing = existing_flow_issue_ref(flow_id)?;
    let supplied = supplied
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_issue_ref)
        .transpose()?;
    if let (Some(existing), Some(supplied)) = (existing.as_deref(), supplied.as_deref()) {
        if existing != supplied {
            return Err(format!(
                "link_pr issue_ref mismatch: flow has '{existing}', request supplied '{supplied}'"
            ));
        }
    }
    Ok(supplied.or(existing))
}

fn existing_flow_issue_ref(flow_id: &str) -> Result<Option<String>, String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let status = read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({}));
    let from_top = status
        .get("issue_ref")
        .and_then(Value::as_str)
        .map(str::to_string);
    if from_top.is_some() {
        return Ok(from_top);
    }
    Ok(status
        .get("github")
        .and_then(|github| github.get("issue_ref"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

fn normalize_issue_ref(raw: &str) -> Result<String, String> {
    parse_issue_ref(raw, None)
        .map(|target| format!("{}#{}", target.repo, target.number))
        .ok_or_else(|| {
            "issue_ref must be owner/repo#123 or a GitHub issue URL for link_pr".to_string()
        })
}

fn initial_merge_state_for_pr(state: Option<&str>) -> &'static str {
    match state.unwrap_or("").to_ascii_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "blocked",
        _ => "pending",
    }
}

async fn read_issue_snapshot(
    server: &MemoryServer,
    target: &GithubTarget,
    params: &TachiTaskParams,
) -> Result<IssueSnapshot, String> {
    let raw = crate::gh_ops::handle_tachi_gh(
        server,
        TachiGhParams {
            action: "issue_read".to_string(),
            repo: target.repo.clone(),
            number: Some(target.number),
            title: None,
            body: None,
            labels: Vec::new(),
            state: None,
            limit: None,
            merge_strategy: None,
            dry_run: Some(true),
            confirm: false,
            flow_id: None,
            merge_policy: None,
            author_filter: None,
            write_digest: None,
        },
    )
    .await?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("parse issue_read: {e}"))?;
    let result = value.get("result").cloned().unwrap_or_else(|| json!({}));
    let title = result
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("GitHub issue")
        .to_string();
    let body = result
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_string);
    let comments = result
        .get("comments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("body").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut doc_paths = params.doc_paths.clone();
    doc_paths.extend(extract_markdown_paths(body.as_deref().unwrap_or("")));
    for comment in &comments {
        doc_paths.extend(extract_markdown_paths(comment));
    }
    let mut spec_paths = params.spec_paths.clone();
    spec_paths.extend(
        doc_paths
            .iter()
            .filter(|path| path.to_ascii_lowercase().contains("spec"))
            .cloned(),
    );
    dedupe_strings(&mut doc_paths);
    dedupe_strings(&mut spec_paths);
    Ok(IssueSnapshot {
        repo: target.repo.clone(),
        number: target.number,
        title,
        state: result
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: result
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "https://github.com/{}/issues/{}",
                    target.repo, target.number
                )
            }),
        doc_paths,
        spec_paths,
    })
}

async fn read_pr_snapshot(
    server: &MemoryServer,
    target: &GithubTarget,
) -> Result<PrSnapshot, String> {
    let raw = crate::gh_ops::handle_tachi_gh(
        server,
        TachiGhParams {
            action: "pr_read".to_string(),
            repo: target.repo.clone(),
            number: Some(target.number),
            title: None,
            body: None,
            labels: Vec::new(),
            state: None,
            limit: None,
            merge_strategy: None,
            dry_run: Some(true),
            confirm: false,
            flow_id: None,
            merge_policy: None,
            author_filter: None,
            write_digest: None,
        },
    )
    .await?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("parse pr_read: {e}"))?;
    let result = value.get("result").cloned().unwrap_or_else(|| json!({}));
    Ok(PrSnapshot {
        repo: target.repo.clone(),
        number: target.number,
        title: result
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("GitHub PR")
            .to_string(),
        state: result
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: result
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("https://github.com/{}/pull/{}", target.repo, target.number)
            }),
        head_ref: result
            .get("headRefName")
            .and_then(Value::as_str)
            .map(str::to_string),
        base_ref: result
            .get("baseRefName")
            .and_then(Value::as_str)
            .map(str::to_string),
        review_decision: result
            .get("reviewDecision")
            .and_then(Value::as_str)
            .map(str::to_string),
        mergeable: result
            .get("mergeable")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

async fn seed_intake_orchestrator(
    server: &MemoryServer,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
) -> Result<(), String> {
    let issue_ref = format!("{}#{}", issue.repo, issue.number);
    let refs = std::iter::once(issue_ref.clone())
        .chain(issue.doc_paths.iter().cloned())
        .chain(issue.spec_paths.iter().cloned())
        .collect::<Vec<_>>();
    let todo_status = if issue.doc_paths.is_empty() && issue.spec_paths.is_empty() {
        "pending"
    } else {
        "done"
    };
    let _ = crate::orchestrator_ops::handle_orchestrator(
        server,
        TachiOrchestratorParams {
            action: "todo_update".to_string(),
            task_id: Some(flow_id.to_string()),
            todo_id: Some("canonical-docs".to_string()),
            todo_content: Some(
                "Attach or confirm canonical docs/specs for this feature.".to_string(),
            ),
            todo_status: Some(todo_status.to_string()),
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref.clone()),
            blocked_reason: None,
            verification: if todo_status == "done" {
                Some("Issue intake found canonical doc/spec references.".to_string())
            } else {
                None
            },
            references: refs.clone(),
            objective: None,
            current_state: None,
            completed_steps: Vec::new(),
            remaining_steps: Vec::new(),
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: Vec::new(),
            next_action: None,
            newest_user_instruction: None,
        },
    )
    .await?;
    let _ = crate::orchestrator_ops::handle_orchestrator(
        server,
        TachiOrchestratorParams {
            action: "todo_update".to_string(),
            task_id: Some(flow_id.to_string()),
            todo_id: Some("dispatch-or-plan".to_string()),
            todo_content: Some(
                "Run briefing/recommend, then dispatch a bounded implementation or review slice."
                    .to_string(),
            ),
            todo_status: Some("pending".to_string()),
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref.clone()),
            blocked_reason: None,
            verification: None,
            references: refs.clone(),
            objective: None,
            current_state: None,
            completed_steps: Vec::new(),
            remaining_steps: Vec::new(),
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: Vec::new(),
            next_action: None,
            newest_user_instruction: None,
        },
    )
    .await?;
    let _ = crate::orchestrator_ops::handle_orchestrator(
        server,
        TachiOrchestratorParams {
            action: "handoff_write".to_string(),
            task_id: Some(flow_id.to_string()),
            todo_id: None,
            todo_content: None,
            todo_status: None,
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref),
            blocked_reason: None,
            verification: None,
            references: refs,
            objective: Some(objective.to_string()),
            current_state: Some("GitHub issue has been bound to a Tachi flow.".to_string()),
            completed_steps: vec!["Read issue and wrote intake flow artifacts.".to_string()],
            remaining_steps: vec![
                "Confirm canonical docs/specs.".to_string(),
                "Dispatch bounded worker slice.".to_string(),
                "Link PR and run pr_status gate before merge.".to_string(),
            ],
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: Vec::new(),
            next_action: Some("Call tachi_task(action='briefing', flow_id=...) and dispatch the next bounded slice.".to_string()),
            newest_user_instruction: None,
        },
    )
    .await?;
    Ok(())
}

fn issue_to_json(issue: &IssueSnapshot) -> Value {
    json!({
        "repo": issue.repo,
        "number": issue.number,
        "title": issue.title,
        "state": issue.state,
        "url": issue.url,
        "doc_paths": issue.doc_paths,
        "spec_paths": issue.spec_paths,
    })
}

fn pr_to_json(pr: &PrSnapshot) -> Value {
    json!({
        "repo": pr.repo,
        "number": pr.number,
        "title": pr.title,
        "state": pr.state,
        "url": pr.url,
        "head_ref": pr.head_ref,
        "base_ref": pr.base_ref,
        "review_decision": pr.review_decision,
        "mergeable": pr.mergeable,
    })
}

fn write_intake_instruction(
    run_dir: &Path,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
) -> Result<(), String> {
    let mut body = String::new();
    body.push_str(&format!("# Tachi Issue Intake - {flow_id}\n\n"));
    body.push_str("## Objective\n\n");
    body.push_str(objective.trim());
    body.push_str("\n\n## GitHub Issue\n\n");
    body.push_str(&format!("- repo: `{}`\n", issue.repo));
    body.push_str(&format!("- issue: `{}#{}`\n", issue.repo, issue.number));
    body.push_str(&format!("- url: {}\n", issue.url));
    if let Some(state) = issue.state.as_deref() {
        body.push_str(&format!("- state: `{state}`\n"));
    }
    body.push_str("\n## Canonical Docs / Specs\n\n");
    if issue.doc_paths.is_empty() && issue.spec_paths.is_empty() {
        body.push_str("- No docs/spec refs discovered from the issue. Create or attach one before treating memory as feature truth.\n");
    } else {
        for path in &issue.spec_paths {
            body.push_str(&format!("- spec: `{path}`\n"));
        }
        for path in &issue.doc_paths {
            body.push_str(&format!("- doc: `{path}`\n"));
        }
    }
    body.push_str("\n## Next Lifecycle Actions\n\n");
    body.push_str("- `tachi_task(action='briefing', flow_id=...)`\n");
    body.push_str("- `tachi_task(action='recommend', task=..., doc_paths=[...])`\n");
    body.push_str("- `tachi_task(action='dispatch', flow_id=..., issue_ref=...)`\n");
    body.push_str("- `tachi_task(action='link_pr', flow_id=..., pr_ref=...)`\n");
    body.push_str("- `tachi_task(action='pr_status', flow_id=..., pr_ref=...)`\n");
    std::fs::write(run_dir.join("instruction.md"), body)
        .map_err(|e| format!("write intake instruction.md: {e}"))
}

fn merge_flow_status(run_dir: &Path, patch: Value) -> Result<Value, String> {
    let mut status = read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }
    deep_merge(&mut status, patch);
    if status.get("created_at").is_none() {
        status["created_at"] = json!(Utc::now().to_rfc3339());
    }
    write_json_atomic(&run_dir.join("status.json"), &status)?;
    Ok(status)
}

fn read_json_file(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<Value>(&raw)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|e| format!("serialize {}: {e}", path.display()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serialized).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

fn deep_merge(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(&k);
                } else if let Some(existing) = t.get_mut(&k) {
                    deep_merge(existing, v);
                } else {
                    t.insert(k, v);
                }
            }
        }
        (slot, replacement) => *slot = replacement,
    }
}

fn string_array_field(value: &Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn extract_markdown_paths(text: &str) -> Vec<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ')' | '(' | '[' | ']'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | ';')))
        .filter(|token| {
            token.ends_with(".md") && (token.starts_with("docs/") || token.contains("/docs/"))
        })
        .map(str::to_string)
        .collect()
}

fn dedupe_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn new_task_flow_id(stage: &str, title: &str) -> String {
    let slug = title
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug.chars().take(40).collect::<String>();
    let slug = if slug.is_empty() {
        "flow".to_string()
    } else {
        slug
    };
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!(
        "flow_{}_{}_{}_{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        stage,
        slug,
        suffix
    )
}
